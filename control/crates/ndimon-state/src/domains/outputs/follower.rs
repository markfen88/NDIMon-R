//! Complete-domain delivery. Equal-revision replay also repairs an interrupted
//! transaction whose candidate reached a follower before durable commit.
use crate::{apply::ProductionApplier, ipc::Decoder, persist::StateFile};
use ndimon_privd_proto::{ManagedUnit, Request, UnitAction};
use ndimon_types::Service;
use serde_json::{Value, json};

pub async fn request(client: &dyn Decoder, command: Value) -> Result<Value, String> {
    crate::follower::request(&|command| client.request(command), command).await
}
pub async fn deliver(
    client: &dyn Decoder,
    state: &StateFile,
    config: Value,
) -> Result<Value, String> {
    crate::follower::deliver(&|command| client.request(command), state, config).await
}

pub fn decoder_config(state: &StateFile) -> Value {
    // Audio deliberately retains reload_audio until its separate handover. Do
    // not claim device/network here; display has its own native delivery hook.
    json!({"outputs":state.document.outputs,"ndi":state.document.ndi})
}

pub async fn decoder(
    applier: &ProductionApplier,
    state: &StateFile,
    restarts: &[Service],
) -> Result<(), String> {
    let reply = deliver(applier.decoder.as_ref(), state, decoder_config(state)).await?;
    if reply["restart_required"] == true {
        if !restarts.contains(&Service::Decoder) {
            return Err("decoder requested a restart outside the declared impact".into());
        }
        restart(applier, ManagedUnit::Decoder).await?;
    }
    // P2 audio deliberately keeps its file/reload path. This reload respects
    // outputs/NDI IPC ownership and refreshes the shared NDIAudio projection.
    request(applier.decoder.as_ref(), json!({"action":"reload_config"})).await?;
    Ok(())
}
pub async fn finder(applier: &ProductionApplier, state: &StateFile) -> Result<(), String> {
    let reply = deliver(
        applier.finder.as_ref(),
        state,
        json!({"ndi":state.document.ndi}),
    )
    .await?;
    if reply["restart_required"] == true {
        return Err("finder requested an undeclared restart".into());
    }
    Ok(())
}
pub async fn restart(applier: &ProductionApplier, unit: ManagedUnit) -> Result<(), String> {
    applier
        .privd
        .execute(Request::ServiceControl {
            unit,
            action: UnitAction::Restart,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
