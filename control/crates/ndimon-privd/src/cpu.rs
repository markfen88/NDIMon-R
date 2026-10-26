//! RK3588 global ondemand interval, journaled before mutation and restored only
//! if the saved value still belongs to us (or an interrupted write was pending).
use crate::{Host, store::io_error};
use ndimon_privd_proto::{ActionOutput, CpuProfile, Error};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    time::{Duration, Instant},
};
const TARGET: u32 = 10000;
const RATE: &str = "/sys/devices/system/cpu/cpufreq/ondemand/sampling_rate";
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    original: u32,
    applied: u32,
    pending: bool,
}
fn read_rate(host: &Host) -> Result<u32, Error> {
    let text = fs::read_to_string(host.root.path(RATE)).map_err(io_error)?;
    let value = text
        .trim()
        .parse()
        .map_err(|_| Error::invalid("sampling_rate", "not an integer"))?;
    if !(1..=60000000).contains(&value) {
        return Err(Error::invalid("sampling_rate", "outside supported range"));
    }
    Ok(value)
}
fn write_rate(host: &Host, value: u32) -> Result<(), Error> {
    let (dir, name) = host.root.parent(RATE, false, false)?;
    let fd = rustix::fs::openat(
        &dir.0,
        name.as_str(),
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::TRUNC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| Error::failed("sampling write", e))?;
    let mut file = std::fs::File::from(fd);
    file.write_all(format!("{value}\n").as_bytes())
        .map_err(io_error)?;
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        if read_rate(host)? == value {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::failed(
                "sampling write",
                "kernel did not accept interval",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn restore(host: &Host, dir: &crate::store::Directory) -> Result<(), Error> {
    let Some(bytes) = dir.read("cpu-sampling.json", 4096)? else {
        return Ok(());
    };
    let file = dir.open("cpu-sampling.json")?;
    let m = file.metadata().map_err(io_error)?;
    if !m.is_file() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o077 != 0 {
        return Err(Error::PermissionDenied);
    }
    let saved: Snapshot = serde_json::from_slice(&bytes)
        .map_err(|e| Error::invalid("sampling snapshot", e.to_string()))?;
    if !(1..=60000000).contains(&saved.original) || !(1..=60000000).contains(&saved.applied) {
        return Err(Error::invalid(
            "sampling snapshot",
            "interval outside supported range",
        ));
    }
    if host.root.path(RATE).exists() {
        let current = read_rate(host)?;
        if saved.pending || current == saved.applied {
            write_rate(host, saved.original)?;
            tracing::info!("restored original CPU sampling interval");
        } else {
            tracing::info!("preserved operator CPU sampling change");
        }
    }
    dir.remove("cpu-sampling.json")
}
pub fn tune(host: &Host, profile: CpuProfile) -> Result<ActionOutput, Error> {
    tune_with(
        host,
        profile,
        std::env::var("NDIMON_CPU_SAMPLING").as_deref() != Ok("off"),
        write_rate,
    )
}
fn tune_with(
    host: &Host,
    profile: CpuProfile,
    enabled: bool,
    write: impl Fn(&Host, u32) -> Result<(), Error>,
) -> Result<ActionOutput, Error> {
    let dir = host.root.directory("/run/ndimon", true, true)?;
    let _lock = dir.lock("cpu-sampling.lock")?;
    restore(host, &dir)?;
    if profile != CpuProfile::Decode {
        return Ok(ActionOutput::default());
    }
    if !enabled {
        return Ok(ActionOutput::default());
    }
    let compatible = match fs::read(host.root.path("/proc/device-tree/compatible")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ActionOutput::default()),
        Err(e) => return Err(io_error(e)),
    };
    if !compatible
        .split(|v| *v == 0)
        .any(|s| s == b"rockchip,rk3588")
        || !host.root.path(RATE).exists()
    {
        return Ok(ActionOutput::default());
    }
    let policies = fs::read_dir(host.root.path("/sys/devices/system/cpu/cpufreq"))
        .map_err(io_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io_error)?;
    let policies: Vec<_> = policies
        .into_iter()
        .filter(|p| p.file_name().to_string_lossy().starts_with("policy"))
        .collect();
    if policies.is_empty() {
        return Ok(ActionOutput::default());
    }
    for policy in policies {
        if fs::read_to_string(policy.path().join("scaling_governor"))
            .map_err(io_error)?
            .trim()
            != "ondemand"
        {
            return Ok(ActionOutput::default());
        }
    }
    let original = read_rate(host)?;
    if original <= TARGET {
        return Ok(ActionOutput::default());
    }
    let mut snapshot = Snapshot {
        original,
        applied: TARGET,
        pending: true,
    };
    let save = |s: &Snapshot| -> Result<(), Error> {
        dir.atomic(
            "cpu-sampling.json",
            &serde_json::to_vec(s).map_err(|e| Error::failed("snapshot", e))?,
            nix::unistd::geteuid().as_raw(),
            nix::unistd::getegid().as_raw(),
            0o600,
        )
    };
    save(&snapshot)?;
    if let Err(e) = write(host, TARGET) {
        let _ = restore(host, &dir);
        return Err(e);
    }
    snapshot.pending = false;
    if let Err(e) = save(&snapshot) {
        let _ = restore(host, &dir);
        return Err(e);
    }
    tracing::info!(original, applied = TARGET, "CPU sampling interval changed");
    Ok(ActionOutput::default())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_is_strict() {
        assert!(
            serde_json::from_str::<Snapshot>(r#"{"original":100,"applied":10000,"pending":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Snapshot>(
                r#"{"original":100,"applied":10000,"pending":false,"other":0}"#
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod legacy_tests;
