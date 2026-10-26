//! Isolated process sessions. Socket I/O and plugin code stay outside the actor.
use super::*;
use crate::{StateHandle, Submission};
use ndimon_privd_proto::plugin::{Manifest, validate_value};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::PathBuf, sync::OnceLock};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{Mutex, mpsc, oneshot},
};
const MAX_FRAME: usize = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);
#[derive(Debug)]
struct Call {
    request: Value,
    reply: oneshot::Sender<Result<Value, String>>,
}
#[derive(Debug)]
struct Connection {
    version: String,
    events: Vec<String>,
    sender: mpsc::Sender<Call>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Debug)]
pub struct Manager {
    sockets: PathBuf,
    connections: Mutex<BTreeMap<String, Connection>>,
}
static RUNTIME: OnceLock<Manager> = OnceLock::new();
pub fn native() -> &'static Manager {
    RUNTIME.get_or_init(|| Manager::new("/run/ndimon-control/plugins".into()))
}
pub fn initialize(sockets: PathBuf) -> Result<(), String> {
    RUNTIME
        .set(Manager::new(sockets))
        .map_err(|_| "plugin runtime already initialized".into())
}
impl Manager {
    pub fn new(sockets: PathBuf) -> Self {
        Self {
            sockets,
            connections: Mutex::new(BTreeMap::new()),
        }
    }
    pub async fn call(&self, name: &str, request: Value) -> Result<Value, String> {
        let (reply, response) = oneshot::channel();
        {
            let connections = self.connections.lock().await;
            let connection = connections.get(name).ok_or("plugin process unavailable")?;
            if connection.sender.try_send(Call { request, reply }).is_err() {
                connection.task.abort();
                return Err("plugin event backpressure".into());
            }
        }
        tokio::time::timeout(DEADLINE + Duration::from_millis(100), response)
            .await
            .map_err(|_| "plugin command timeout".to_string())?
            .map_err(|_| "plugin disconnected".to_string())?
    }
    pub async fn stop(&self, handle: &StateHandle, name: &str) {
        let _ = self
            .call(name, json!({"stop":{"reason":"disabled or replaced"}}))
            .await;
        self.connections.lock().await.remove(name);
        if let Ok(name) = name.to_owned().try_into() {
            let _ = privileged(handle.applier.as_ref(), Request::PluginRuntimeStop { name }).await;
        }
        let _ = tokio::fs::remove_file(self.sockets.join(name).join("control.sock")).await;
    }
    pub(super) async fn quiesce(&self, name: &str) {
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            self.call(name, json!({"stop":{"reason":"lifecycle transition"}})),
        )
        .await;
        self.connections.lock().await.remove(name);
    }
    pub async fn launch(&self, handle: StateHandle, manifest: Manifest) -> Result<(), String> {
        let name = manifest.name.as_str().to_owned();
        let state = handle.snapshot().await.map_err(|e| e.to_string())?;
        let plugin = state
            .document
            .plugins
            .get(&name)
            .ok_or("plugin not installed")?;
        let data = plugin_data(plugin, &manifest)?;
        let directory = self.sockets.join(&name);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|e| e.to_string())?;
        tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| e.to_string())?;
        let path = directory.join("control.sock");
        if let Ok(info) = tokio::fs::symlink_metadata(&path).await {
            use std::os::unix::fs::FileTypeExt;
            if !info.file_type().is_socket() {
                return Err("plugin socket is not a socket".into());
            }
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| e.to_string())?;
        }
        let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .await
            .map_err(|e| e.to_string())?;
        let started = privileged(
            handle.applier.as_ref(),
            Request::PluginRuntimeStart {
                name: manifest.name.clone(),
            },
        )
        .await?;
        let uid = started["data"]["uid"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("missing plugin UID")?;
        let (mut stream, _) = tokio::time::timeout(DEADLINE, listener.accept())
            .await
            .map_err(|_| "plugin handshake timeout".to_string())?
            .map_err(|e| e.to_string())?;
        if stream.peer_cred().map_err(|e| e.to_string())?.uid() != uid {
            return Err("plugin peer UID mismatch".into());
        }
        let hello = tokio::time::timeout(DEADLINE, read_frame(&mut stream))
            .await
            .map_err(|_| "plugin handshake timeout".to_string())??;
        if hello["v"] != 1
            || hello["id"] != 1
            || hello["req"]["hello"]["name"] != name
            || hello["req"]["hello"]["version"] != manifest.version.as_str()
            || hello.as_object().is_none_or(|o| o.len() != 3)
            || hello["req"].as_object().is_none_or(|o| o.len() != 1)
        {
            return Err("invalid plugin identity".into());
        }
        write_frame(
            &mut stream,
            &json!({"v":1,"id":1,"ok":{"protocol":1,"revision":state.revision,"state":data}}),
        )
        .await?;
        let (sender, requests) = mpsc::channel(16);
        let session_manifest = manifest.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = session(stream, requests, handle, session_manifest).await {
                tracing::warn!(%error, "plugin disconnected");
            }
        });
        self.connections.lock().await.insert(
            name.clone(),
            Connection {
                version: manifest.version.to_string(),
                events: manifest.events.clone(),
                sender,
                task,
            },
        );
        self.call(
            &name,
            json!({"start":{"revision":state.revision,"state":data}}),
        )
        .await?;
        Ok(())
    }
    async fn enqueue(&self, name: &str, request: Value) {
        let (reply, _) = oneshot::channel();
        if let Some(connection) = self.connections.lock().await.get(name) {
            if connection.sender.try_send(Call { request, reply }).is_err() {
                connection.task.abort();
            }
        }
    }
    async fn synchronize(&self, state: &StateFile) {
        for (name, plugin) in &state.document.plugins {
            if !plugin.enabled {
                continue;
            }
            if let Ok(manifest) = host::native().manifest(name) {
                if manifest.process.is_some() {
                    if let Ok(data) = plugin_data(plugin, &manifest) {
                        self.enqueue(
                            name,
                            json!({"start":{"revision":state.revision,"state":data}}),
                        )
                        .await;
                    }
                }
            }
        }
    }
    pub async fn event(&self, event: &Event) {
        let names = self
            .connections
            .lock()
            .await
            .iter()
            .map(|(n, c)| (n.clone(), c.events.clone()))
            .collect::<Vec<_>>();
        let mut sanitized = event.clone();
        match &mut sanitized {
            Event::StateChanged { origin, .. } => *origin = Origin::Migration,
            Event::Observed { delta, .. } => delta.detail = None,
            Event::Drift {
                artifact, foreign, ..
            } => {
                *artifact = None;
                *foreign = None;
            }
            _ => return,
        }
        let value = match serde_json::to_value(sanitized) {
            Ok(v) => v,
            Err(_) => return,
        };
        let Some(kind) = value.as_object().and_then(|o| o.keys().next()) else {
            return;
        };
        for (name, wanted) in names {
            if wanted.iter().any(|e| e == kind) {
                // The session owns the bounded queue and ordered acknowledgements.
                let (reply, _) = oneshot::channel();
                if let Some(connection) = self.connections.lock().await.get(&name) {
                    if connection
                        .sender
                        .try_send(Call {
                            request: json!({"event":{"event":value}}),
                            reply,
                        })
                        .is_err()
                    {
                        connection.task.abort();
                    }
                }
            }
        }
    }
}
fn plugin_data(plugin: &PluginState, manifest: &Manifest) -> Result<Value, String> {
    let schema = manifest
        .state
        .as_ref()
        .ok_or("missing plugin state schema")?;
    let data = if plugin.state.is_none() {
        schema.default.clone()
    } else {
        json!(plugin.state)
    };
    validate_value(&schema.schema, &data)?;
    Ok(data)
}
pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(stream: &mut R) -> Result<Value, String> {
    // Idle connections are allowed; once a frame begins its remaining bytes
    // must arrive within the protocol deadline.
    let first = stream.read_u8().await.map_err(|e| e.to_string())?;
    tokio::time::timeout(DEADLINE, async {
        let mut header = [first, 0, 0, 0];
        stream
            .read_exact(&mut header[1..])
            .await
            .map_err(|e| e.to_string())?;
        let n = u32::from_be_bytes(header) as usize;
        if n == 0 || n > MAX_FRAME {
            return Err("invalid plugin frame length".into());
        }
        let mut bytes = vec![0; n];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "plugin frame timeout".to_string())?
}
pub async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    value: &Value,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err("invalid plugin frame length".into());
    }
    tokio::time::timeout(DEADLINE, async {
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(&bytes).await
    })
    .await
    .map_err(|_| "plugin write timeout".to_string())?
    .map_err(|e: std::io::Error| e.to_string())
}
struct ReaderTask(tokio::task::JoinHandle<()>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn session(
    stream: UnixStream,
    mut calls: mpsc::Receiver<Call>,
    handle: StateHandle,
    manifest: Manifest,
) -> Result<(), String> {
    let (mut reader, mut writer) = stream.into_split();
    let (frames, mut incoming) = mpsc::channel(16);
    let _reader = ReaderTask(tokio::spawn(async move {
        loop {
            let frame = read_frame(&mut reader).await;
            let failed = frame.is_err();
            if frames.try_send(frame).is_err() || failed {
                break;
            }
        }
    }));
    let mut next_id = 2u64;
    let mut plugin_id = 1u64;
    loop {
        tokio::select! {
            frame = incoming.recv() => {
                let frame = frame.ok_or("plugin disconnected")??;
                process_request(&mut writer, &frame, &handle, &manifest, &mut plugin_id).await?;
            }
            call = calls.recv() => {
                let Some(call) = call else { return Ok(()); };
                let id = next_id; next_id = next_id.checked_add(1).ok_or("plugin ID exhausted")?;
                write_frame(&mut writer, &json!({"v":1,"id":id,"req":call.request})).await?;
                let result = tokio::time::timeout(DEADLINE, async {
                    loop {
                        let frame = incoming.recv().await.ok_or("plugin disconnected")??;
                        if frame.get("req").is_some() { process_request(&mut writer, &frame, &handle, &manifest, &mut plugin_id).await?; continue; }
                        if frame["v"] != 1 || frame["id"] != id || frame.as_object().is_none_or(|o|o.len()!=3) { return Err("invalid plugin response ID or envelope".into()); }
                        if let Some(error) = frame.get("err") { return Err(error["message"].as_str().unwrap_or("plugin command failed").to_string()); }
                        return frame.get("ok").cloned().ok_or_else(||"missing plugin response".into());
                    }
                }).await.unwrap_or_else(|_|Err("plugin command timeout".into()));
                let failed = result.is_err(); let _ = call.reply.send(result);
                if failed { return Err("plugin command failed or timed out".into()); }
            }
        }
    }
}
async fn process_request<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Value,
    handle: &StateHandle,
    manifest: &Manifest,
    last_id: &mut u64,
) -> Result<(), String> {
    let id = frame["id"]
        .as_u64()
        .filter(|id| *id > *last_id)
        .ok_or("duplicate plugin request ID")?;
    if frame["v"] != 1
        || frame.as_object().is_none_or(|o| o.len() != 3)
        || frame["req"].as_object().is_none_or(|o| o.len() != 1)
    {
        return Err("invalid plugin request envelope".into());
    }
    *last_id = id;
    let result = if let Some(p) = frame["req"].get("propose_state") {
        let expected = p["expect_revision"]
            .as_u64()
            .ok_or("missing plugin revision")?;
        let data = p["state"].clone();
        match validate_value(
            &manifest
                .state
                .as_ref()
                .ok_or("missing plugin schema")?
                .schema,
            &data,
        ) {
            Err(e) => Err(e),
            Ok(()) => {
                let mut request = CommandRequest::new(Command::SetPluginData {
                    name: manifest.name.to_string(),
                    state: serde_json::from_value(data).map_err(|e| e.to_string())?,
                });
                request.expect_revision = Some(expected);
                handle
                    .submit(
                        request,
                        Origin::Plugin {
                            name: manifest.name.to_string(),
                        },
                    )
                    .await
                    .map(|r| json!({"revision":r.revision}))
                    .map_err(|e| e.to_string())
            }
        }
    } else if let Some(p) = frame["req"].get("privilege") {
        let command = p["command"].as_str().unwrap_or("");
        if p.as_object().is_none_or(|o| o.len() != 1)
            || !manifest.priv_commands.iter().any(|c| c.as_str() == command)
        {
            Err("plugin privilege not allowed".into())
        } else {
            privileged(
                handle.applier.as_ref(),
                Request::PluginExec {
                    name: manifest.name.clone(),
                    command: command.to_owned().try_into()?,
                },
            )
            .await
        }
    } else {
        Err("unsupported plugin request".into())
    };
    let reply = match result {
        Ok(value) => json!({"v":1,"id":id,"ok":value}),
        Err(error) => {
            let state = handle.snapshot().await.map_err(|e| e.to_string())?;
            json!({"v":1,"id":id,"err":{"code":"rejected","message":error,"revision":state.revision,"state":state.document.plugins.get(manifest.name.as_str()).map(|p|json!(p.state))}})
        }
    };
    write_frame(writer, &reply).await
}
impl StateHandle {
    /// Commands execute off-actor; the complete proposal is CAS-committed by the
    /// ordinary domain transaction, including installed-schema validation.
    pub async fn plugin_command(
        &self,
        name: String,
        command: String,
        input: Value,
        revision: Option<u64>,
        origin: Origin,
    ) -> Result<(Submission, Value), ApiError> {
        self.plugin_command_with(native(), name, command, input, revision, origin)
            .await
    }
    pub async fn plugin_command_with(
        &self,
        manager: &Manager,
        name: String,
        command: String,
        input: Value,
        revision: Option<u64>,
        origin: Origin,
    ) -> Result<(Submission, Value), ApiError> {
        let before = self.snapshot().await?;
        if before.plugin_job.is_some() {
            return Err(ApiError::StillApplying);
        }
        let expected = revision.unwrap_or(before.revision);
        if expected != before.revision {
            return Err(ApiError::RevisionConflict {
                current_revision: before.revision,
            });
        }
        let entry = self
            .applier
            .plugin_query(&before, json!({"operation":"manifest","name":name}))
            .await
            .map_err(internal)?;
        let manifest: Manifest =
            serde_json::from_value(entry["manifest"].clone()).map_err(invalid)?;
        if manifest.process.is_none()
            || before
                .document
                .plugins
                .get(&name)
                .is_none_or(|p| !p.enabled)
        {
            return Err(invalid("plugin process is not enabled"));
        }
        let schema = manifest
            .commands
            .get(&command)
            .ok_or_else(|| invalid("unknown plugin command"))?;
        validate_value(&schema.input, &input).map_err(invalid)?;
        let reply = manager
            .call(
                &name,
                json!({"command":{"name":command,"input":input,"revision":expected}}),
            )
            .await
            .map_err(internal)?;
        let accepted = async {
            validate_value(&schema.output, &reply["output"]).map_err(invalid)?;
            validate_value(
                &manifest
                    .state
                    .as_ref()
                    .ok_or_else(|| invalid("missing plugin state"))?
                    .schema,
                &reply["state"],
            )
            .map_err(invalid)?;
            let data = serde_json::from_value(reply["state"].clone()).map_err(invalid)?;
            let mut request = CommandRequest::new(Command::SetPluginData {
                name: name.clone(),
                state: data,
            });
            request.expect_revision = Some(expected);
            self.submit_with_result(request, origin).await
        }
        .await;
        match accepted {
            Ok(submission) => Ok((submission, reply["output"].clone())),
            Err(error) => {
                let committed = self.snapshot().await?;
                if let Some(plugin) = committed.document.plugins.get(&name) {
                    let _ = manager.call(&name, json!({"start":{"revision":committed.revision,"state":plugin_data(plugin,&manifest).map_err(invalid)?}})).await;
                }
                Err(error)
            }
        }
    }
}
/// One observer group owns runtime reconciliation, event delivery and the feed
/// timer. Dropping it cancels in-flight work through the parent's task group.
struct ObserverConnections;
impl Drop for ObserverConnections {
    fn drop(&mut self) {
        if let Ok(mut connections) = native().connections.try_lock() {
            connections.clear();
        }
    }
}
pub(super) async fn observe(handle: StateHandle) {
    let _connections = ObserverConnections;
    tokio::join!(reconcile(handle.clone()), feeds(handle));
}
async fn feeds(handle: StateHandle) {
    let mut timer = tokio::time::interval(Duration::from_secs(900));
    loop {
        timer.tick().await;
        let _ = handle.query(Query::RefreshPluginFeed).await;
    }
}
async fn reconcile(handle: StateHandle) {
    let mut events = handle.subscribe();
    let mut retry = BTreeMap::<String, (tokio::time::Instant, u32)>::new();
    let mut starting = std::collections::BTreeSet::new();
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        while let Some(result) = tasks.try_join_next() {
            if let Ok((name, result)) = result {
                starting.remove(&name);
                match result {
                    Ok(()) => {}
                    Err(error) => {
                        tracing::warn!(plugin=%name,%error,"plugin runtime start failed");
                        let attempts = retry.get(&name).map_or(0, |(_, n)| *n).saturating_add(1);
                        retry.insert(
                            name,
                            (
                                tokio::time::Instant::now()
                                    + Duration::from_secs((1u64 << attempts.min(5)).min(30)),
                                attempts,
                            ),
                        );
                    }
                }
            }
        }
        let state = match handle.snapshot().await {
            Ok(s) => s,
            Err(_) => break,
        };
        let active = native()
            .connections
            .lock()
            .await
            .iter()
            .map(|(n, c)| (n.clone(), (c.version.clone(), c.task.is_finished())))
            .collect::<BTreeMap<_, _>>();
        for (name, (version, failed)) in &active {
            if *failed
                || state
                    .document
                    .plugins
                    .get(name)
                    .is_none_or(|p| !p.enabled || &p.version != version)
            {
                native().stop(&handle, name).await;
                if *failed {
                    let attempts = retry.get(name).map_or(0, |(_, n)| *n).saturating_add(1);
                    retry.insert(
                        name.clone(),
                        (
                            tokio::time::Instant::now()
                                + Duration::from_secs((1u64 << attempts.min(5)).min(30)),
                            attempts,
                        ),
                    );
                    let _ = runtime_observation(
                        &handle,
                        name,
                        version,
                        Some("plugin process disconnected".into()),
                    )
                    .await;
                }
            }
        }
        if state.plugin_job.is_none() {
            for (name, plugin) in &state.document.plugins {
                if !plugin.enabled || name == "tessera" || starting.contains(name) {
                    continue;
                }
                let Ok(manifest) = host::native().manifest(name) else {
                    continue;
                };
                if manifest.version.as_str() != plugin.version {
                    continue;
                }
                // Manifest-v1 plugins cannot be activated without the removed loader.
                if manifest.process.is_none() {
                    continue;
                }
                let observation = state
                    .plugin_runtime
                    .get("plugins")
                    .and_then(|v| v.get(name))
                    .cloned()
                    .unwrap_or(Value::Null);
                let pending = observation["version"] != plugin.version
                    || observation["pending_enable"] == true;
                if !pending && observation["enable_error"].is_string()
                    || native().connections.lock().await.contains_key(name)
                {
                    continue;
                }
                if retry
                    .get(name)
                    .is_some_and(|(at, _)| *at > tokio::time::Instant::now())
                {
                    continue;
                }
                starting.insert(name.clone());
                let handle = handle.clone();
                let name = name.clone();
                tasks.spawn(async move {
                    let result = activate(handle, manifest, pending).await;
                    (name, result)
                });
            }
        }
        tokio::select! {
            event=events.recv()=>match event {
                Ok(event)=> {
                    if matches!(&event,Event::StateChanged { domains, .. } if domains.contains(&Domain::Plugins)) {
                        if let Ok(snapshot)=handle.snapshot().await { native().synchronize(&snapshot).await; }
                    }
                    native().event(&event).await;
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=> { let names=native().connections.lock().await.keys().cloned().collect::<Vec<_>>(); for name in names { native().stop(&handle,&name).await; } }, Err(_)=>break
            },
            _=tokio::time::sleep(Duration::from_millis(250))=>(),
        }
    }
}
static ACTIVATION: Mutex<()> = Mutex::const_new(());
async fn activate(handle: StateHandle, manifest: Manifest, pending: bool) -> Result<(), String> {
    let name = manifest.name.to_string();
    let version = manifest.version.to_string();
    if pending {
        let _activation = ACTIVATION.lock().await;
        loop {
            let current = handle.snapshot().await.map_err(|e| e.to_string())?;
            if current
                .document
                .plugins
                .get(&name)
                .is_none_or(|p| !p.enabled || p.version != version)
            {
                return Ok(());
            }
            if current.plugin_job.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let consume = json!({"name":name,"version":version,"enabled":false,"pending_enable":false,"enable_error":"activation interrupted; enable again to retry"});
        handle
            .complete_domain(Domain::Plugins, json!({"runtime":consume}))
            .await
            .map_err(|e| e.to_string())?;
        // Startup activation uses the same reservation, durable receipt and job
        // supervisor as an explicit enable. It must not create an orphan hook.
        let mut request = CommandRequest::new(Command::SetPluginEnabled {
            name: name.clone(),
            enabled: true,
            version: Some(version.clone()),
        });
        request.confirm = true;
        let output = handle.submit_with_result(request, Origin::Migration).await;
        let error = match output {
            Ok(submission) => match submission.apply["job"].as_str() {
                Some(job) => jobs::wait_result(&handle, job)
                    .await
                    .err()
                    .map(|e| e.to_string()),
                None => None,
            },
            Err(error) => Some(error.to_string()),
        };
        let update = json!({"name":name,"version":version,"enabled":error.is_none(),"pending_enable":false,"enable_error":error});
        handle
            .complete_domain(Domain::Plugins, json!({"runtime":update}))
            .await
            .map_err(|e| e.to_string())?;
        if let Some(error) = error {
            return Err(error);
        }
    }
    if manifest.process.is_none() {
        return Ok(());
    }
    let current = handle.snapshot().await.map_err(|e| e.to_string())?;
    if current
        .document
        .plugins
        .get(&name)
        .is_none_or(|p| !p.enabled || p.version != version)
    {
        return Ok(());
    }
    if let Err(error) = native().launch(handle.clone(), manifest).await {
        let _ = runtime_observation(&handle, &name, &version, Some(error.clone())).await;
        let _ = privileged(
            handle.applier.as_ref(),
            Request::PluginRuntimeStop {
                name: manifest_name(&name),
            },
        )
        .await;
        return Err(error);
    }
    runtime_observation(&handle, &name, &version, None).await?;
    Ok(())
}
async fn runtime_observation(
    handle: &StateHandle,
    name: &str,
    version: &str,
    error: Option<String>,
) -> Result<(), String> {
    let state = handle.snapshot().await.map_err(|e| e.to_string())?;
    let mut update = state
        .plugin_runtime
        .get("plugins")
        .and_then(|p| p.get(name))
        .cloned()
        .unwrap_or_else(|| json!({}));
    update["name"] = json!(name);
    update["version"] = json!(version);
    update["runtime_error"] = json!(error);
    handle
        .complete_domain(Domain::Plugins, json!({"runtime":update}))
        .await
        .map_err(|e| e.to_string())
}
fn manifest_name(name: &str) -> ndimon_privd_proto::PluginName {
    name.to_owned().try_into().expect("validated plugin name")
}
