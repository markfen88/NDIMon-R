use crate::{Host, process::args};
use ndimon_privd_proto::{ActionOutput, Error, NtpServer, RebootSchedule};
pub fn systemctl(host: &Host, values: &[&str]) -> Result<ActionOutput, Error> {
    host.runner.run("/usr/bin/systemctl", &args(values), None)
}
pub fn ntp(host: &Host, server: Option<&NtpServer>) -> Result<ActionOutput, Error> {
    let timesync = "/etc/systemd/timesyncd.conf.d/ndimon.conf";
    let chrony = "/etc/chrony/sources.d/ndimon.sources";
    let has_chrony = host.root.path("/etc/chrony/chrony.conf").exists()
        || host.root.path("/usr/sbin/chronyd").exists();
    if let Some(server) = server {
        host.root.atomic(
            timesync,
            format!("[Time]\nNTP={server}\n").as_bytes(),
            nix::unistd::getegid().as_raw(),
            0o644,
        )?;
        if has_chrony {
            host.root.atomic(
                chrony,
                format!("server {server} iburst\n").as_bytes(),
                nix::unistd::getegid().as_raw(),
                0o644,
            )?;
        }
    } else {
        for path in [timesync, chrony] {
            if host.root.path(path).parent().is_some_and(|p| p.exists()) {
                host.root.remove(path)?;
            }
        }
    }
    let _ = host
        .runner
        .run("/usr/bin/timedatectl", &args(&["set-ntp", "true"]), None);
    if has_chrony {
        if systemctl(host, &["enable", "--now", "chrony"]).is_err() {
            let _ = systemctl(host, &["enable", "--now", "chronyd"]);
        }
        if systemctl(host, &["try-restart", "chrony"]).is_err() {
            let _ = systemctl(host, &["try-restart", "chronyd"]);
        }
    } else {
        let _ = systemctl(host, &["enable", "--now", "systemd-timesyncd"]);
        let _ = systemctl(host, &["try-restart", "systemd-timesyncd"]);
    }
    Ok(ActionOutput::default())
}
pub fn reboot_schedule(host: &Host, schedule: &RebootSchedule) -> Result<ActionOutput, Error> {
    let service = "/etc/systemd/system/ndimon-scheduled-reboot.service";
    let timer = "/etc/systemd/system/ndimon-scheduled-reboot.timer";
    if matches!(schedule, RebootSchedule::Disabled) {
        let _ = systemctl(host, &["disable", "--now", "ndimon-scheduled-reboot.timer"]);
        for path in [service, timer] {
            if host.root.path(path).parent().is_some_and(|p| p.exists()) {
                host.root.remove(path)?;
            }
        }
        return systemctl(host, &["daemon-reload"]);
    }
    let calendar = match schedule {
        RebootSchedule::Daily { time } => format!("*-*-* {time}:00"),
        RebootSchedule::Weekly { time, days } => format!(
            "{} *-*-* {time}:00",
            days.iter()
                .map(|d| d.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
        RebootSchedule::Disabled => unreachable!(),
    };
    host.root.atomic(service,b"[Unit]\nDescription=NDIMon-R scheduled reboot\n\n[Service]\nType=oneshot\nExecStart=/sbin/reboot\n",nix::unistd::getegid().as_raw(),0o644)?;
    host.root.atomic(timer,format!("[Unit]\nDescription=NDIMon-R scheduled reboot timer\n\n[Timer]\nOnCalendar={calendar}\nPersistent=false\nAccuracySec=1min\n\n[Install]\nWantedBy=timers.target\n").as_bytes(),nix::unistd::getegid().as_raw(),0o644)?;
    systemctl(host, &["daemon-reload"])?;
    systemctl(host, &["enable", "--now", "ndimon-scheduled-reboot.timer"])
}
