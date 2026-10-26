//! HW-1 H4: the clock role and priority a running Tessera daemon actually uses.
//!
//! A device whose Tessera plugin never set them has the daemon's own default
//! (`follower`, 128 in 0.1.0) in no file at all, so importing the files alone
//! made the role `auto` and the daemon became the PTP leader on the audio
//! network the moment the control plane pushed its configuration. The one-shot
//! import therefore asks the daemon first; the files only decide when it does
//! not answer (not running, too old to say).
use crate::persist::StateFile;
use ndimon_tessera::{Client, wire::Status};
use ndimon_types::ClockRole;
use std::{path::Path, time::Duration};

/// The control socket of `tesserad`.
pub const SOCKET: &str = "/run/tessera/control.sock";

/// `clock_settings` of the daemon's status: `{"role": ..., "priority": ...}`.
pub fn from_status(status: &Status) -> Option<(ClockRole, u8)> {
    let settings = status.extra.get("clock_settings")?;
    let role = serde_json::from_value(settings.get("role")?.clone()).ok()?;
    let priority = u8::try_from(settings.get("priority")?.as_u64()?).ok()?;
    Some((role, priority))
}
/// Ask the daemon. Blocking, for the one-shot import binary; any failure is
/// `None` and never an error, so a stopped daemon cannot stop an upgrade.
pub fn observe(socket: &Path) -> Option<(ClockRole, u8)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let status = runtime
        .block_on(Client::new(socket, Duration::from_secs(3)).status())
        .ok()?;
    from_status(&status)
}
/// Carry the observed role and priority into the imported document, exactly.
pub fn keep(state: &mut StateFile, observed: Option<(ClockRole, u8)>) -> Result<(), String> {
    let (Some((role, priority)), Some(tessera)) = (observed, state.document.tessera.as_mut())
    else {
        return Ok(());
    };
    tessera.clock.role = role;
    tessera.clock.priority = priority;
    state.check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import;
    use ndimon_render::LegacyFiles;
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };

    // The ROCK 5B's Tessera files before the upgrade (HW-1), with the device name
    // its workaround added: no `[clock]` section, no clock in the runtime state.
    fn board_files() -> LegacyFiles {
        LegacyFiles::from([
            (
                "/etc/tessera/config.d/60-ndimon.toml".into(),
                "[[device]]\nid = \"main\"\nname = \"NDIMON-53AB\"\nrx_channels = 2\ntx_channels = 2\nsample_rate = 48000\nencoding = 24\n\n[discovery]\nadvertise_devices = false\n".into(),
            ),
            (
                "/var/lib/tessera/state.toml".into(),
                "# managed by tesserad - do not edit while the daemon is running\nschema_version = 1\n\n[device.main]\nwire_device_id = \"c637c483fc9cbe0a\"\nlatency_us = 10000\nrx_channels = 2\ntx_channels = 2\n".into(),
            ),
        ])
    }
    fn status(settings: Value) -> Status {
        serde_json::from_value(
            json!({"version":"0.1.0","devices":[],"clocks":[],"clock_settings":settings}),
        )
        .unwrap()
    }

    #[test] // The daemon's own words, nothing else.
    fn clock_settings_are_read_from_the_status() {
        for (settings, expected) in [
            (
                json!({"role":"follower","priority":128,"locked":[]}),
                Some((ClockRole::Follower, 128)),
            ),
            (
                json!({"role":"leader","priority":0}),
                Some((ClockRole::Leader, 0)),
            ),
            (
                json!({"role":"auto","priority":255,"locked":["role"]}),
                Some((ClockRole::Auto, 255)),
            ),
            (json!({"role":"boss","priority":1}), None),
            (json!({"role":"follower","priority":256}), None),
            (json!({"role":"follower"}), None),
            (json!("follower"), None),
        ] {
            assert_eq!(
                from_status(&status(settings.clone())),
                expected,
                "{settings}"
            );
        }
        // An older daemon without the field says nothing.
        let silent: Status = serde_json::from_value(json!({"version":"0.0.9"})).unwrap();
        assert_eq!(from_status(&silent), None);
    }

    #[test] // HW-1 H4 with the board's own files: follower must stay follower.
    fn the_import_keeps_the_role_and_priority_the_daemon_runs() {
        let imported = import::import(board_files()).unwrap();
        let tessera = imported.document.tessera.as_ref().unwrap();
        // Without the daemon the files give the type default: the bug.
        assert_eq!(
            (tessera.clock.role, tessera.clock.priority),
            (ClockRole::Auto, 128)
        );
        for (role, priority) in [
            (ClockRole::Follower, 128),
            (ClockRole::Follower, 90),
            (ClockRole::Leader, 10),
            (ClockRole::Auto, 128),
        ] {
            let mut state = import::import(board_files()).unwrap();
            keep(&mut state, Some((role, priority))).unwrap();
            let clock = &state.document.tessera.as_ref().unwrap().clock;
            assert_eq!((clock.role, clock.priority), (role, priority));
        }
        // No answer: the files decide, as before.
        let mut state = import::import(board_files()).unwrap();
        keep(&mut state, None).unwrap();
        assert_eq!(state.document.tessera.unwrap().clock.role, ClockRole::Auto);
        // A device without Tessera has nothing to keep.
        let mut state = import::import(LegacyFiles::new()).unwrap();
        keep(&mut state, Some((ClockRole::Follower, 128))).unwrap();
        assert!(state.document.tessera.is_none());
    }

    #[test] // The role in the files is the daemon's too when it is locked there.
    fn the_daemon_wins_over_the_plugin_file() {
        let mut files = board_files();
        files.insert(
            "/etc/tessera/config.d/60-ndimon.toml".into(),
            "[[device]]\nid = \"main\"\nname = \"X\"\nlatency_us = 10000\n\n[clock]\nrole = \"leader\"\npriority = 5\n".into(),
        );
        let mut state = import::import(files).unwrap();
        assert_eq!(
            state.document.tessera.as_ref().unwrap().clock.role,
            ClockRole::Leader
        );
        keep(&mut state, Some((ClockRole::Follower, 200))).unwrap();
        let clock = &state.document.tessera.unwrap().clock;
        assert_eq!((clock.role, clock.priority), (ClockRole::Follower, 200));
    }

    async fn serve(listener: UnixListener, result: Value) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let n = stream.read_u32().await.unwrap();
        let mut bytes = vec![0; n as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(request["body"], json!({"query":"status"}));
        let reply = serde_json::to_vec(&json!({"v":1,"id":request["id"],"result":result})).unwrap();
        stream.write_u32(reply.len() as u32).await.unwrap();
        stream.write_all(&reply).await.unwrap();
    }

    #[test] // Over the real client and a socket, and a missing daemon is just None.
    fn observe_asks_the_daemon_and_survives_its_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        assert_eq!(observe(&path), None, "no daemon");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let listener = {
            let _guard = runtime.enter();
            UnixListener::bind(&path).unwrap()
        };
        let reply = json!({"ok":{"status":{"version":"0.1.0","devices":[],"clocks":[],"clock_settings":{"role":"follower","priority":128,"locked":[]}}}});
        let server = runtime.spawn(serve(listener, reply));
        assert_eq!(observe(&path), Some((ClockRole::Follower, 128)));
        runtime.block_on(server).unwrap();
    }
}
