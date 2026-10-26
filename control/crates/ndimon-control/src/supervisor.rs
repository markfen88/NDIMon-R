//! Source recovery and appliance health policy. All effects enter the state queue.
use crate::sse::{EventHub, HubMessage};
use ndimon_state::{StateHandle, commands::CommandActivity, ipc::Decoder};
use ndimon_types::{
    ApiError, Command, CommandRequest, DesiredState, Event, NdiSourceRef, Origin, StatusSource,
    WatchdogMode,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::broadcast, time::Instant};

pub const STATS_FILE: &str = "/tmp/ndimon-watchdog-stats.json";
const GRACE: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Serialize)]
struct Stats {
    watchdog_mode: WatchdogMode,
    recv_stalls: u64,
    video_stalls: u64,
    decoder_stalls: u64,
    display_freezes: u64,
    reconnects: u64,
    service_restarts: u64,
    timestamp: u64,
}
impl Stats {
    fn poll(&mut self, reply: Result<Value, String>) -> Vec<u8> {
        if self.watchdog_mode == WatchdogMode::Disabled {
            return Vec::new();
        }
        let Ok(reply) = reply else {
            return Vec::new();
        };
        let mut stalled = Vec::new();
        for output in reply["outputs"].as_array().into_iter().flatten() {
            if output["connected"] != true
                || matches!(output["health"].as_str(), None | Some("ok" | "idle"))
            {
                continue;
            }
            self.recv_stalls +=
                u64::from(output["recv_stale_ms"].as_i64().is_some_and(|n| n > 5000));
            self.video_stalls += u64::from(output["video_stale_ms"] == -1);
            self.decoder_stalls += u64::from(
                output["decoded_stale_ms"]
                    .as_i64()
                    .is_some_and(|n| n > 5000),
            );
            self.display_freezes += u64::from(
                output["display_stale_ms"]
                    .as_i64()
                    .is_some_and(|n| n > 3000),
            );
            if self.watchdog_mode == WatchdogMode::Active
                && output["stall_count"].as_u64().is_some_and(|n| n >= 60)
                && let Some(id) = output_id(output, "output")
            {
                stalled.push(id);
            }
        }
        stalled
    }
    async fn publish(&mut self, path: &Path) -> std::io::Result<()> {
        self.timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bytes = serde_json::to_vec(self)?;
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::{io::Write, os::unix::fs::PermissionsExt};
            let mut temporary = tempfile::Builder::new()
                .prefix(".ndimon-watchdog-")
                .tempfile_in(
                    path.parent()
                        .ok_or_else(|| std::io::Error::other("stats parent missing"))?,
                )?;
            temporary.write_all(&bytes)?;
            temporary
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o644))?;
            temporary.persist(path).map_err(|e| e.error)?;
            Ok(())
        })
        .await
        .map_err(std::io::Error::other)?
    }
}

#[derive(Debug, Clone)]
struct Retry {
    source: NdiSourceRef,
    attempts: u32,
    last_endpoint: String,
    due: Instant,
    stalled: bool,
}
impl Retry {
    fn endpoint(&mut self, sources: &Value) -> NdiSourceRef {
        self.attempts = self.attempts.saturating_add(1);
        let direct = self.attempts % 2 == 1;
        let mut endpoint = if direct {
            self.source.endpoint.clone()
        } else {
            String::new()
        };
        if let Some(discovered) = sources["list"][&self.source.name]
            .as_str()
            .filter(|s| ndimon_state::domains::outputs::valid_endpoint(s))
            .filter(|s| direct || *s != self.last_endpoint)
        {
            endpoint = discovered.into();
        }
        if !endpoint.is_empty() {
            self.last_endpoint = endpoint.clone();
        }
        NdiSourceRef::new(&self.source.name, endpoint)
    }
    fn reschedule(&mut self) {
        let seconds = match self.attempts {
            0 => 5,
            1 => 10,
            2 => 20,
            _ => 30,
        };
        self.due = Instant::now() + Duration::from_secs(seconds);
    }
}

#[derive(Debug, Default)]
struct Recovery {
    saved: DesiredState,
    retries: BTreeMap<u8, Retry>,
    grace: BTreeMap<u8, Instant>,
}
impl Recovery {
    fn activity(&mut self, activity: CommandActivity) {
        match activity.command {
            Command::SelectSource { output, .. }
            | Command::ReconnectSource { output }
            | Command::AdoptRemoteSource { output, .. } => {
                self.retries.remove(&output);
                self.grace.insert(output, activity.at + GRACE);
            }
            Command::ForgetSource { output } => {
                self.retries.remove(&output);
                self.grace.remove(&output);
            }
            _ => (),
        }
    }
    fn update(&mut self, saved: DesiredState) {
        self.retries.retain(|id, retry| {
            saved.outputs.get(id).and_then(|o| o.source.as_ref()) == Some(&retry.source)
                && (!retry.stalled || saved.device.watchdog_mode == WatchdogMode::Active)
        });
        self.saved = saved;
    }
    fn start(&mut self, id: u8, stalled: bool) {
        if self
            .grace
            .get(&id)
            .is_some_and(|until| *until > Instant::now())
        {
            return;
        }
        let Some(source) = self
            .saved
            .outputs
            .get(&id)
            .and_then(|o| o.source.as_ref())
            .filter(|s| !s.name.is_empty() && s.name != "None")
        else {
            self.retries.remove(&id);
            return;
        };
        self.retries.entry(id).or_insert_with(|| Retry {
            source: source.clone(),
            attempts: 0,
            last_endpoint: String::new(),
            due: Instant::now() + Duration::from_secs(5),
            stalled,
        });
    }
    fn connection(&mut self, id: u8, data: &Value) {
        if data["drm_ready"] == false {
            self.retries.remove(&id);
        } else if data["connected"] == true {
            self.grace.remove(&id);
            if !self.retries.get(&id).is_some_and(|r| r.stalled) {
                self.retries.remove(&id);
            }
        } else if data["connected"] == false {
            self.start(id, false);
        }
    }
    fn event(&mut self, data: &Value) {
        if data["type"] == "connection"
            && let Some(id) = output_id(data, "output")
        {
            // A real success event ends even watchdog-initiated recovery.
            if data["connected"] == true {
                self.retries.remove(&id);
            }
            self.connection(id, data);
        }
    }
    fn status(&mut self, data: &Value) {
        for output in data["outputs"].as_array().into_iter().flatten() {
            if output["connected"] == true
                && self
                    .grace
                    .get(&output_id(output, "output_index").unwrap_or(0))
                    .is_some_and(|until| *until > Instant::now())
            {
                continue;
            }
            if let Some(id) = output_id(output, "output_index") {
                self.connection(id, output);
            }
        }
    }
    fn deadline(&self) -> Instant {
        self.retries
            .values()
            .map(|r| r.due)
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86400))
    }
}
/// A Discovery Server controller switched an output (the decoder's
/// `remote_source` event): save that source, last writer wins. The SDK has
/// already switched the receiver, so the command carries no connect.
fn remote_source(saved: &DesiredState, data: &Value) -> Option<Command> {
    if data["type"] != "remote_source" {
        return None;
    }
    let id = output_id(data, "output")?;
    let name = data["source"].as_str()?;
    if !saved.ndi.remote_control {
        tracing::warn!(
            output = id,
            source = name,
            "Discovery Server route ignored: remote control is off"
        );
        return None;
    }
    let current = saved.outputs.get(&id).and_then(|o| o.source.as_ref());
    if current.map_or("", |s| s.name.as_str()) == name {
        return None;
    }
    let source = (!name.is_empty()).then(|| NdiSourceRef::new(name, ""));
    Some(Command::AdoptRemoteSource { output: id, source })
}
fn output_id(data: &Value, key: &str) -> Option<u8> {
    data[key].as_u64().filter(|n| *n < 8).map(|n| n as u8 + 1)
}

pub fn spawn(
    handle: StateHandle,
    decoder: Arc<dyn Decoder>,
    hub: EventHub,
) -> tokio::task::JoinHandle<()> {
    let events = hub.subscribe();
    let commands = handle.subscribe_commands();
    tokio::spawn(run(handle, decoder, events, commands, STATS_FILE.into()))
}

async fn refresh(handle: &StateHandle, recovery: &mut Recovery) {
    if let Ok(state) = handle.snapshot().await {
        recovery.update(state.document);
    }
}
fn drain(commands: &mut broadcast::Receiver<CommandActivity>, recovery: &mut Recovery) {
    loop {
        match commands.try_recv() {
            Ok(activity) => recovery.activity(activity),
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                // Lost manual activity is never permission to keep an old timer.
                recovery.retries.clear();
                for id in 1..=8 {
                    recovery.grace.insert(id, Instant::now() + GRACE);
                }
            }
            Err(_) => break,
        }
    }
}
async fn run(
    handle: StateHandle,
    decoder: Arc<dyn Decoder>,
    mut events: broadcast::Receiver<HubMessage>,
    mut commands: broadcast::Receiver<CommandActivity>,
    stats_path: PathBuf,
) {
    let mut recovery = Recovery::default();
    let mut stats = Stats::default();
    let mut status = tokio::time::interval(Duration::from_secs(5));
    let mut health = tokio::time::interval(Duration::from_secs(15));
    status.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            activity = commands.recv() => {
                match activity {
                    Ok(activity) => recovery.activity(activity),
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        recovery.retries.clear();
                        for id in 1..=8 { recovery.grace.insert(id, Instant::now() + GRACE); }
                    }
                }
                refresh(&handle, &mut recovery).await;
            }
            event = events.recv() => {
                refresh(&handle, &mut recovery).await;
                drain(&mut commands, &mut recovery);
                match event {
                    Ok(HubMessage::Event(event)) => if let Event::Status { source: StatusSource::Decoder, data } = event.as_ref() {
                        recovery.event(data);
                        if let Some(command) = remote_source(&recovery.saved, data) {
                            tracing::info!(?command, "Discovery Server routed an output");
                            if let Err(error) = handle.submit(CommandRequest::new(command), Origin::DiscoveryServer).await {
                                tracing::warn!(?error, "cannot save the Discovery Server route");
                            }
                            refresh(&handle, &mut recovery).await;
                            drain(&mut commands, &mut recovery);
                        }
                    },
                    Err(broadcast::error::RecvError::Closed) => return,
                    _ => { if let Ok(data) = decoder.request(json!({"action":"status"})).await { recovery.status(&data); } }
                }
            }
            _ = status.tick() => {
                refresh(&handle, &mut recovery).await;
                if let Ok(data) = decoder.request(json!({"action":"status"})).await {
                    drain(&mut commands, &mut recovery);
                    recovery.status(&data);
                }
            }
            _ = health.tick() => {
                refresh(&handle, &mut recovery).await;
                stats.watchdog_mode = recovery.saved.device.watchdog_mode;
                if stats.watchdog_mode != WatchdogMode::Active { recovery.retries.retain(|_, r| !r.stalled); }
                if stats.watchdog_mode != WatchdogMode::Disabled {
                    let reply = decoder.request(json!({"action":"health"})).await;
                    drain(&mut commands, &mut recovery);
                    if let Ok(data) = &reply {
                        for output in data["outputs"].as_array().into_iter().flatten() {
                            if matches!(output["health"].as_str(), Some("ok" | "idle"))
                                && let Some(id) = output_id(output, "output")
                                && recovery.retries.get(&id).is_some_and(|r| r.stalled) { recovery.retries.remove(&id); }
                        }
                    }
                    for id in stats.poll(reply) { recovery.start(id, true); }
                }
                if let Err(error) = stats.publish(&stats_path).await { tracing::warn!(%error, "cannot publish watchdog stats"); }
            }
            _ = tokio::time::sleep_until(recovery.deadline()) => {
                // Fence the queued retry against any command admitted during I/O,
                // including a manual reconnect which leaves revisions unchanged.
                let sequence = handle.command_sequence();
                refresh(&handle, &mut recovery).await;
                let sources = handle.query(ndimon_types::Query::Catalog { name: ndimon_types::Catalog::NdiSources, output: None }).await.unwrap_or(Value::Null);
                drain(&mut commands, &mut recovery);
                let due: Vec<_> = recovery.retries.iter().filter(|(_, r)| r.due <= Instant::now()).map(|(id, _)| *id).collect();
                for id in due {
                    let Some(retry) = recovery.retries.get_mut(&id) else { continue; };
                    let previous = retry.clone();
                    let source = retry.endpoint(&sources);
                    let stalled = retry.stalled;
                    let mut request = CommandRequest::new(Command::RetrySource { output: id, source });
                    request.expect_activity = Some(sequence);
                    tracing::info!(output = id, attempt = retry.attempts, "supervisor source retry");
                    let result = handle.submit(request, Origin::Boot).await;
                    if matches!(&result, Err(ApiError::Internal { message }) if message == "command superseded by newer activity") {
                        // Unrelated commands also advance the optimistic fence.
                        // They must not consume an output's attempt/backoff.
                        *retry = previous;
                        break;
                    }
                    if result.is_ok() && stalled { stats.reconnects += 1; }
                    retry.reschedule();
                    // Each admitted retry advances the fence. Process one per
                    // turn so the next output uses a fresh sequence and intent.
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
