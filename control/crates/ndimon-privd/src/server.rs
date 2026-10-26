//! One socket-activated listener per privileged caller, and the connection loop.
//!
//! systemd binds each socket with its own group (`SocketGroup=`), and hands the
//! descriptors over together. A connection's caller is the listener that
//! accepted it; the request table that applies is that caller's.
use crate::{
    Host,
    policy::{self, PeerCheck, Unit},
};
use ndimon_privd_proto::{CONTROL_SOCKET, Envelope, Error, Reply, ReplyResult, VERSION, codec};
use std::{
    error::Error as StdError,
    future::poll_fn,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
    task::Poll,
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
};

const UNITS: [Unit; 1] = [Unit::Control];

/// Where each caller's socket is bound. Tests bind elsewhere.
#[derive(Debug, Clone)]
pub struct Layout {
    pub control: PathBuf,
}
impl Layout {
    pub fn production() -> Self {
        Self {
            control: CONTROL_SOCKET.into(),
        }
    }
    pub fn path(&self, unit: Unit) -> &Path {
        match unit {
            Unit::Control => &self.control,
        }
    }
    fn unit_at(&self, path: &Path) -> Option<Unit> {
        UNITS.into_iter().find(|unit| self.path(*unit) == path)
    }
}

/// Ownership facts of one socket and its directory, for `judge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub unit: Unit,
    pub uid: u32,
    pub mode: u32,
    pub parent_uid: u32,
    pub parent_mode: u32,
}
/// Refuse a layout with a foreign owner, any world access, or a directory
/// others can modify. A second caller's socket would also need its own group,
/// or its members could reach the other caller's table; that check belongs
/// with the second caller.
pub fn judge(owner: u32, facts: &[Facts]) -> Result<(), String> {
    for f in facts {
        let name = f.unit.name();
        if f.uid != owner || f.mode & 0o007 != 0 {
            return Err(format!(
                "{name} socket must belong to the daemon with no world access"
            ));
        }
        if f.parent_uid != owner || f.parent_mode & 0o022 != 0 {
            return Err(format!(
                "{name} socket directory must belong to the daemon and not be writable by others"
            ));
        }
    }
    Ok(())
}
fn facts(layout: &Layout, unit: Unit) -> Result<Facts, String> {
    let path = layout.path(unit);
    let describe = |what: &Path, e: std::io::Error| format!("{}: {e}", what.display());
    let info = std::fs::symlink_metadata(path).map_err(|e| describe(path, e))?;
    if !info.file_type().is_socket() {
        return Err(format!("{} is not a socket", path.display()));
    }
    let parent = path.parent().ok_or("socket has no directory")?;
    let dir = std::fs::symlink_metadata(parent).map_err(|e| describe(parent, e))?;
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", parent.display()));
    }
    Ok(Facts {
        unit,
        uid: info.uid(),
        mode: info.mode(),
        parent_uid: dir.uid(),
        parent_mode: dir.mode(),
    })
}

#[derive(Debug)]
pub struct Listeners(Vec<(Unit, UnixListener)>);
impl Listeners {
    /// Take the descriptors systemd passed. They are identified by the path
    /// each is bound to; the descriptor name, when systemd supplies one, must
    /// agree. Order is not relied on.
    pub fn inherit(layout: &Layout) -> Result<Self, Box<dyn StdError>> {
        let mut fds = listenfd::ListenFd::from_env();
        let names = std::env::var("LISTEN_FDNAMES").unwrap_or_default();
        let mut names = names.split(':');
        let mut found = Vec::new();
        for index in 0..fds.len() {
            let name = names.next().map(str::to_owned);
            let listener = fds
                .take_unix_listener(index)?
                .ok_or("inherited descriptor already taken")?;
            found.push((name, listener));
        }
        Self::identify(layout, found)
    }
    pub fn identify(
        layout: &Layout,
        found: Vec<(Option<String>, std::os::unix::net::UnixListener)>,
    ) -> Result<Self, Box<dyn StdError>> {
        let mut listeners: Vec<(Unit, UnixListener)> = Vec::new();
        for (name, listener) in found {
            let address = listener.local_addr()?;
            let path = address
                .as_pathname()
                .ok_or("inherited socket is not bound to a filesystem path")?;
            let unit = layout
                .unit_at(path)
                .ok_or_else(|| format!("unexpected privd socket {}", path.display()))?;
            // systemd names an unnamed descriptor "unknown".
            let named = name.filter(|n| !n.is_empty() && n != "unknown");
            if named.as_deref().is_some_and(|n| n != unit.name()) {
                return Err(format!(
                    "descriptor {} is bound to the {} socket",
                    named.unwrap_or_default(),
                    unit.name()
                )
                .into());
            }
            if listeners.iter().any(|(known, _)| *known == unit) {
                return Err(format!("duplicate {} socket", unit.name()).into());
            }
            listener.set_nonblocking(true)?;
            listeners.push((unit, UnixListener::from_std(listener)?));
        }
        if listeners.is_empty() {
            return Err("ndimon-privd requires socket activation".into());
        }
        Ok(Self(listeners))
    }
    pub fn callers(&self) -> Vec<&'static str> {
        self.0.iter().map(|(unit, _)| unit.name()).collect()
    }
    /// Check the sockets as they exist on disk; see `judge`.
    pub fn check_layout(&self, layout: &Layout) -> Result<(), String> {
        let facts = self
            .0
            .iter()
            .map(|(unit, _)| facts(layout, *unit))
            .collect::<Result<Vec<_>, _>>()?;
        judge(nix::unistd::geteuid().as_raw(), &facts)
    }
    /// Cancel-safe: a connection is returned from the poll that accepted it.
    /// `turn` rotates which listener is polled first, so neither starves.
    async fn accept(&self, turn: usize) -> std::io::Result<(Unit, UnixStream)> {
        poll_fn(|cx| {
            for offset in 0..self.0.len() {
                let (unit, listener) = &self.0[(turn + offset) % self.0.len()];
                if let Poll::Ready(accepted) = listener.poll_accept(cx) {
                    return Poll::Ready(accepted.map(|(stream, _)| (*unit, stream)));
                }
            }
            Poll::Pending
        })
        .await
    }
}

/// Serve until `idle` passes with no connection and no request in flight.
pub async fn serve(
    listeners: Listeners,
    host: Arc<Host>,
    check: PeerCheck,
    idle: Duration,
) -> Result<(), Box<dyn StdError>> {
    let permits = Arc::new(Semaphore::new(32));
    let mut jobs = JoinSet::new();
    let mut turn = 0usize;
    loop {
        tokio::select! {
            accepted = tokio::time::timeout(idle, listeners.accept(turn)) => match accepted {
                Err(_) if jobs.is_empty() => break,
                Err(_) => (),
                Ok(Err(e)) => return Err(e.into()),
                Ok(Ok((unit, stream))) => {
                    turn = turn.wrapping_add(1);
                    let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                    let host = host.clone();
                    jobs.spawn(async move {
                        let _permit = permit;
                        if let Err(e) = connection(stream, unit, check, host).await {
                            tracing::warn!(caller = unit.name(), error = %e, "privd connection rejected");
                        }
                    });
                },
            },
            finished = jobs.join_next(), if !jobs.is_empty() => {
                if let Some(Err(e)) = finished {
                    tracing::error!(error = %e, "connection worker failed");
                }
            },
        }
    }
    tracing::info!("privd idle timeout");
    Ok(())
}

pub async fn connection(
    mut stream: UnixStream,
    unit: Unit,
    check: PeerCheck,
    host: Arc<Host>,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let caller = unit.name();
    let uid = stream.peer_cred()?.uid();
    // DAC on the caller's socket admitted this process. Where the kernel can
    // pin its identity, do so now; the table below is the caller's own.
    let peer = policy::Peer::capture(&stream, check);
    let raw: serde_json::Value =
        match tokio::time::timeout(Duration::from_secs(10), codec::read_async(&mut stream)).await {
            Ok(Ok(value)) => value,
            _ => {
                tracing::warn!(
                    caller,
                    peer_uid = uid,
                    request_kind = "decode",
                    outcome = "invalid",
                    "malformed, oversized or timed-out frame"
                );
                return Err("malformed, oversized or timed-out frame".into());
            }
        };
    let envelope: Envelope = match serde_json::from_value(raw.clone()) {
        Ok(v) => v,
        Err(error) => {
            let id = raw
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            tracing::warn!(
                caller,
                peer_uid = uid,
                request_id = id,
                request_kind = "decode",
                outcome = "invalid",
                "privileged request"
            );
            let reply = Reply {
                v: VERSION,
                id,
                result: ReplyResult::Err(Error::invalid("req", error.to_string())),
            };
            tokio::time::timeout(
                Duration::from_secs(10),
                codec::write_async(&mut stream, &reply),
            )
            .await??;
            return Ok(());
        }
    };
    let kind = envelope.req.kind();
    let routine = envelope.req.is_routine_read();
    let id = envelope.id;
    let result = if envelope.v != VERSION {
        Err(Error::Unsupported {
            what: "protocol version".into(),
        })
    } else {
        tokio::task::spawn_blocking(move || {
            policy::confirm(unit, peer?.as_ref())?;
            policy::authorize(unit, &envelope.req)?;
            host.handle(envelope.req)
        })
        .await?
    };
    let outcome = match &result {
        Ok(_) => "ok",
        Err(Error::Invalid { .. }) => "invalid",
        Err(Error::PermissionDenied) => "permission_denied",
        Err(Error::Busy) => "busy",
        Err(Error::Failed { .. }) => "failed",
        Err(Error::Unsupported { .. }) => "unsupported",
        Err(Error::Internal { .. }) => "internal",
    };
    let job = result
        .as_ref()
        .ok()
        .and_then(|out| out.job.as_ref())
        .map(|job| job.as_str());
    if routine && outcome == "ok" {
        tracing::debug!(
            caller,
            peer_uid = uid,
            request_id = id,
            request_kind = kind,
            outcome,
            "privileged request"
        );
    } else {
        tracing::info!(caller,peer_uid=uid,request_id=id,request_kind=kind,outcome,job=?job,"privileged request");
    }
    let reply = Reply {
        v: VERSION,
        id,
        result: match result {
            Ok(out) => ReplyResult::Ok(out),
            Err(err) => ReplyResult::Err(err),
        },
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        codec::write_async(&mut stream, &reply),
    )
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener as StdListener;

    fn layout(directory: &Path) -> Layout {
        Layout {
            control: directory.join("control.sock"),
        }
    }
    fn good(unit: Unit) -> Facts {
        Facts {
            unit,
            uid: 0,
            mode: 0o140660,
            parent_uid: 0,
            parent_mode: 0o40755,
        }
    }
    #[test]
    fn production_paths_are_the_documented_ones() {
        let layout = Layout::production();
        assert_eq!(layout.control, Path::new("/run/ndimon-privd/control.sock"));
        assert_eq!(layout.unit_at(&layout.control), Some(Unit::Control));
        // The retired API socket and the original shared socket serve nobody.
        assert_eq!(
            layout.unit_at(Path::new("/run/ndimon-privd/api.sock")),
            None
        );
        assert_eq!(layout.unit_at(Path::new("/run/ndimon/privd.sock")), None);
    }
    #[test]
    fn layout_judgement_refuses_foreign_owners_world_access_and_open_directories() {
        let control = [good(Unit::Control)];
        assert_eq!(judge(0, &control), Ok(()));

        let mut world = control.clone();
        world[0].mode = 0o140666;
        assert!(judge(0, &world).unwrap_err().contains("control socket"));
        let mut other = control.clone();
        other[0].uid = 1000;
        assert!(judge(0, &other).unwrap_err().contains("control socket"));
        let mut open_dir = control.clone();
        open_dir[0].parent_mode = 0o40775;
        assert!(judge(0, &open_dir).unwrap_err().contains("directory"));
        let mut foreign_dir = control.clone();
        foreign_dir[0].parent_uid = 1000;
        assert!(judge(0, &foreign_dir).unwrap_err().contains("directory"));
    }
    #[tokio::test]
    async fn descriptors_are_identified_by_bound_path_and_checked_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let layout = layout(directory.path());
        let bind = |path: &Path| StdListener::bind(path).unwrap();

        // The name must agree with the path.
        let found = vec![(Some("control".to_owned()), bind(&layout.control))];
        let listeners = Listeners::identify(&layout, found).unwrap();
        assert_eq!(listeners.callers(), ["control"]);
        drop(listeners);
        std::fs::remove_file(&layout.control).unwrap();

        // Unnamed or "unknown" descriptors rely on the path.
        for name in [None, Some("unknown".to_owned())] {
            let found = vec![(name, bind(&layout.control))];
            assert_eq!(
                Listeners::identify(&layout, found).unwrap().callers(),
                ["control"]
            );
            std::fs::remove_file(&layout.control).unwrap();
        }

        // A name that contradicts the path is a unit misconfiguration.
        let found = vec![(Some("api".to_owned()), bind(&layout.control))];
        let error = Listeners::identify(&layout, found).err().unwrap();
        assert!(error.to_string().contains("bound to the control socket"));
        std::fs::remove_file(&layout.control).unwrap();

        // Two descriptors for one caller are a misconfiguration too.
        let first = bind(&layout.control);
        std::fs::remove_file(&layout.control).unwrap();
        let second = bind(&layout.control);
        let error = Listeners::identify(&layout, vec![(None, first), (None, second)])
            .err()
            .unwrap();
        assert!(error.to_string().contains("duplicate control socket"));
        std::fs::remove_file(&layout.control).unwrap();

        // Neither a stray socket (the retired API socket included) nor nothing
        // at all is served.
        for name in ["other.sock", "api.sock"] {
            let stray = directory.path().join(name);
            let error = Listeners::identify(&layout, vec![(None, bind(&stray))])
                .err()
                .unwrap();
            assert!(error.to_string().contains("unexpected privd socket"));
            std::fs::remove_file(&stray).unwrap();
        }
        let error = Listeners::identify(&layout, vec![]).err().unwrap();
        assert!(error.to_string().contains("socket activation"));
    }
    #[tokio::test]
    async fn layout_is_checked_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        // The default mode follows the umask; the check is about 0700 here.
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let layout = layout(directory.path());
        let found = vec![(None, StdListener::bind(&layout.control).unwrap())];
        let listeners = Listeners::identify(&layout, found).unwrap();
        let set = |mode| {
            std::fs::set_permissions(&layout.control, std::fs::Permissions::from_mode(mode))
                .unwrap()
        };
        set(0o660);
        listeners.check_layout(&layout).unwrap();
        set(0o666);
        assert!(listeners.check_layout(&layout).is_err());
        set(0o660);
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(listeners.check_layout(&layout).is_err());
    }
}
