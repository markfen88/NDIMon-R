//! What NDIMon-R remembers of the reloads it sent to tesserad, so that a replay
//! reloads the daemon only when it would pick something up: a configuration
//! file it reads differs from the one the last reload saw, or another daemon
//! process runs than the one that was reloaded.
use ndimon_render::PATHS;
use ndimon_tessera::Client;
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// The configuration files tesserad reads on a reload; ALSA files are read by
/// the clients, not by the daemon.
const CONFIG_DIR: &str = "/etc/tessera/config.d/";

/// The span, on this process's monotonic clock, in which the daemon process
/// started, from one reading of its `daemon.uptime_ms` metric (milliseconds
/// since it started). The daemon status carries no process identity, so the
/// start is the identity: two readings of one process overlap, a restarted
/// daemon starts later than the reading that preceded the restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Started {
    earliest: Instant,
    latest: Instant,
}
impl Started {
    /// `sent` and `received` bracket the exchange: the daemon measured its
    /// uptime somewhere between them and truncated it to whole milliseconds.
    pub fn from_uptime(sent: Instant, received: Instant, uptime: Duration) -> Option<Self> {
        Some(Self {
            earliest: sent.checked_sub(uptime + Duration::from_millis(1))?,
            latest: received.checked_sub(uptime)?,
        })
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        self.earliest <= other.latest && other.earliest <= self.latest
    }
}

/// The bytes of every configuration file, `None` for one that does not exist.
type Artifacts = Vec<(&'static str, Option<Vec<u8>>)>;

/// What a reload would act on, read just before it is sent: the configuration
/// files and the daemon process that reads them.
#[derive(Debug, Clone)]
pub struct Seen {
    /// `None` when a file could not be read: nothing is known about it, so it
    /// never equals an earlier reading.
    pub artifacts: Option<Artifacts>,
    /// `None` when the daemon did not report an uptime.
    pub daemon: Option<Started>,
}
impl Seen {
    pub fn same_as(&self, earlier: &Self) -> bool {
        let (Some(now), Some(then)) = (&self.artifacts, &earlier.artifacts) else {
            return false;
        };
        now == then
            && match (&self.daemon, &earlier.daemon) {
                (Some(now), Some(then)) => now.overlaps(then),
                // A daemon that reports no uptime cannot be told apart.
                (None, None) => true,
                _ => false,
            }
    }
}

/// The reload last sent, shared by every clone of the applier. Control starts
/// knowing nothing, so its first replay reloads once.
#[derive(Debug, Clone)]
pub struct Reloads {
    root: PathBuf,
    last: Arc<Mutex<Option<Seen>>>,
}
impl Default for Reloads {
    fn default() -> Self {
        Self::under("/")
    }
}
impl Reloads {
    /// Configuration files are read below `root`; tests use a temporary one.
    pub fn under(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            last: Arc::default(),
        }
    }
    fn lock(&self) -> MutexGuard<'_, Option<Seen>> {
        self.last.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// Reads the files and asks the daemon how long it has been running. Call
    /// it before the reload, so that a file changing meanwhile is seen as a
    /// change by the next replay. A daemon that does not answer is not an error
    /// here: the reload itself reports that.
    pub async fn look(&self, client: &Client) -> Seen {
        let sent = Instant::now();
        let uptime = client.metrics().await.ok().and_then(|metrics| {
            metrics
                .fields
                .get("daemon")?
                .get("uptime_ms")?
                .as_u64()
                .map(Duration::from_millis)
        });
        let received = Instant::now();
        Seen {
            artifacts: read(&self.root).await,
            daemon: uptime.and_then(|uptime| Started::from_uptime(sent, received, uptime)),
        }
    }
    /// Whether a reload would pick up anything since the last one sent.
    pub fn due(&self, seen: &Seen) -> bool {
        !self.lock().as_ref().is_some_and(|last| seen.same_as(last))
    }
    /// A reload that was read as `seen` succeeded.
    pub fn sent(&self, seen: Seen) {
        *self.lock() = Some(seen);
    }
    /// The daemon cannot be reached: nothing is known about what it runs.
    pub fn forget(&self) {
        *self.lock() = None;
    }
}

async fn read(root: &Path) -> Option<Artifacts> {
    let mut files = Vec::new();
    for spec in PATHS.iter().filter(|p| p.path.starts_with(CONFIG_DIR)) {
        let bytes = match tokio::fs::read(root.join(spec.path.trim_start_matches('/'))).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                tracing::warn!(path = spec.path, %error, "Tessera configuration unreadable; reloading");
                return None;
            }
        };
        files.push((spec.path_id, bytes));
    }
    Some(files)
}
