use crate::{
    ActionOutput, CONTROL_SOCKET, Envelope, Error, Reply, ReplyResult, Request, VERSION, codec,
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    timeout: Duration,
}
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
impl Default for Client {
    fn default() -> Self {
        Self::new(CONTROL_SOCKET, Duration::from_secs(30))
    }
}
impl Client {
    pub fn new(socket: impl AsRef<Path>, timeout: Duration) -> Self {
        Self {
            socket: socket.as_ref().into(),
            timeout,
        }
    }
    /// Wait for an independently managed job's durable result. Use this for
    /// reconciliation; `request` alone acknowledges that a job was started.
    pub async fn execute(&self, req: Request) -> Result<ActionOutput, Error> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let output = self.request(req).await?;
        let Some(job) = output.job.clone() else {
            return Ok(output);
        };
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::failed(
                    "job",
                    "timed out; system-manager job may still be running",
                ));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let status = tokio::time::timeout(
                remaining,
                self.request(Request::PluginJobStatus { job: job.clone() }),
            )
            .await
            .map_err(|_| Error::failed("job", "timed out; job may still be running"))??;
            if let Some(result) = status.data.as_ref().and_then(|v| v.get("result")) {
                return match serde_json::from_value::<ReplyResult>(result.clone())
                    .map_err(|e| Error::failed("job result", e))?
                {
                    ReplyResult::Ok(out) => Ok(out),
                    ReplyResult::Err(err) => Err(err),
                };
            }
            if status.stdout.trim() == "inactive" {
                return Err(Error::failed("job", "ended without a durable result"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    pub async fn request(&self, req: Request) -> Result<ActionOutput, Error> {
        req.validate()?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let future = async {
            let mut stream = tokio::net::UnixStream::connect(&self.socket)
                .await
                .map_err(|e| Error::failed("connect", e))?;
            codec::write_async(
                &mut stream,
                &Envelope {
                    v: VERSION,
                    id,
                    req,
                },
            )
            .await
            .map_err(|e| Error::failed("send", e))?;
            let reply: Reply = codec::read_async(&mut stream)
                .await
                .map_err(|e| Error::failed("receive", e))?;
            if reply.v != VERSION || reply.id != id {
                return Err(Error::failed("receive", "version or request id mismatch"));
            }
            match reply.result {
                ReplyResult::Ok(out) => Ok(out),
                ReplyResult::Err(err) => Err(err),
            }
        };
        tokio::time::timeout(self.timeout, future)
            .await
            .map_err(|_| Error::failed("request", "privd timeout"))?
    }
}
