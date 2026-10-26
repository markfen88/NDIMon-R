//! Shared native follower framing, retries and revision-safe recovery.
use crate::{BoxFuture, persist::StateFile};
use serde_json::{Value, json};
use std::time::Duration;

type Client<'a> = dyn Fn(Value) -> BoxFuture<'a, Result<Value, String>> + Sync + 'a;

pub async fn request(client: &Client<'_>, command: Value) -> Result<Value, String> {
    let mut delay = Duration::from_millis(100);
    for attempt in 0..5 {
        match client(command.clone()).await {
            Ok(reply) if reply["ok"] == true => return Ok(reply),
            Ok(reply) => return Err(reply.to_string()),
            Err(error) if attempt == 4 => return Err(error),
            Err(_) => {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
    unreachable!("bounded retry")
}

/// What a follower answers when asked for a revision older than the one it has
/// applied (`follower::Revision::check` in `src/FollowerProtocol.h`).
pub const OLDER_REVISION: &str = "revision: older than last applied revision";

fn apply_config(revision: u64, config: &Value, reset: bool) -> Value {
    let mut command = json!({"action":"apply_config","revision":revision,"config":config});
    if reset {
        command["reset"] = json!(true);
    }
    command
}

pub async fn deliver(
    client: &Client<'_>,
    state: &StateFile,
    config: Value,
) -> Result<Value, String> {
    // A follower is normally at or behind the durable revision, so the first
    // request is the delivery itself: one round trip per write (VV-2 N4).
    let (revision, reply) =
        match request(client, apply_config(state.revision, &config, false)).await {
            Ok(reply) => (state.revision, reply),
            Err(refusal) if refusal.contains(OLDER_REVISION) => {
                ahead(client, state, &config, refusal).await?
            }
            Err(error) => return Err(error),
        };
    if reply["revision"].as_u64() != Some(revision) {
        return Err("follower acknowledged a different revision".into());
    }
    if reply["refused"].as_array().is_some_and(|v| !v.is_empty()) {
        return Err(reply.to_string());
    }
    Ok(reply)
}

/// The follower has applied a newer revision than the state. Recovery may find
/// the immediately following attempted global revision in its volatile cache.
/// Never invent a revision or accept an unrelated, arbitrarily newer cache: more
/// than one ahead can only be a cache of another lineage of the durable state
/// (the package was purged and installed again without a reboot, or an older
/// state was restored), so the durable document wins and the follower is told to
/// start over from it instead of refusing every write until a reboot.
async fn ahead(
    client: &Client<'_>,
    state: &StateFile,
    config: &Value,
    refusal: String,
) -> Result<(u64, Value), String> {
    let status = client(json!({"action":"status"})).await;
    let cached = status
        .ok()
        .and_then(|s| s["revision"].as_u64())
        .filter(|r| *r > state.revision);
    match cached {
        Some(r) if state.revision.checked_add(1) == Some(r) => {
            Ok((r, request(client, apply_config(r, config, false)).await?))
        }
        Some(r) => {
            tracing::warn!(
                follower = r,
                state = state.revision,
                "follower cache is ahead of the state: another lineage, resetting it"
            );
            Ok((
                state.revision,
                request(client, apply_config(state.revision, config, true)).await?,
            ))
        }
        None => Err(refusal),
    }
}
