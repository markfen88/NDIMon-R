//! In-memory sessions and failure-only login throttling.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::auth::random_hex;
use axum::http::{HeaderMap, header};

pub const COOKIE: &str = "ndimon_session";

#[derive(Debug, Clone)]
pub struct Session {
    /// Public event attribution, independent of the bearer credential.
    pub id: String,
    pub csrf: String,
    pub credential: [u8; 32],
    last_used: Instant,
}

#[derive(Debug)]
pub struct Sessions {
    entries: Mutex<HashMap<String, Session>>,
    idle: Duration,
}

impl Default for Sessions {
    fn default() -> Self {
        Self::new(Duration::from_secs(8 * 3600))
    }
}

impl Sessions {
    pub fn new(idle: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            idle,
        }
    }

    pub fn create(&self, credential: [u8; 32], now: Instant) -> (String, Session) {
        let token = random_hex(32);
        let session = Session {
            id: random_hex(16),
            csrf: random_hex(32),
            credential,
            last_used: now,
        };
        let mut entries = self.entries.lock().expect("session lock");
        entries.retain(|_, s| now.saturating_duration_since(s.last_used) < self.idle);
        if entries.len() >= 64 {
            let oldest = entries
                .iter()
                .min_by_key(|(_, s)| s.last_used)
                .map(|(t, _)| t.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        entries.insert(token.clone(), session.clone());
        (token, session)
    }

    pub fn get(&self, token: &str, credential: [u8; 32], now: Instant) -> Option<Session> {
        let mut entries = self.entries.lock().ok()?;
        let session = entries.get_mut(token)?;
        if session.credential != credential
            || now.saturating_duration_since(session.last_used) >= self.idle
        {
            entries.remove(token);
            return None;
        }
        session.last_used = now;
        Some(session.clone())
    }

    pub fn remove(&self, token: &str) {
        self.entries.lock().expect("session lock").remove(token);
    }

    pub fn clear(&self) {
        self.entries.lock().expect("session lock").clear();
    }
}

#[derive(Debug, Clone)]
pub struct Authenticated {
    pub token: String,
    pub session: Session,
    pub bearer: bool,
}

pub fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .find(|(key, _)| *key == COOKIE)
        .map(|(_, value)| value.trim())
}

pub fn cookie(token: &str, secure: bool) -> String {
    format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/{}",
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_cookie(secure: bool) -> String {
    format!("{}; Max-Age=0", cookie("", secure))
}

#[derive(Debug)]
struct Failures {
    per_address: HashMap<IpAddr, VecDeque<Instant>>,
    all: VecDeque<Instant>,
}

#[derive(Debug)]
pub struct LoginLimiter {
    failures: Mutex<Failures>,
    window: Duration,
    per_address: usize,
    total: usize,
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), 5, 50)
    }
}

impl LoginLimiter {
    pub fn new(window: Duration, per_address: usize, total: usize) -> Self {
        Self {
            failures: Mutex::new(Failures {
                per_address: HashMap::new(),
                all: VecDeque::new(),
            }),
            window,
            per_address,
            total,
        }
    }

    fn prune(&self, failures: &mut Failures, now: Instant) {
        let recent = |at: &Instant| now.saturating_duration_since(*at) < self.window;
        failures.all.retain(recent);
        failures.per_address.retain(|_, times| {
            times.retain(recent);
            !times.is_empty()
        });
    }

    /// Only this source's failures can refuse a verification attempt. Starting
    /// at the fifth failure, wait 1/2/4/8/16/30 seconds since its last failure.
    pub fn retry_after(&self, address: IpAddr, now: Instant) -> Option<Duration> {
        let mut failures = self.failures.lock().expect("limiter lock");
        self.prune(&mut failures, now);
        let times = failures.per_address.get(&source(address))?;
        if times.len() < self.per_address {
            return None;
        }
        let seconds = (1u64 << times.len().saturating_sub(self.per_address).min(5)).min(30);
        Duration::from_secs(seconds)
            .checked_sub(now.saturating_duration_since(*times.back()?))
            .filter(|delay| !delay.is_zero())
    }

    /// Distributed failures add bounded latency, never a global refusal. Sleep
    /// outside the authentication gate so a delayed client holds no shared lock.
    pub fn soft_delay(&self, now: Instant) -> Duration {
        let mut failures = self.failures.lock().expect("limiter lock");
        self.prune(&mut failures, now);
        if failures.all.len() < self.total {
            return Duration::ZERO;
        }
        Duration::from_millis(250u64 << failures.all.len().saturating_sub(self.total).min(3))
    }

    pub fn succeeded(&self, address: IpAddr) {
        self.failures
            .lock()
            .expect("limiter lock")
            .per_address
            .remove(&source(address));
    }

    pub fn failed(&self, address: IpAddr, now: Instant) {
        let mut failures = self.failures.lock().expect("limiter lock");
        self.prune(&mut failures, now);
        let address = source(address);
        if failures.per_address.len() >= 4096 && !failures.per_address.contains_key(&address) {
            let oldest = failures
                .per_address
                .iter()
                .min_by_key(|(_, times)| times.back().copied())
                .map(|(ip, _)| *ip);
            if let Some(oldest) = oldest {
                failures.per_address.remove(&oldest);
            }
        }
        let times = failures.per_address.entry(address).or_default();
        if times.len() >= self.per_address.saturating_add(5) {
            times.pop_front();
        }
        times.push_back(now);
        if failures.all.len() >= self.total.saturating_add(3) {
            failures.all.pop_front();
        }
        failures.all.push_back(now);
    }
}

fn source(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return IpAddr::V4(ip);
            }
            let mut bytes = ip.octets();
            bytes[8..].fill(0);
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        ip => ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_timeout_and_credential_replacement() {
        let sessions = Sessions::new(Duration::from_secs(10));
        let now = Instant::now();
        let (token, s) = sessions.create([0; 32], now);
        assert_eq!(token.len(), 64);
        assert_eq!(s.csrf.len(), 64);
        assert!(
            sessions
                .get(&token, [0; 32], now + Duration::from_secs(9))
                .is_some()
        );
        assert!(
            sessions
                .get(&token, [0; 32], now + Duration::from_secs(18))
                .is_some()
        );
        assert!(
            sessions
                .get(&token, [0; 32], now + Duration::from_secs(28))
                .is_none()
        );
        let (token, _) = sessions.create([0; 32], now);
        assert!(sessions.get(&token, [1; 32], now).is_none());
    }

    #[test]
    fn source_backoff_is_exponential_bounded_and_expires() {
        let limiter = LoginLimiter::new(Duration::from_secs(60), 2, 3);
        let now = Instant::now();
        let a = [192, 0, 2, 1].into();
        let b = [192, 0, 2, 2].into();
        for _ in 0..100 {
            assert_eq!(limiter.retry_after(a, now), None);
        }
        limiter.failed(a, now);
        limiter.failed(a, now);
        assert_eq!(limiter.retry_after(a, now), Some(Duration::from_secs(1)));
        assert_eq!(limiter.retry_after(b, now), None);
        for seconds in [2, 4, 8, 16, 30, 30] {
            limiter.failed(a, now);
            assert_eq!(
                limiter.retry_after(a, now),
                Some(Duration::from_secs(seconds))
            );
        }
        assert_eq!(limiter.retry_after(a, now + Duration::from_secs(30)), None);
        assert_eq!(limiter.retry_after(a, now + Duration::from_secs(60)), None);
        limiter.failed(b, now);
        limiter.failed(a, now);
        limiter.failed(a, now);
        assert_eq!(limiter.retry_after(a, now), Some(Duration::from_secs(1)));
        limiter.succeeded(a);
        assert_eq!(limiter.retry_after(a, now), None);
    }

    #[test]
    fn global_pressure_only_adds_a_bounded_soft_delay() {
        let limiter = LoginLimiter::new(Duration::from_secs(60), 5, 50);
        let now = Instant::now();
        for n in 1..=100 {
            limiter.failed(IpAddr::from([192, 0, 2, n]), now);
        }
        let clean = "198.51.100.1".parse().unwrap();
        assert_eq!(limiter.retry_after(clean, now), None);
        assert_eq!(limiter.soft_delay(now), Duration::from_secs(2));
        assert_eq!(
            limiter.soft_delay(now + Duration::from_secs(60)),
            Duration::ZERO
        );
    }

    #[test]
    fn ipv6_sources_share_a_64_bucket_and_mapped_ipv4_shares_its_ipv4_bucket() {
        let limiter = LoginLimiter::new(Duration::from_secs(60), 2, 50);
        let now = Instant::now();
        for ip in [
            "2001:db8:1:2::1",
            "2001:db8:1:2:ffff::2",
            "192.0.2.1",
            "::ffff:192.0.2.1",
        ] {
            limiter.failed(ip.parse().unwrap(), now);
        }
        for ip in ["2001:db8:1:2::3", "192.0.2.1", "::ffff:192.0.2.1"] {
            assert_eq!(
                limiter.retry_after(ip.parse().unwrap(), now),
                Some(Duration::from_secs(1)),
                "{ip}"
            );
        }
        assert_eq!(
            limiter.retry_after("2001:db8:1:3::1".parse().unwrap(), now),
            None
        );
    }

    #[test]
    fn cookies_have_the_required_flags() {
        assert_eq!(
            cookie("abc", false),
            "ndimon_session=abc; HttpOnly; SameSite=Strict; Path=/"
        );
        assert!(cookie("abc", true).ends_with("; Secure"));
        assert!(clear_cookie(true).contains("Max-Age=0"));
    }
}
