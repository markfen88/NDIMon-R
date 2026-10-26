//! Socket activation, systemd readiness and watchdog heartbeats.

use ndimon_control::{AppState, config::Config, router, sse};
use ndimon_state::{StateHandle, apply::ProductionApplier, ipc::UnixDecoder};
use sd_notify::NotifyState;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::from_args().map_err(io::Error::other)?;
    ndimon_state::domains::plugins::host::initialize(config.legacy_root.clone())
        .map_err(io::Error::other)?;
    ndimon_state::domains::plugins::runtime::initialize(
        config.legacy_root.join("run/ndimon-control/plugins"),
    )
    .map_err(io::Error::other)?;
    // Root migration and privd readiness precede actor recovery/boot rendering.
    // No public or private listener accepts requests before start completes.
    let applier = Arc::new(ProductionApplier {
        memory: Default::default(),
        privd: ndimon_privd_proto::client::Client::new(
            &config.privd_socket,
            Duration::from_secs(30),
        ),
        tessera: ndimon_tessera::Client::new(&config.tessera_socket, Duration::from_secs(3)),
        decoder: Arc::new(UnixDecoder {
            socket: config.decoder_socket.clone(),
            ..UnixDecoder::default()
        }),
        finder: Arc::new(UnixDecoder {
            socket: config.finder_socket.clone(),
            timeout: Duration::from_secs(2),
        }),
    });
    let document = StateHandle::start(
        &config.state_dir,
        config.legacy_root.clone(),
        applier.clone(),
    )
    .await?
    .with_backup_io(Arc::new(ndimon_state::backup::environment::SystemIo {
        root: config.legacy_root.clone(),
        auth_file: config.auth_file.clone(),
        privd: ndimon_privd_proto::client::Client::new(
            &config.privd_socket,
            Duration::from_secs(10),
        ),
    }));
    let backups = Arc::new(ndimon_state::backup::plugins::Native::new(document.clone()));
    let document = document.with_plugin_backups(backups);
    let mut activated = listenfd::ListenFd::from_env();
    let mut listeners = Vec::new();
    for index in 0..activated.len() {
        if let Some(listener) = activated.take_tcp_listener(index)? {
            listener.set_nonblocking(true)?;
            listeners.push(TcpListener::from_std(listener)?);
        }
    }
    if listeners.is_empty() {
        listeners.push(TcpListener::bind(config.listen).await?);
    }
    let replay = document.spawn_replay();
    let follower = document.spawn_follower(config.legacy_root.clone());
    let observers = document.spawn_observers(applier.as_ref());
    let state = Arc::new(AppState::new(config).with_document(document.clone()));
    let state_events = sse::follow_state(state.hub.clone(), document.clone());
    let decoder = sse::follow_decoder(state.hub.clone(), state.config.decoder_socket.clone());
    let mut supervisor = ndimon_control::supervisor::spawn(
        document.clone(),
        applier.decoder.clone(),
        state.hub.clone(),
    );
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let mut servers = tokio::task::JoinSet::new();
    for listener in listeners {
        let app = router(state.clone())
            .into_make_service_with_connect_info::<ndimon_control::connections::SocketInfo>();
        let mut receiver = receiver.clone();
        servers.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = receiver.changed().await;
                })
                .await
        });
    }
    sd_notify::notify(false, &[NotifyState::Ready])?;
    let watchdog = tokio::spawn(async {
        let period = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0);
        let owner = std::env::var("WATCHDOG_PID").ok();
        if owner
            .as_ref()
            .is_some_and(|pid| pid.parse::<u32>().ok() != Some(std::process::id()))
        {
            return;
        }
        if let Some(period) = period {
            let mut interval = tokio::time::interval(Duration::from_micros((period / 2).max(1)));
            loop {
                interval.tick().await;
                if let Err(error) = sd_notify::notify(false, &[NotifyState::Watchdog]) {
                    tracing::warn!(%error, "systemd watchdog notification failed");
                }
            }
        }
    });
    let outcome = tokio::select! {
        () = stop_signal() => Ok(()),
        result = &mut supervisor => Err(io::Error::other(format!("supervisor stopped: {result:?}"))),
        result = servers.join_next() => match result {
            Some(Ok(result)) => result,
            Some(Err(error)) => Err(io::Error::other(error)),
            None => Ok(()),
        }
    };
    let _ = sd_notify::notify(false, &[NotifyState::Stopping]);
    let _ = shutdown.send(true);
    replay.abort();
    follower.abort();
    observers.abort();
    state_events.abort();
    decoder.abort();
    watchdog.abort();
    supervisor.abort();
    // Legacy SSE streams can remain open forever; use a bounded drain. The
    // state actor drains beside it: a transaction in flight (the plugin runtime
    // stopping with the unit starts one) must commit or roll back before the
    // process exits, or the next reader of the state finds `state.pending`.
    // Both fit TimeoutStopSec=10; past it privd's reader recovers the marker.
    let served = tokio::time::timeout(Duration::from_secs(5), async {
        while servers.join_next().await.is_some() {}
    });
    let (drained, _) = tokio::join!(document.shutdown(Duration::from_secs(8)), served);
    if let Err(error) = drained {
        tracing::error!(%error, "state transaction unfinished at shutdown; the next reader rolls it back");
    }
    servers.abort_all();
    outcome?;
    Ok(())
}

async fn stop_signal() {
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! { _ = tokio::signal::ctrl_c() => {}, () = terminate => {} }
}
