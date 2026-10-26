//! Who is calling, and what that caller may ask for.
//!
//! The identity is the listening socket that accepted the connection: each
//! caller has its own socket, whose group is granted only through that unit's
//! `SupplementaryGroups=`. This works on every kernel. Where the kernel can
//! also vouch for the peer (Linux 6.5+, `SO_PEERPIDFD`), the peer's cgroup must
//! name the same unit; older vendor kernels rely on the socket alone.
use ndimon_privd_proto::{Error, Request};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{Mode, OFlags, open, openat},
};
use std::{
    fs::File,
    io::Read,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};
use tokio::net::UnixStream;

/// A privileged caller. A second caller needs its own socket, group and an
/// explicit request table in `authorize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Control,
}
impl Unit {
    /// Descriptor name systemd gives the caller's socket (`FileDescriptorName=`).
    pub fn name(self) -> &'static str {
        match self {
            Unit::Control => "control",
        }
    }
}

/// Environment override for the second check. `off` skips it, as on a kernel
/// without `SO_PEERPIDFD`; anything else probes the kernel.
pub const PEER_PIDFD_ENV: &str = "NDIMON_PRIVD_PEER_PIDFD";

/// Whether the peer's cgroup is checked against the socket identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerCheck {
    /// Pin the peer by pidfd and require its unit to equal the socket's.
    Pidfd,
    /// Kernels before 6.5 (for example the Rockchip BSP 5.10/6.1 trees): the
    /// socket identity alone decides. There is no PID-based substitute, which
    /// could pin a recycled PID.
    SocketOnly,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub check: PeerCheck,
    pub reason: String,
}
impl PeerCheck {
    pub fn detect() -> Detection {
        Self::detect_with(std::env::var(PEER_PIDFD_ENV).ok().as_deref(), Self::probe)
    }
    /// Ask the running kernel for a socket's peer pidfd, ignoring any override.
    pub fn probe() -> std::io::Result<()> {
        peer_pidfd(&std::os::unix::net::UnixStream::pair()?.0).map(drop)
    }
    /// The probe is injected so the unsupported-kernel path is testable on any
    /// kernel. Any probe failure degrades to the socket identity: the kernel
    /// version never makes the daemon refuse to serve.
    pub fn detect_with(
        setting: Option<&str>,
        probe: impl FnOnce() -> std::io::Result<()>,
    ) -> Detection {
        let note = match setting {
            Some("off") => {
                return Detection {
                    check: PeerCheck::SocketOnly,
                    reason: format!("{PEER_PIDFD_ENV}=off"),
                };
            }
            Some("auto") | None => "",
            Some(_) => "unrecognized override ignored; ",
        };
        match probe() {
            Ok(()) => Detection {
                check: PeerCheck::Pidfd,
                reason: format!("{note}SO_PEERPIDFD available"),
            },
            Err(e) if e.raw_os_error() == Some(nix::libc::ENOPROTOOPT) => Detection {
                check: PeerCheck::SocketOnly,
                reason: format!("{note}kernel has no SO_PEERPIDFD (Linux before 6.5)"),
            },
            Err(e) => Detection {
                check: PeerCheck::SocketOnly,
                reason: format!("{note}SO_PEERPIDFD probe failed: {e}"),
            },
        }
    }
}

/// Only direct system units count. A delegated child, user unit, suffix or
/// escaped lookalike must not inherit a parent's authority. Support unified
/// cgroup v2 and the systemd controller on v1; reject conflicting records.
pub fn calling_unit(cgroups: &str) -> Result<Unit, Error> {
    let mut found = None;
    for line in cgroups.lines() {
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next().ok_or(Error::PermissionDenied)?;
        let controllers = fields.next().ok_or(Error::PermissionDenied)?;
        let path = fields.next().ok_or(Error::PermissionDenied)?;
        if !(hierarchy == "0" && controllers.is_empty()
            || controllers.split(',').any(|c| c == "name=systemd"))
        {
            continue;
        }
        let unit = match path {
            "/system.slice/ndimon-control.service" | "/ndimonndi.slice/ndimon-control.service" => {
                Unit::Control
            }
            _ => return Err(Error::PermissionDenied),
        };
        if found.is_some_and(|previous| previous != unit) {
            return Err(Error::PermissionDenied);
        }
        found = Some(unit);
    }
    found.ok_or(Error::PermissionDenied)
}

/// The control plane is the only caller and holds the complete request set.
pub fn authorize(unit: Unit, _request: &Request) -> Result<(), Error> {
    match unit {
        Unit::Control => Ok(()),
    }
}

/// Linux 6.5+ gives us the *socket's* peer pidfd, including after peer exit.
/// pidfd_open(peer_pid) alone could pin a recycled PID, so older kernels get no
/// pidfd at all (`ENOPROTOOPT`) rather than a PID-based stand-in.
#[allow(unsafe_code)]
fn peer_pidfd(socket: &impl AsRawFd) -> std::io::Result<OwnedFd> {
    let mut fd: nix::libc::c_int = -1;
    let mut len = std::mem::size_of_val(&fd) as nix::libc::socklen_t;
    // SAFETY: fd and len are valid output buffers of exactly the declared size;
    // getsockopt borrows the live socket and returns a newly owned descriptor.
    let result = unsafe {
        nix::libc::getsockopt(
            socket.as_raw_fd(),
            nix::libc::SOL_SOCKET,
            nix::libc::SO_PEERPIDFD,
            (&mut fd as *mut nix::libc::c_int).cast(),
            &mut len,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if fd < 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    // SAFETY: successful SO_PEERPIDFD transfers one new fd to this caller.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[derive(Debug)]
pub struct Peer {
    pidfd: OwnedFd,
    proc: File,
}
impl Peer {
    /// `None` when the socket identity is the only check. Where the check is
    /// required, every failure to pin the peer refuses the caller.
    pub fn capture(stream: &UnixStream, check: PeerCheck) -> Result<Option<Self>, Error> {
        if check == PeerCheck::SocketOnly {
            return Ok(None);
        }
        let cred = stream.peer_cred().map_err(|_| Error::PermissionDenied)?;
        let pid = cred
            .pid()
            .filter(|pid| *pid > 0)
            .ok_or(Error::PermissionDenied)?;
        let pidfd = peer_pidfd(stream).map_err(|_| Error::PermissionDenied)?;
        let proc = File::from(
            open(
                format!("/proc/{pid}"),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Error::PermissionDenied)?,
        );
        let peer = Self { pidfd, proc };
        peer.alive()?;
        Ok(Some(peer))
    }
    fn alive(&self) -> Result<(), Error> {
        let mut fds = [PollFd::new(&self.pidfd, PollFlags::IN)];
        match poll(&mut fds, Some(&Timespec::default())) {
            Ok(0) => Ok(()),
            _ => Err(Error::PermissionDenied),
        }
    }
    /// Read through the pinned proc directory and recheck the original peer's
    /// liveness after lookup. Exited/recycled peers never acquire authority.
    pub fn unit(&self) -> Result<Unit, Error> {
        self.alive()?;
        let file = File::from(
            openat(
                &self.proc,
                "cgroup",
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Error::PermissionDenied)?,
        );
        let mut text = String::new();
        file.take(65537)
            .read_to_string(&mut text)
            .map_err(|_| Error::PermissionDenied)?;
        if text.len() > 65536 {
            return Err(Error::PermissionDenied);
        }
        let unit = calling_unit(&text)?;
        self.alive()?;
        Ok(unit)
    }
}

/// The accepting socket decides who is calling. When the kernel also vouches
/// for the peer, its unit must be the same; a mismatch is refused, never
/// resolved in the caller's favour.
pub fn confirm(socket: Unit, peer: Option<&Peer>) -> Result<(), Error> {
    match peer {
        None => Ok(()),
        Some(peer) => agree(socket, peer.unit()?),
    }
}
fn agree(socket: Unit, cgroup: Unit) -> Result<(), Error> {
    if socket == cgroup {
        Ok(())
    } else {
        Err(Error::PermissionDenied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cgroups_are_exact_and_fail_closed() {
        for (text, expected) in [
            (
                "0::/system.slice/ndimon-control.service\n",
                Some(Unit::Control),
            ),
            (
                "0::/ndimonndi.slice/ndimon-control.service\n",
                Some(Unit::Control),
            ),
            (
                "8:cpu:/irrelevant\n1:name=systemd:/system.slice/ndimon-control.service\n",
                Some(Unit::Control),
            ),
            // The retired API unit is no caller.
            ("0::/system.slice/ndimon-api.service\n", None),
            ("0::/system.slice/ndimon-finder.service\n", None),
            ("0::/system.slice/ndimon-control.service/child\n", None),
            ("0::/user.slice/system.slice/ndimon-control.service\n", None),
            ("0::/system.slice/NDIMON-control.service\n", None),
            ("0::/system.slice/ndimon-control.service-extra\n", None),
            (
                "0::/system.slice/ndimon-control.service\n1:name=systemd:/system.slice/ndimon-api.service\n",
                None,
            ),
            ("1:cpu:/system.slice/ndimon-control.service\n", None),
            ("garbage", None),
            ("", None),
        ] {
            assert_eq!(calling_unit(text).ok(), expected, "{text}");
        }
    }
    #[test]
    fn the_control_plane_holds_the_complete_request_set() {
        for value in [
            json!("stop_core"),
            json!("reboot"),
            json!({"plugin_exec":{"name":"example","command":"enable"}}),
            json!({"install_update":{"version":"1.3.1-1","force":true}}),
            json!({"write_legacy_config":{"file":"ndimon-auth.json","json":{}}}),
            json!({"plugin_install":{"name":"example","archive":{"download":{"id":"example.tar.gz"}},"expected_version":null,"force":false}}),
            json!({"render_artifacts":{"revision":1}}),
        ] {
            let request: Request = serde_json::from_value(value).unwrap();
            assert!(authorize(Unit::Control, &request).is_ok());
        }
        // The Node API's request is gone from the protocol altogether.
        assert!(serde_json::from_value::<Request>(json!("restart_api")).is_err());
    }
    #[test]
    fn socket_names_match_the_systemd_descriptor_names() {
        assert_eq!(Unit::Control.name(), "control");
    }
    #[test]
    fn socket_and_cgroup_identity_must_agree() {
        assert!(agree(Unit::Control, Unit::Control).is_ok());
        // Without a kernel-vouched peer the socket alone stands.
        assert!(confirm(Unit::Control, None).is_ok());
    }
    #[test]
    fn older_kernels_degrade_to_the_socket_identity() {
        let unsupported = || Err(std::io::Error::from_raw_os_error(nix::libc::ENOPROTOOPT));
        let detect = PeerCheck::detect_with(None, unsupported);
        assert_eq!(detect.check, PeerCheck::SocketOnly);
        assert!(detect.reason.contains("before 6.5"), "{}", detect.reason);
        // Any other probe failure also keeps the daemon serving, with the cause.
        let detect = PeerCheck::detect_with(Some("auto"), || {
            Err(std::io::Error::from_raw_os_error(nix::libc::EPERM))
        });
        assert_eq!(detect.check, PeerCheck::SocketOnly);
        assert!(detect.reason.contains("probe failed"), "{}", detect.reason);
        let detect = PeerCheck::detect_with(None, || Ok(()));
        assert_eq!(detect.check, PeerCheck::Pidfd);
        // The override never probes, and unknown values do not weaken the check.
        let detect = PeerCheck::detect_with(Some("off"), || panic!("override must not probe"));
        assert_eq!(detect.check, PeerCheck::SocketOnly);
        assert!(detect.reason.contains(PEER_PIDFD_ENV));
        let detect = PeerCheck::detect_with(Some("disable"), || Ok(()));
        assert_eq!(detect.check, PeerCheck::Pidfd);
        assert!(detect.reason.contains("ignored"), "{}", detect.reason);
    }
    #[tokio::test]
    async fn socket_only_mode_pins_no_peer_and_never_refuses_by_kernel() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(Peer::capture(&a, PeerCheck::SocketOnly).unwrap().is_none());
    }
    #[tokio::test]
    async fn ordinary_process_cannot_claim_a_service_unit() {
        let (a, _b) = UnixStream::pair().unwrap();
        // Independent of the override: ask the kernel directly.
        if peer_pidfd(&a).is_ok() {
            let peer = Peer::capture(&a, PeerCheck::Pidfd).unwrap().unwrap();
            assert_eq!(peer.unit(), Err(Error::PermissionDenied));
            assert_eq!(
                confirm(Unit::Control, Some(&peer)),
                Err(Error::PermissionDenied)
            );
        } else {
            // Explicitly requiring a check the kernel cannot give still refuses.
            assert_eq!(
                Peer::capture(&a, PeerCheck::Pidfd).unwrap_err(),
                Error::PermissionDenied
            );
        }
    }
}
