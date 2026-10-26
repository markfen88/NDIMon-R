//! Closed runtime units. No plugin-selected unit, environment, argv or identity.
use super::*;
use sha2::{Digest, Sha256};
fn unit(name: &PluginName) -> String {
    format!("ndimon-plugin-runtime-{name}.service")
}
fn identity(name: &PluginName) -> String {
    format!("ndp-{:x}", Sha256::digest(name.as_str().as_bytes()))[..28].to_owned()
}
pub fn status(host: &Host, name: &PluginName) -> Result<ActionOutput, Error> {
    let mut output = crate::system::systemctl(
        host,
        &[
            "show",
            "--property=ActiveState",
            "--value",
            "--",
            &unit(name),
        ],
    )?;
    let identity = crate::system::systemctl(
        host,
        &["show", "--property=UID", "--value", "--", &unit(name)],
    )?;
    output.data = Some(
        serde_json::json!({"unit":unit(name),"uid":identity.stdout.trim().parse::<u32>().ok(),"state":output.stdout.trim()}),
    );
    Ok(output)
}
pub fn stop(host: &Host, name: &PluginName) -> Result<ActionOutput, Error> {
    let loaded = crate::system::systemctl(
        host,
        &["show", "--property=LoadState", "--value", "--", &unit(name)],
    )?;
    if loaded.stdout.trim() == "not-found" {
        return Ok(ActionOutput::default());
    }
    crate::system::systemctl(host, &["stop", "--no-block", "--", &unit(name)])
}
/// A stack restart stops the old runtime without waiting (`stop --no-block`) and
/// the new control plane starts it again at once: while the old transient unit is
/// still being stopped (`deactivating`, up to its `TimeoutStopSec`) systemd-run
/// answers "already loaded or has a fragment file" (4 of 12 restarts in VV-2). Wait
/// for the stop to finish, for a bounded time. A unit that is merely active is not
/// waited for: that start is refused as it always was.
fn wait_for_stop(
    host: &Host,
    name: &PluginName,
    step: std::time::Duration,
    limit: std::time::Duration,
) -> Result<(), Error> {
    let end = std::time::Instant::now() + limit;
    loop {
        let state = crate::system::systemctl(
            host,
            &[
                "show",
                "--property=ActiveState",
                "--value",
                "--",
                &unit(name),
            ],
        )?;
        if state.stdout.trim() != "deactivating" || std::time::Instant::now() >= end {
            return Ok(());
        }
        std::thread::sleep(step);
    }
}
pub fn start(host: &Host, name: &PluginName) -> Result<ActionOutput, Error> {
    let base = base(host)?;
    let _lock = base.lock(".operation-lock")?;
    let manifest = installed(host, name)?;
    if destination(host, name).join(".broken").exists() {
        return Err(Error::PermissionDenied);
    }
    let process = manifest
        .process
        .as_ref()
        .ok_or_else(|| Error::invalid("process", "v2 manifest required"))?;
    // Pin every component and require root ownership and non-writable code.
    let mut directory = host
        .root
        .directory(&format!("{BASE}/{name}"), false, true)?;
    let mut parts = process.exec.split('/').peekable();
    let executable = loop {
        let component = parts.next().ok_or(Error::PermissionDenied)?;
        if parts.peek().is_none() {
            break directory.open(component)?;
        }
        directory = directory.child(component, false)?;
        directory.trusted()?;
    };
    let info = executable.metadata().map_err(io_error)?;
    if !info.is_file()
        || info.uid() != nix::unistd::geteuid().as_raw()
        || info.mode() & 0o022 != 0
        || info.mode() & 0o111 == 0
    {
        return Err(Error::PermissionDenied);
    }
    let socket_directory = format!("/run/ndimon-control/plugins/{name}");
    let socket_dir = host.root.directory(&socket_directory, false, false)?;
    let socket = rustix::fs::statat(
        &socket_dir.0,
        "control.sock",
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| Error::failed("plugin socket", e))?;
    let control_uid = if host.root.0 == Path::new("/") {
        nix::unistd::User::from_name("ndimon-control")
            .map_err(|e| Error::failed("control identity", e))?
            .ok_or(Error::PermissionDenied)?
            .uid
            .as_raw()
    } else {
        nix::unistd::geteuid().as_raw()
    };
    let directory = socket_dir.0.metadata().map_err(io_error)?;
    if directory.uid() != control_uid
        || directory.mode() & 0o777 != 0o700
        || socket.st_uid != control_uid
        || socket.st_mode & 0o777 != 0o666
        || socket.st_mode & nix::libc::S_IFMT != nix::libc::S_IFSOCK
    {
        return Err(Error::PermissionDenied);
    }
    let user = identity(name);
    let mut argv = args(&[
        "--quiet",
        "--collect",
        &format!("--unit={}", unit(name)),
        "--property=Type=exec",
        "--property=DynamicUser=yes",
        &format!("--property=User={user}"),
        "--property=NoNewPrivileges=yes",
        "--property=ProtectSystem=strict",
        "--property=ProtectHome=yes",
        "--property=PrivateTmp=yes",
        "--property=PrivateDevices=yes",
        "--property=ProtectControlGroups=yes",
        "--property=ProtectKernelTunables=yes",
        "--property=ProtectKernelModules=yes",
        "--property=RestrictSUIDSGID=yes",
        "--property=CapabilityBoundingSet=",
        "--property=AmbientCapabilities=",
        "--property=RestrictAddressFamilies=AF_UNIX",
        "--property=UMask=0077",
        "--property=TimeoutStopSec=5",
        "--property=InaccessiblePaths=-/run/ndimon-privd",
        &format!(
            "--property=BindReadOnlyPaths={}/control.sock:/run/ndimon-plugin/control.sock {}:/run/ndimon-plugin-code",
            host.root.path(&socket_directory).display(),
            destination(host, name).display()
        ),
        "--setenv=NDIMON_PLUGIN_SOCKET=/run/ndimon-plugin/control.sock",
    ]);
    if !manifest.supplementary_groups.is_empty() {
        argv.push(format!(
            "--property=SupplementaryGroups={}",
            manifest.supplementary_groups.join(" ")
        ));
    }
    if !manifest.readwrite_paths.is_empty() {
        argv.push(format!(
            "--property=ReadWritePaths={}",
            manifest.readwrite_paths.join(" ")
        ));
    }
    if host.root.0 == Path::new("/") {
        argv.push("--property=BindsTo=ndimon-control.service".into());
        argv.push("--property=After=ndimon-control.service".into());
    }
    argv.push("--".into());
    // systemd-run resolves its command before the unit's private binds exist.
    // A fixed env trampoline resolves the pinned plugin inside that namespace.
    argv.push("/usr/bin/env".into());
    argv.push("--".into());
    argv.push(format!("/run/ndimon-plugin-code/{}", process.exec));
    argv.extend(process.args.clone());
    wait_for_stop(
        host,
        name,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )?;
    host.runner.run("/usr/bin/systemd-run", &argv, None)?;
    // Query PID 1 directly: dynamic identities do not require libnss-systemd
    // on minimal appliances, and Type=exec has already allocated the identity.
    let properties = crate::system::systemctl(
        host,
        &[
            "show",
            "--property=UID",
            "--property=GID",
            "--",
            &unit(name),
        ],
    )?;
    let number = |key: &str| {
        properties
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| *n != u32::MAX)
    };
    let uid = number("UID=")
        .ok_or_else(|| Error::failed("plugin identity", "dynamic UID unavailable"))?;
    let gid = number("GID=")
        .ok_or_else(|| Error::failed("plugin identity", "dynamic GID unavailable"))?;
    if uid == 0 || uid == control_uid || gid == 0 {
        return Err(Error::PermissionDenied);
    }
    // The socket's 0700 control-owned parent blocks every other plugin on the
    // host. PID 1 binds only this socket file into the private unit namespace,
    // where its 0666 mode permits this unit's DynamicUser to connect. Privd
    // never writes control's runtime tree from its read-only mount namespace.
    Ok(ActionOutput {
        data: Some(serde_json::json!({"unit":unit(name),"uid":uid})),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{artifacts::UnavailableRenderer, process::Runner, store::Root};
    use std::{sync::Mutex, time::Duration};
    /// Answers `ActiveState` from a script and counts the questions.
    #[derive(Debug)]
    struct Script(Mutex<Vec<&'static str>>, Mutex<usize>);
    impl Runner for Script {
        fn run(&self, _: &str, _: &[String], _: Option<&[u8]>) -> Result<ActionOutput, Error> {
            *self.1.lock().unwrap() += 1;
            let mut script = self.0.lock().unwrap();
            let stdout = if script.len() > 1 {
                script.remove(0)
            } else {
                script[0]
            };
            Ok(ActionOutput {
                stdout: format!("{stdout}\n"),
                ..Default::default()
            })
        }
    }
    fn scripted(script: Vec<&'static str>) -> (Host, std::sync::Arc<Script>) {
        let runner = std::sync::Arc::new(Script(Mutex::new(script), Mutex::new(0)));
        let host = Host {
            root: Root(std::env::temp_dir()),
            runner: runner.clone(),
            ndimon_gid: 0,
            renderer: std::sync::Arc::new(UnavailableRenderer),
        };
        (host, runner)
    }
    #[test] // NI-17: the start of the new runtime waits for the stop of the old one.
    fn a_start_waits_while_the_old_runtime_is_stopping_and_not_for_ever() {
        let name: PluginName = "hello".to_owned().try_into().unwrap();
        let (host, runner) = scripted(vec!["deactivating", "deactivating", "inactive"]);
        wait_for_stop(
            &host,
            &name,
            Duration::from_millis(1),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(*runner.1.lock().unwrap(), 3);
        // Running, or unknown to systemd: nothing to wait for.
        for state in ["active", "inactive", ""] {
            let (host, runner) = scripted(vec![state]);
            wait_for_stop(
                &host,
                &name,
                Duration::from_millis(1),
                Duration::from_secs(5),
            )
            .unwrap();
            assert_eq!(*runner.1.lock().unwrap(), 1, "{state}");
        }
        // A unit that never finishes stopping does not hold the start for ever.
        let (host, runner) = scripted(vec!["deactivating"]);
        wait_for_stop(
            &host,
            &name,
            Duration::from_millis(1),
            Duration::from_millis(20),
        )
        .unwrap();
        assert!(*runner.1.lock().unwrap() > 2);
    }
}
