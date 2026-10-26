//! One bounded rolling budget shared by Pending/Keep/Revert, keyed by socket IP.
use std::{
    collections::{BTreeMap, VecDeque},
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};
#[derive(Debug, Default)]
pub struct Limiter {
    peers: Mutex<BTreeMap<IpAddr, VecDeque<Instant>>>,
    pub gate: tokio::sync::Mutex<()>,
}
fn normalized(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        ip => ip,
    }
}
impl Limiter {
    pub fn limited(&self, peer: IpAddr, now: Instant) -> bool {
        let mut peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
        peers.retain(|_, hits| {
            hits.retain(|t| now.saturating_duration_since(*t) < Duration::from_secs(60));
            !hits.is_empty()
        });
        peers
            .get(&normalized(peer))
            .is_some_and(|hits| hits.len() >= 5)
    }
    pub fn record(&self, peer: IpAddr, now: Instant) {
        let peer = normalized(peer);
        let mut peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
        if peers.len() >= 4096 && !peers.contains_key(&peer) {
            if let Some(oldest) = peers
                .iter()
                .min_by_key(|(_, hits)| hits.back().copied())
                .map(|(ip, _)| *ip)
            {
                peers.remove(&oldest);
            }
        }
        let hits = peers.entry(peer).or_default();
        if hits.len() < 5 {
            hits.push_back(now);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mapped_peers_share_budget_and_hits_expire_at_exactly_one_minute() {
        let limiter = Limiter::default();
        let now = Instant::now();
        let mapped = "::ffff:192.0.2.5".parse().unwrap();
        let plain = "192.0.2.5".parse().unwrap();
        for _ in 0..5 {
            limiter.record(mapped, now);
        }
        assert!(limiter.limited(plain, now + Duration::from_millis(59999)));
        assert!(!limiter.limited(mapped, now + Duration::from_secs(60)));
    }
}
