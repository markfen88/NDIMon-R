//! Typed event hub and the transitional decoder IPC follower.

use crate::Shared;
use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use ndimon_state::ipc::{Decoder, UnixDecoder};
use ndimon_types::{Event, StatusSource};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

#[derive(Debug, Clone)]
pub enum HubMessage {
    Event(Arc<Event>),
    Resync,
}

#[derive(Debug, Clone)]
pub struct EventHub {
    sender: broadcast::Sender<HubMessage>,
}

impl EventHub {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    pub fn publish(&self, event: Event) {
        let _ = self.sender.send(HubMessage::Event(Arc::new(event)));
    }

    pub fn resync(&self) {
        let _ = self.sender.send(HubMessage::Resync);
    }
    pub fn subscribe(&self) -> broadcast::Receiver<HubMessage> {
        self.sender.subscribe()
    }
}

pub async fn events(State(state): State<Shared>) -> Response {
    let mut keepalive = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(15),
        Duration::from_secs(15),
    );
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let stream = stream::unfold(
        (state.hub.subscribe(), keepalive),
        |(mut receiver, mut keepalive)| async move {
            let event = tokio::select! {
                biased;
                _ = keepalive.tick() => Ok(":\n\n".to_owned()),
                message = receiver.recv() => match message {
                    Ok(HubMessage::Event(event)) => serde_json::to_string(event.as_ref())
                        .map(|data| format!("event: event\ndata: {data}\n\n")),
                    Ok(HubMessage::Resync) | Err(RecvError::Lagged(_)) => {
                        // Empty data is required for browsers to dispatch resync.
                        Ok("event: resync\ndata: \n\n".to_owned())
                    }
                    Err(RecvError::Closed) => return None,
                },
            };
            Some((event, (receiver, keepalive)))
        },
    );
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

pub fn follow_decoder(hub: EventHub, path: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // The decoder pushes routing/connection edges. Sample full telemetry
        // for the native stream too; one sampler is shared by every tab.
        let edges = async {
            let mut backoff = Duration::from_secs(1);
            loop {
                let connected_at = tokio::time::Instant::now();
                if let Err(error) = relay(&hub, &path).await {
                    tracing::debug!(%error, "decoder subscription disconnected");
                }
                hub.resync();
                if connected_at.elapsed() >= Duration::from_secs(30) {
                    backoff = Duration::from_secs(1);
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        };
        let samples = async {
            let decoder = UnixDecoder {
                socket: path.clone(),
                ..UnixDecoder::default()
            };
            let mut tick = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_millis(1500),
                Duration::from_millis(1500),
            );
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Ok(data) = decoder
                    .request(serde_json::json!({"action":"status"}))
                    .await
                {
                    hub.publish(Event::Status {
                        source: StatusSource::Decoder,
                        data,
                    });
                }
                if let Ok(data) = ndimon_state::ipc::ndi_sources().await {
                    hub.publish(Event::Status {
                        source: StatusSource::Finder,
                        data,
                    });
                }
            }
        };
        tokio::join!(edges, samples);
    })
}

async fn relay(hub: &EventHub, path: &std::path::Path) -> std::io::Result<()> {
    let mut socket = tokio::net::UnixStream::connect(path).await?;
    socket.write_all(b"{\"action\":\"subscribe\"}\n").await?;
    hub.resync();
    let mut reader = BufReader::new(socket);
    loop {
        let mut line = Vec::new();
        let count = (&mut reader)
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if count == 0 {
            return Ok(());
        }
        if count > 1024 * 1024 || line.last() != Some(&b'\n') {
            return Err(std::io::Error::other(
                "decoder frame too large or truncated",
            ));
        }
        match serde_json::from_slice::<Value>(&line) {
            Ok(data) => hub.publish(Event::Status {
                source: StatusSource::Decoder,
                data,
            }),
            Err(_) => hub.resync(),
        }
    }
}

/// Keep actor events typed until the SSE serialization boundary.
pub fn follow_state(
    hub: EventHub,
    handle: ndimon_state::StateHandle,
) -> tokio::task::JoinHandle<()> {
    let mut receiver = handle.subscribe();
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(event) => hub.publish(event),
                Err(RecvError::Lagged(_)) => {
                    hub.resync();
                    if let Err(error) = handle.query(ndimon_types::Query::State).await {
                        tracing::warn!(%error, "state event resync failed");
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}
