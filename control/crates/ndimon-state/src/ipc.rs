use crate::BoxFuture;
use serde_json::Value;
use std::{fmt::Debug, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

pub trait Decoder: Debug + Send + Sync + 'static {
    fn request(&self, command: Value) -> BoxFuture<'_, Result<Value, String>>;
}
pub async fn ndi_sources() -> Result<Value, String> {
    let bytes = match tokio::fs::read("/run/ndimon/ndimon-sources.json").await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match tokio::fs::read("/var/lib/ndimon/ndimon-sources.json").await {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(serde_json::json!({}));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// Read-only catalog observation, same framed v1 query as the legacy plugin.
pub async fn tessera_status() -> Result<Value, String> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = UnixStream::connect("/run/tessera/control.sock")
            .await
            .map_err(|e| e.to_string())?;
        let request =
            serde_json::to_vec(&serde_json::json!({"v":1,"id":1,"body":{"query":"status"}}))
                .map_err(|e| e.to_string())?;
        stream
            .write_u32(request.len() as u32)
            .await
            .map_err(|e| e.to_string())?;
        stream
            .write_all(&request)
            .await
            .map_err(|e| e.to_string())?;
        let n = stream.read_u32().await.map_err(|e| e.to_string())?;
        if n == 0 || n > 1024 * 1024 {
            return Err("invalid Tessera response frame".into());
        }
        let mut bytes = vec![0; n as usize];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        let reply: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if reply["v"] != 1 || reply["id"] != 1 {
            return Err("invalid Tessera response identity".into());
        }
        reply
            .pointer("/result/ok/status")
            .cloned()
            .ok_or_else(|| "Tessera status unavailable".into())
    })
    .await
    .map_err(|_| "Tessera status timeout".to_string())?
}
#[derive(Debug, Clone)]
pub struct UnixDecoder {
    pub socket: PathBuf,
    pub timeout: Duration,
}
impl Default for UnixDecoder {
    fn default() -> Self {
        Self {
            socket: "/tmp/ndi-decoder.sock".into(),
            timeout: Duration::from_millis(1500),
        }
    }
}
impl Decoder for UnixDecoder {
    fn request(&self, command: Value) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            tokio::time::timeout(self.timeout, async {
                let mut stream =
                    UnixStream::connect(&self.socket)
                        .await
                        .map_err(|e| match e.kind() {
                            std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::ConnectionRefused => "decoder_offline".into(),
                            _ => e.to_string(),
                        })?;
                let mut bytes = serde_json::to_vec(&command).map_err(|e| e.to_string())?;
                bytes.push(b'\n');
                stream.write_all(&bytes).await.map_err(|e| e.to_string())?;
                let mut reader = BufReader::new(stream);
                let mut reply = Vec::new();
                loop {
                    let available = reader.fill_buf().await.map_err(|e| e.to_string())?;
                    if available.is_empty() {
                        break;
                    }
                    let n = available
                        .iter()
                        .position(|b| *b == b'\n')
                        .map_or(available.len(), |i| i + 1);
                    if reply.len() + n > 4 * 1024 * 1024 {
                        return Err("ipc_response_too_large".into());
                    }
                    let ended = available[n - 1] == b'\n';
                    reply.extend_from_slice(&available[..n]);
                    reader.consume(n);
                    if ended {
                        break;
                    }
                }
                serde_json::from_slice(&reply).map_err(|_| "bad_ipc".into())
            })
            .await
            .map_err(|_| "timeout".to_string())?
        })
    }
}
