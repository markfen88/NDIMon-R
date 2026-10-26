//! NDIMon owns Tessera intent and config files. The daemon remains the authority
//! for reload validation and the impact token shown to the operator.
use super::DomainModule;
use crate::{
    BoxFuture, StateHandle,
    apply::{Applier, ProductionApplier},
    persist::StateFile,
    plan::{Action as DomainAction, DomainPlan, Plan, internal},
};
use ndimon_render::LegacyFiles;
use ndimon_tessera::{Client, Error as ClientError, wire as upstream};
use ndimon_types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod reload;
pub use reload::Reloads;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    TesseraReload,
    TesseraService(TesseraServiceAction),
}
#[derive(Debug)]
pub struct Module;
fn invalid(reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new("tessera", reason)])
}
pub fn offline() -> ApiError {
    failure("offline", "Tessera daemon is offline")
}
fn failure(code: &str, detail: &str) -> ApiError {
    ApiError::ApplyFailed {
        domain: Domain::Tessera,
        step: "tessera".into(),
        reason: json!({"error":code,"detail":detail}).to_string(),
    }
}
pub fn api_error(error: ClientError) -> ApiError {
    use upstream::CommandError as E;
    match error {
        ClientError::Command(E::Invalid { field, reason }) => {
            ApiError::invalid(vec![FieldError::new(field, reason)])
        }
        ClientError::Command(E::Locked { field, by }) => ApiError::Locked { field, by },
        ClientError::Command(E::NotFound { what }) => ApiError::NotFound { what },
        ClientError::Command(E::Unsupported { what }) => ApiError::Unsupported { what },
        ClientError::Command(E::PermissionDenied) => ApiError::PermissionDenied,
        ClientError::Command(E::RateLimited) => ApiError::RateLimited,
        ClientError::Command(E::ConfirmationRequired { impact }) => {
            let mut own = Impact::new(ImpactScope::Service);
            own.token = ImpactToken::new(impact.token.as_deref().unwrap_or(""));
            own.restarts.push(Service::Tessera);
            own.interruptions.push(Interruption::new(
                Domain::Tessera,
                "Tessera network audio",
                None,
            ));
            own.lines.push(
                impact
                    .fields
                    .get("restart")
                    .and_then(|v| v["reason"].as_str())
                    .unwrap_or("Tessera configuration changes restart network audio")
                    .into(),
            );
            own.counterpart = Some(Box::new(json!(impact)));
            ApiError::ConfirmationRequired { impact: own }
        }
        ClientError::Timeout => failure("timeout", "Tessera request timed out"),
        ClientError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            ApiError::PermissionDenied
        }
        ClientError::Io(e) => failure("offline", &e.to_string()),
        ClientError::Protocol(e) => failure("protocol", &e),
        ClientError::Command(E::ResponseTooLarge { .. }) => {
            failure("response_too_large", "Tessera response exceeds frame limit")
        }
        e => failure("internal", &e.to_string()),
    }
}
impl DomainModule for Module {
    fn imperative(&self, command: &Command) -> bool {
        matches!(command, Command::TesseraService { .. })
    }
    fn observe(
        &self,
        handle: StateHandle,
        applier: &ProductionApplier,
    ) -> Option<BoxFuture<'static, ()>> {
        Some(observe(handle, applier.tessera.clone()))
    }
    fn external_confirmation(&self, command: &Command) -> bool {
        !crate::backup::handles(command)
            && !matches!(
                command,
                Command::TesseraService {
                    action: TesseraServiceAction::Start
                        | TesseraServiceAction::Stop
                        | TesseraServiceAction::Restart
                }
            )
    }
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        if !state.plugins.get("tessera").is_some_and(|p| p.installed) {
            return Err(ApiError::Unsupported {
                what: "Tessera plugin is not installed".into(),
            });
        }
        match command {
            Command::SetTessera(t) => {
                // Ownership locks are policy, not caller-removable settings.
                let mut t = t.clone();
                t.locks.extend(Tessera::owned_locks());
                if state.tessera.as_ref().is_some_and(|before| {
                    before.device.rx_channels != t.device.rx_channels
                        || before.device.tx_channels != t.device.tx_channels
                }) {
                    resize_legacy_audio(&mut state.audio, &t.device)?;
                }
                state.tessera = Some(t);
            }
            Command::PutTesseraFlows { flows } => {
                state
                    .tessera
                    .as_mut()
                    .ok_or_else(|| invalid("Tessera is not configured"))?
                    .flows = flows.clone()
            }
            Command::SetTesseraLabel {
                direction,
                channel,
                label,
            } => {
                let t = state
                    .tessera
                    .as_mut()
                    .ok_or_else(|| invalid("Tessera is not configured"))?;
                let (count, labels) = match direction {
                    Direction::Rx => (t.device.rx_channels, &mut t.device.rx_labels),
                    Direction::Tx => (t.device.tx_channels, &mut t.device.tx_labels),
                    _ => return Err(invalid("invalid direction")),
                };
                if *channel == 0 || *channel > count {
                    return Err(invalid("invalid channel label"));
                }
                if let Some(label) = label {
                    labels.insert(*channel, label.clone());
                } else {
                    labels.remove(channel);
                }
            }
            Command::RestoreTessera { tessera, audio } => {
                if audio.engine != state.audio.engine
                    || audio.settings != state.audio.settings
                    || audio.video_delay_ms != state.audio.video_delay_ms
                {
                    return Err(invalid(
                        "Tessera restore cannot change decoder engine settings; use the audio command",
                    ));
                }
                let mut t = tessera.clone();
                t.locks.extend(Tessera::owned_locks());
                state.tessera = Some(t);
                state.audio = audio.clone();
            }
            Command::TesseraService { .. } => (),
            _ => return Err(invalid("command does not belong to Tessera")),
        }
        if let Some(t) = &state.tessera {
            if let Some(binding) = &t.network_binding {
                if binding != &state.network.roles.audio
                    || !state.network.networks.iter().any(|n| &n.id == binding)
                {
                    return Err(invalid(
                        "Tessera binding must reference the adopted audio network role",
                    ));
                }
            }
            for source in state
                .audio
                .sources
                .values()
                .filter(|s| s.enabled && s.resource == "tessera:main:rx")
            {
                if source.channel_start + u32::from(source.channels) - 1
                    > u32::from(t.device.rx_channels)
                {
                    return Err(invalid("active RX endpoint exceeds Tessera channels"));
                }
            }
            for sink in state
                .audio
                .sinks
                .values()
                .filter(|s| s.enabled && s.resource == "tessera:main:tx")
            {
                if sink.channel_start + u32::from(sink.channels) - 1
                    > u32::from(t.device.tx_channels)
                {
                    return Err(invalid("active TX endpoint exceeds Tessera channels"));
                }
            }
        }
        Ok(())
    }
    fn plan(
        &self,
        previous: &DesiredState,
        next: &DesiredState,
        command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        if previous.tessera == next.tessera {
            return service_plan(command);
        }
        let mut impact = Impact::default();
        let disruptive = match (&previous.tessera, &next.tessera) {
            (Some(a), Some(b)) => {
                a.flows != b.flows
                    || a.device.sample_rate != b.device.sample_rate
                    || a.device.encoding != b.device.encoding
                    || a.device.latency_us != b.device.latency_us
                    || a.device.rx_channels != b.device.rx_channels
                    || a.device.tx_channels != b.device.tx_channels
            }
            _ => true,
        };
        if disruptive {
            impact.scope = ImpactScope::Service;
            impact.restarts.push(Service::Tessera);
            impact.interruptions.push(Interruption::new(
                Domain::Tessera,
                "Tessera network audio",
                None,
            ));
            impact.lines.push("Reload Tessera device geometry or flows; network audio pauses and audio clients may detach".into());
        }
        Ok(DomainPlan {
            actions: vec![DomainAction::Tessera(Action::TesseraReload)],
            impact,
        })
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        crate::import::import_tessera(state, files)
    }
    fn preflight<'a>(
        &self,
        applier: &'a ProductionApplier,
        _previous: &'a StateFile,
        next: &'a StateFile,
        _plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let (Some(t), Ok(status)) = (&next.document.tessera, applier.tessera.status().await)
            {
                if t.protocol_flow_lock != status.supports_flow_lock() {
                    return Err("Tessera flow lock mode must match the daemon contract; wait for capability reconciliation".into());
                }
            }
            Ok(())
        })
    }
    fn apply<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>> {
        let effect = self.effect(applier, state, plan, false);
        Box::pin(async move { effect.await.map_err(|e| e.to_string()) })
    }
    fn apply_checked<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, ApiError>> {
        self.effect(applier, state, plan, false)
    }
    fn rollback<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        let effect = self.effect(applier, state, plan, true);
        Box::pin(async move { effect.await.map(|_| ()).map_err(|e| e.to_string()) })
    }
    fn replay<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if state.document.tessera.is_some()
                && state
                    .document
                    .plugins
                    .get("tessera")
                    .is_some_and(|p| p.enabled)
            {
                match applier.tessera.status().await {
                    // Boot rendering precedes tesserad; the event observer retries
                    // replay after the daemon becomes reachable.
                    Err(ClientError::Io(_)) => {
                        applier.memory.tessera.forget();
                        return Ok(());
                    }
                    Err(e) => return Err(e.to_string()),
                    Ok(_) => (),
                }
                // Replay runs after every write and every 30 s: reload only
                // what the daemon would pick up.
                let reloads = &applier.memory.tessera;
                let seen = reloads.look(&applier.tessera).await;
                if reloads.due(&seen) {
                    applier
                        .tessera
                        .command(upstream::Command::ReloadConfig, Some(None))
                        .await
                        .map_err(|e| e.to_string())?;
                    reloads.sent(seen);
                }
            }
            Ok(())
        })
    }
    fn query<'a>(
        &self,
        state: &'a StateFile,
        applier: &'a dyn Applier,
        query: &'a Query,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        if !matches!(
            query,
            Query::TesseraStatus
                | Query::TesseraMetrics
                | Query::TesseraFlows
                | Query::TesseraDiagnostics
        ) {
            return None;
        }
        Some(Box::pin(async move {
            let flows = flow_values(
                state
                    .document
                    .tessera
                    .as_ref()
                    .map_or(&[], |t| t.flows.as_slice()),
            );
            let toml = state
                .document
                .tessera
                .as_ref()
                .map(|_| ndimon_render::tessera::flows(&state.document))
                .transpose()
                .map_err(internal)?
                .unwrap_or_else(|| {
                    "# NDIMon-R Tessera plugin; static flows, all device channels\n\n".into()
                });
            match query {
                Query::TesseraFlows => Ok(json!({"flows":flows,"toml":toml})),
                Query::TesseraMetrics => applier.tessera_query(upstream::Query::Metrics).await,
                Query::TesseraDiagnostics => Ok(
                    json!({"status":applier.tessera_query(upstream::Query::Status).await?,"metrics":applier.tessera_query(upstream::Query::Metrics).await?,"flows":flows,"experimental":true}),
                ),
                _ => {
                    let status = applier.tessera_query(upstream::Query::Status).await;
                    let mut error = status.as_ref().err().map(ToString::to_string);
                    let daemon = if status.is_ok() { "online" } else { "offline" };
                    let status = status.unwrap_or(Value::Null);
                    let metrics = if daemon == "online" {
                        match applier.tessera_query(upstream::Query::Metrics).await {
                            Ok(m) => m,
                            Err(e) => {
                                error = Some(e.to_string());
                                Value::Null
                            }
                        }
                    } else {
                        Value::Null
                    };
                    let core = applier
                        .decoder(json!({"action":"status"}))
                        .await
                        .unwrap_or(Value::Null);
                    let audio = ndimon_render::audio::project(
                        &state.document.audio,
                        state.audio_revision(),
                    );
                    let active =
                        state.document.audio.engine == AudioEngine::V2
                            && (state.document.audio.sources.iter().any(|(id, s)| {
                                s.enabled && TesseraEndpoint::is_rx(id, &s.resource)
                            }) || state.document.audio.sinks.iter().any(|(id, s)| {
                                s.enabled && TesseraEndpoint::is_tx(id, &s.resource)
                            }));
                    Ok(
                        json!({"daemon":daemon,"status":status,"metrics":metrics,"engine":core.get("audio").cloned().unwrap_or(Value::Null),"flows":flows,"audio":audio,"audio_engine":state.document.audio.engine,"audio_enabled":active,"bundle":{"installed_version":null,"embedded_version":applier.tessera_bundle_version().await,"daemon_version":status.get("version").cloned().unwrap_or(Value::Null),"experimental":true},"capabilities":{"subscriptions":false},"flow_toml":toml,"error":error}),
                    )
                }
            }
        }))
    }
}
impl Module {
    fn effect<'a>(
        &self,
        applier: &'a ProductionApplier,
        _state: &'a StateFile,
        plan: &'a Plan,
        rollback: bool,
    ) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async move {
            let mut result = json!("done");
            for action in &plan.actions {
                match action {
                    DomainAction::Tessera(
                        Action::TesseraReload
                        | Action::TesseraService(TesseraServiceAction::Reload),
                    ) => {
                        // The replay that follows this transaction must not
                        // send the same reload again.
                        let seen = applier.memory.tessera.look(&applier.tessera).await;
                        result = serde_json::to_value(
                            applier
                                .tessera
                                .command(
                                    upstream::Command::ReloadConfig,
                                    if rollback {
                                        Some(None)
                                    } else {
                                        plan.confirmation.clone()
                                    },
                                )
                                .await
                                .map_err(api_error)?,
                        )
                        .map_err(internal)?;
                        applier.memory.tessera.sent(seen);
                    }
                    DomainAction::Tessera(Action::TesseraService(action)) if !rollback => {
                        use ndimon_privd_proto::UnitAction;
                        let action = match action {
                            TesseraServiceAction::Start => UnitAction::Start,
                            TesseraServiceAction::Stop => UnitAction::Stop,
                            TesseraServiceAction::Restart => UnitAction::Restart,
                            _ => return Err(invalid("invalid service action")),
                        };
                        applier
                            .privd
                            .execute(ndimon_privd_proto::Request::TesseraService { action })
                            .await
                            .map_err(internal)?;
                    }
                    _ => (),
                }
            }
            Ok(json!({"ok":true,"tessera_result":result}))
        })
    }
}
fn service_plan(command: &Command) -> Result<DomainPlan, ApiError> {
    let Command::TesseraService { action } = command else {
        return Ok(DomainPlan::default());
    };
    let mut impact = Impact::default();
    if matches!(
        action,
        TesseraServiceAction::Stop | TesseraServiceAction::Restart
    ) {
        impact.scope = ImpactScope::Service;
        impact.restarts.push(Service::Tessera);
        impact.lines.push(format!(
            "Tessera service {action:?}; interrupts network audio"
        ));
        impact.interruptions.push(Interruption::new(
            Domain::Tessera,
            "Tessera network audio",
            None,
        ));
    }
    Ok(DomainPlan {
        actions: vec![DomainAction::Tessera(Action::TesseraService(*action))],
        impact,
    })
}

/// Only retained legacy aliases follow full-device geometry. Explicit v2 ranges
/// keep their identity and are refused below when an active range no longer fits.
fn resize_legacy_audio(audio: &mut Audio, device: &TesseraDevice) -> Result<(), ApiError> {
    let mut settings = ndimon_render::audio::project(audio, 0);
    let mut changed = false;
    for (kind, id, count) in [
        ("sources", "net-in", device.rx_channels),
        ("sinks", "net-out", device.tx_channels),
    ] {
        if let Some(node) = settings[kind].get_mut(id) {
            node["channels"] = json!(count);
            changed = true;
        }
    }
    if changed {
        *audio = super::audio::overlay(audio, &settings)?;
    }
    Ok(())
}

/// Legacy optional fields remain optional in the plugin projection.
pub fn flow_values(flows: &[TesseraFlow]) -> Value {
    json!(
        flows
            .iter()
            .map(|f| {
                let mut v = json!({"id":f.id,"direction":f.direction,"port":f.port});
                if let Some(peer) = &f.peer {
                    v["peer"] = json!(peer);
                }
                if f.any_source {
                    v["any_source"] = json!(true);
                }
                if let Some(n) = f.frames_per_packet {
                    v["frames_per_packet"] = json!(n);
                }
                v
            })
            .collect::<Vec<_>>()
    )
}

fn observation(event: upstream::Event) -> Option<Event> {
    match &event {
        upstream::Event::LockedValueRefused {
            locked_value_refused: e,
        } => Some(drift_event(&format!("tessera.{}", e.field))),
        upstream::Event::ProtocolFlowsChanged { .. } => Some(drift_event("tessera.flows")),
        upstream::Event::RuntimeValueChanged { .. } => {
            let mut delta = DomainObserved::default();
            delta.detail = Some(json!(event).to_string());
            Some(Event::Observed {
                domain: Domain::Tessera,
                delta,
            })
        }
        _ => None,
    }
}

struct EventReader(tokio::task::JoinHandle<()>);
impl Drop for EventReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// No command is inferred from a daemon event: runtime intent is observed, never
/// adopted into desired state. Reconnect and lagged streams trigger a full read.
fn observe(handle: StateHandle, client: Client) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let mut backoff = 1;
        loop {
            match handle.snapshot().await {
                Ok(state)
                    if state
                        .document
                        .plugins
                        .get("tessera")
                        .is_some_and(|p| p.installed && p.enabled) => {}
                Ok(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
                Err(_) => break,
            }
            let connected=async {
                let mut subscription=client.subscribe().await.map_err(api_error)?;
                // A timer must not cancel a partially read length header/payload.
                let (send,mut events)=tokio::sync::mpsc::channel(32);
                let _reader=EventReader(tokio::spawn(async move {
                    loop {
                        let reply=subscription.next().await;
                        let failed=reply.is_err();
                        if send.send(reply).await.is_err() || failed {break;}
                    }
                }));
                backoff=1;
                let status=client.status().await.map_err(api_error)?;
                let snapshot=handle.snapshot().await?;
                let mut desired=snapshot.document.tessera.clone();
                if desired.is_none() {
                    let mut imported=snapshot.document.clone();
                    crate::import::import_tessera(&mut imported,&snapshot.legacy_files).map_err(internal)?;
                    desired=imported.tessera;
                    if desired.is_none() {
                        let main=status.devices.iter().find(|d|d.key=="main").ok_or_else(||invalid("main Tessera device missing"))?;
                        let name=main.extra.get("name").and_then(Value::as_str).ok_or_else(||invalid("Tessera name missing"))?;
                        let latency=main.extra.get("latency").and_then(Value::as_u64).and_then(|n|u32::try_from(n).ok()).ok_or_else(||invalid("Tessera latency missing"))?;
                        let mut t=Tessera::new(name,latency);
                        t.device.sample_rate=main.sample_rate;
                        t.device.encoding=main.extra.get("encoding").and_then(Value::as_u64).and_then(|n|u8::try_from(n).ok()).ok_or_else(||invalid("Tessera encoding missing"))?;
                        t.device.rx_channels=u16::try_from(main.rx_channels.len()).map_err(internal)?;
                        t.device.tx_channels=u16::try_from(main.tx_channels.len()).map_err(internal)?;
                        for (labels,channels) in [(&mut t.device.rx_labels,&main.rx_channels),(&mut t.device.tx_labels,&main.tx_channels)] {for channel in channels {if let (Some(n),Some(label))=(channel["channel"].as_u64().and_then(|n|u16::try_from(n).ok()),channel["label"].as_str()){labels.insert(n,label.into());}}}
                        if let Some(clock)=status.extra.get("clock_settings").filter(|v|!v.is_null()){t.clock.role=serde_json::from_value(clock["role"].clone()).map_err(internal)?;t.clock.priority=clock["priority"].as_u64().and_then(|n|u8::try_from(n).ok()).ok_or_else(||invalid("Tessera clock priority missing"))?;}
                        desired=Some(t);
                    }
                }
                if let Some(mut desired)=desired {
                    desired.protocol_flow_lock=status.supports_flow_lock();
                    desired.locks.extend(Tessera::owned_locks());
                    if snapshot.document.tessera.as_ref()!=Some(&desired) {
                        let mut request=CommandRequest::new(Command::SetTessera(desired));request.expect_revision=Some(snapshot.revision);
                        handle.submit(request,Origin::Migration).await?;
                    }
                }
                handle.replay().await?;
                handle.observe(Event::Observed {domain:Domain::Tessera,delta:DomainObserved::default()}).await?;
                if !status.supports_flow_lock() && handle.snapshot().await?.document.tessera.is_some() {
                    handle.observe(drift_event("/etc/tessera/config.d/90-probe-flows.toml")).await?;
                    let mut delta=DomainObserved::default();delta.status=SyncStatus::Drift;delta.detail=Some("Flow ownership conflict: daemon lacks protocol flow locks; deprecated probe flows are emptied, but controllers can still change runtime flows. Install a Tessera release whose bundle ships the tessera-bundle installer (0.3.0 or later).".into());handle.observe(Event::Observed {domain:Domain::Tessera,delta}).await?;
                }
                handle.observe(Event::Status {source:StatusSource::Tessera,data:json!({"status":status,"metrics":client.metrics().await.map_err(api_error)?})}).await?;
                let mut timer=tokio::time::interval(std::time::Duration::from_secs(2));
                loop {
                    tokio::select! {
                        reply=events.recv()=>match reply.ok_or_else(offline)?.map_err(api_error)? {
                            upstream::Reply::Event(event)=> {if let Some(event)=observation(event) {handle.observe(event).await?;}}
                            upstream::Reply::Lagged {..}=> {let mut delta=DomainObserved::default();delta.status=SyncStatus::Drift;delta.detail=Some("Tessera event stream lagged; refreshed status".into());handle.observe(Event::Observed {domain:Domain::Tessera,delta}).await?;}
                            _=>(),
                        },
                        _=timer.tick()=> {
                            if !handle.snapshot().await?.document.plugins.get("tessera").is_some_and(|p|p.installed&&p.enabled) {return Ok(());}
                            handle.observe(Event::Status {source:StatusSource::Tessera,data:json!({"status":client.status().await.map_err(api_error)?,"metrics":client.metrics().await.map_err(api_error)?})}).await?;}
                    }
                }
                #[allow(unreachable_code)] Ok::<(),ApiError>(())
            }.await;
            if let Err(e) = connected {
                let mut delta = DomainObserved::default();
                delta.status = SyncStatus::Failed;
                delta.detail = Some(e.to_string());
                let _ = handle
                    .observe(Event::Observed {
                        domain: Domain::Tessera,
                        delta,
                    })
                    .await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    })
}

fn drift_event(path: &str) -> Event {
    Event::Drift {
        domain: Domain::Tessera,
        path: path.into(),
        artifact: None,
        foreign: None,
        selector: String::new(),
        interface: String::new(),
        neutralized: false,
        blocking: false,
    }
}
#[cfg(test)]
mod tests;
