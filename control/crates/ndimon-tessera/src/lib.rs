//! Bounded, sequential u32-BE/JSON control.sock transport, with an independent
//! event connection. Dropping a failed exchange closes its session.
pub mod wire;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::Mutex,
};
use wire::*;
pub const MAX_FRAME: usize = 1 << 20;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("Tessera request timed out")]
    Timeout,
    #[error("Tessera protocol: {0}")]
    Protocol(String),
    #[error("{0}")]
    Command(CommandError),
}
#[derive(Debug)]
struct Inner {
    path: PathBuf,
    timeout: Duration,
    session: Mutex<Option<UnixStream>>,
    next: AtomicU64,
    online: AtomicBool,
}
#[derive(Debug, Clone)]
pub struct Client(Arc<Inner>);
impl Default for Client {
    fn default() -> Self {
        Self::new("/run/tessera/control.sock", Duration::from_secs(3))
    }
}
impl Client {
    pub fn new(path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self(Arc::new(Inner {
            path: path.into(),
            timeout,
            session: Mutex::new(None),
            next: AtomicU64::new(1),
            online: AtomicBool::new(false),
        }))
    }
    pub fn online(&self) -> bool {
        self.0.online.load(Ordering::Relaxed)
    }
    pub async fn request(&self, body: RequestBody) -> Result<Reply, Error> {
        let mut session = self.0.session.lock().await;
        let mut socket = session.take();
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let result = tokio::time::timeout(self.0.timeout, async {
            if socket.is_none() {
                socket = Some(UnixStream::connect(&self.0.path).await?);
            }
            let stream = socket.as_mut().expect("connected");
            write_request(stream, id, body).await?;
            checked(read_response(stream).await?, id)
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        let usable = matches!(&result, Ok(_) | Err(Error::Command(_)));
        self.0.online.store(usable, Ordering::Relaxed);
        if usable {
            *session = socket;
        }
        result
    }
    pub async fn status(&self) -> Result<Status, Error> {
        match self.request(RequestBody::Query(Query::Status)).await? {
            Reply::Status(s) => Ok(s),
            _ => Err(Error::Protocol("expected status".into())),
        }
    }
    pub async fn metrics(&self) -> Result<Metrics, Error> {
        match self.request(RequestBody::Query(Query::Metrics)).await? {
            Reply::Metrics(s) => Ok(s),
            _ => Err(Error::Protocol("expected metrics".into())),
        }
    }
    /// None: unconfirmed; Some(None): explicitly blind-confirmed; Some(Some(t)):
    /// forward exactly the opaque impact token the user saw.
    pub async fn command(
        &self,
        command: Command,
        confirmation: Option<Option<String>>,
    ) -> Result<Reply, Error> {
        self.request(match confirmation {
            None => RequestBody::Command(command),
            Some(expect) => RequestBody::ConfirmedCommand { command, expect },
        })
        .await
    }
    pub async fn subscribe(&self) -> Result<Subscription, Error> {
        tokio::time::timeout(self.0.timeout, async {
            let mut stream = UnixStream::connect(&self.0.path).await?;
            write_request(&mut stream, 1, RequestBody::SubscribeEvents).await?;
            match checked(read_response(&mut stream).await?, 1)? {
                Reply::Done => Ok(Subscription { stream }),
                _ => Err(Error::Protocol(
                    "expected subscription acknowledgement".into(),
                )),
            }
        })
        .await
        .unwrap_or(Err(Error::Timeout))
    }
}
#[derive(Debug)]
pub struct Subscription {
    stream: UnixStream,
}
impl Subscription {
    pub async fn next(&mut self) -> Result<Reply, Error> {
        // Idle subscriptions have no deadline; a partial frame has one.
        let first = self.stream.read_u8().await?;
        let response = tokio::time::timeout(
            Duration::from_secs(10),
            read_after_first(&mut self.stream, first),
        )
        .await
        .unwrap_or(Err(Error::Timeout))?;
        match checked(response, 1)? {
            r @ (Reply::Event(_) | Reply::Lagged { .. }) => Ok(r),
            _ => Err(Error::Protocol("unexpected event reply".into())),
        }
    }
}
fn checked(response: Response, id: u64) -> Result<Reply, Error> {
    if response.v != WIRE_VERSION
        || response.id != id && !(response.id == 0 && matches!(response.result, ResultBody::Err(_)))
    {
        return Err(Error::Protocol("response version or id mismatch".into()));
    }
    match response.result {
        ResultBody::Ok(r) => Ok(r),
        ResultBody::Err(e) => Err(Error::Command(e)),
    }
}
pub async fn write_request(
    stream: &mut UnixStream,
    id: u64,
    body: RequestBody,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec(&Request {
        v: WIRE_VERSION,
        id,
        body,
    })
    .map_err(|e| Error::Protocol(e.to_string()))?;
    if bytes.len() > MAX_FRAME {
        return Err(Error::Protocol("request frame limit".into()));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}
async fn read_response(stream: &mut UnixStream) -> Result<Response, Error> {
    let first = stream.read_u8().await?;
    read_after_first(stream, first).await
}
async fn read_after_first(stream: &mut UnixStream, first: u8) -> Result<Response, Error> {
    let mut header = [first, 0, 0, 0];
    stream.read_exact(&mut header[1..]).await?;
    let n = u32::from_be_bytes(header) as usize;
    if n == 0 || n > MAX_FRAME {
        return Err(Error::Protocol("response frame limit".into()));
    }
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Protocol(e.to_string()))
}
