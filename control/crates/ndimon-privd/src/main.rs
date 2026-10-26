use ndimon_privd::{Host, policy::PeerCheck, server};
use std::{io::Write, sync::Arc, time::Duration};
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();
    if let Err(e) = run().await {
        tracing::error!(error=%e,"privd stopped");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !nix::unistd::geteuid().is_root() {
        return Err("daemon and worker modes require root".into());
    }
    let host = Host::production()?;
    if args.first().is_some_and(|arg| arg == "network-job") {
        ndimon_privd::network::job(&host, &args[1..])?;
        return Ok(());
    }
    // Retain the previous boot argv for a rolling one-release upgrade.
    if args == ["--network-boot"] {
        ndimon_privd::network::job(&host, &["boot".into()])?;
        return Ok(());
    }
    if args.len() == 2 && args[0] == "--job" {
        let id = args[1].parse::<u64>()?;
        let result = host.run_job(id)?;
        std::io::stdout().write_all(result.stdout.as_bytes())?;
        std::io::stderr().write_all(result.stderr.as_bytes())?;
        return Ok(());
    }
    if args.len() == 2 && args[0] == "--cpu" {
        let profile = match args[1].as_str() {
            "apply" => ndimon_privd_proto::CpuProfile::Decode,
            "restore" => ndimon_privd_proto::CpuProfile::Restore,
            _ => return Err("invalid CPU action".into()),
        };
        // As in the old decoder hooks, tuning failures do not stop video.
        if let Err(e) = ndimon_privd::cpu::tune(&host, profile) {
            tracing::warn!(error=%e,"CPU tuning skipped");
        }
        return Ok(());
    }
    if !args.is_empty() {
        return Err("usage: ndimon-privd [--job ID | --cpu apply|restore | network-job boot|queue|apply ID|recover ID]".into());
    }
    let layout = server::Layout::production();
    let listeners = server::Listeners::inherit(&layout)?;
    // A socket layout that merges the callers is a misconfiguration: serve none.
    listeners.check_layout(&layout)?;
    let detection = PeerCheck::detect();
    tracing::info!(
        callers = ?listeners.callers(),
        peer_check = ?detection.check,
        reason = %detection.reason,
        "privd caller identity: listening socket, plus the peer's cgroup where the kernel allows"
    );
    let idle = std::env::var("NDIMON_PRIVD_IDLE_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(5, 3600);
    server::serve(
        listeners,
        Arc::new(host),
        detection.check,
        Duration::from_secs(idle),
    )
    .await
}
