//! Non-default smoke fixture. Runs without root, performs real renders under a
//! private temp root, and simulates follower IPC and managed restarts.
#[path = "fixture/network.rs"]
mod network;
use ndimon_privd::{Host, artifacts::StateRenderer, process::Runner, store::Root};
use ndimon_privd_proto::{
    ActionOutput, Envelope, Error, ManagedUnit, Reply, ReplyResult, Request, UnitAction, VERSION,
    codec,
};
use serde_json::{Value, json};
use std::{
    fs, io,
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

#[derive(Debug)]
struct NoProcesses {
    root: PathBuf,
}
impl Runner for NoProcesses {
    fn run(&self, program: &str, args: &[String], _: Option<&[u8]>) -> Result<ActionOutput, Error> {
        if program == "/usr/bin/hostnamectl" && args.len() == 2 && args[0] == "set-hostname" {
            fs::write(
                self.root.join("hostname-request.json"),
                json!({"program":program,"args":args}).to_string(),
            )
            .map_err(|e| Error::failed("fixture hostname audit", e))?;
            println!("simulated hostnamectl {}", args[1]);
            return Ok(ActionOutput::default());
        }
        if program == "/usr/bin/tesseractl"
            && args.len() == 4
            && args[..3]
                .iter()
                .map(String::as_str)
                .eq(["config", "check", "--config-dir"])
        {
            let stage = PathBuf::from(&args[3])
                .canonicalize()
                .map_err(|e| Error::failed("fixture stage", e))?;
            if !stage.starts_with(self.root.join("run")) {
                return Err(Error::PermissionDenied);
            }
            let flows = fs::read(stage.join("config.d/61-ndimon-flows.toml"))
                .map_err(|e| Error::failed("fixture flows", e))?;
            fs::write(self.root.join("tessera-checked.toml"), flows)
                .map_err(|e| Error::failed("fixture checker audit", e))?;
            println!("simulated Tessera candidate checker (temporary staged config)");
            return Ok(ActionOutput::default());
        }
        Err(Error::PermissionDenied)
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: ndimon-p2-fixture PRIVATE_TEMP_ROOT")?,
    )
    .canonicalize()?;
    let metadata = fs::metadata(&root)?;
    if nix::unistd::geteuid().is_root()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
        || root == std::path::Path::new("/")
    {
        return Err("fixture requires a caller-owned private temp root and non-root uid".into());
    }
    for name in ["etc", "run", "var/lib/ndimon/state"] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(root.join(name))?;
    }
    let host = Arc::new(Host {
        root: Root(root.clone()),
        runner: Arc::new(NoProcesses { root: root.clone() }),
        ndimon_gid: nix::unistd::getegid().as_raw(),
        renderer: Arc::new(StateRenderer),
    });
    let decoder = UnixListener::bind(root.join("decoder.sock"))?;
    let privd = UnixListener::bind(root.join("privd.sock"))?;
    let finder = UnixListener::bind(root.join("finder.sock"))?;
    let tessera = UnixListener::bind(root.join("control.sock"))?;
    let decoder_state = Arc::new(Mutex::new(FollowerState::default()));
    let finder_state = Arc::new(Mutex::new(FollowerState::default()));
    let network = Arc::new(Mutex::new(network::Engine::new(root.clone())));
    println!("fixture ready (non-root; renderer real; hardware simulated)");
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            result = decoder.accept() => {
                let (socket, _) = result?;
                let root = root.clone();
                let state = decoder_state.clone();
                tasks.spawn(async move { fake_follower(socket, root, state, false).await });
            }
            result = finder.accept() => {
                let (socket, _) = result?;
                let root = root.clone();
                let state = finder_state.clone();
                tasks.spawn(async move { fake_follower(socket, root, state, true).await });
            }
            result = tessera.accept() => {
                let (socket, _) = result?;
                let root = root.clone();
                tasks.spawn(async move { fake_tessera(socket, root).await });
            }
            result = privd.accept() => {
                let (socket, _) = result?;
                let host = host.clone();
                let network = network.clone();
                tasks.spawn(async move { fake_privd(socket, host, network).await });
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Ok(Err(error))) = result { eprintln!("fixture connection: {error}"); }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    tasks.abort_all();
    fs::remove_file(root.join("decoder.sock"))?;
    fs::remove_file(root.join("privd.sock"))?;
    fs::remove_file(root.join("finder.sock"))?;
    fs::remove_file(root.join("control.sock"))?;
    Ok(())
}
async fn fake_privd(
    mut socket: UnixStream,
    host: Arc<Host>,
    network: Arc<Mutex<network::Engine>>,
) -> io::Result<()> {
    let envelope: Envelope = codec::read_async(&mut socket).await?;
    let result = match envelope.req {
        Request::NetworkNative { call } => {
            let data = tokio::task::spawn_blocking(move || network.lock().unwrap().handle(call))
                .await
                .map_err(io::Error::other)?;
            Ok(ActionOutput {
                data: Some(data),
                ..Default::default()
            })
        }
        request @ (Request::PluginInspect { .. }
        | Request::PluginInstall { .. }
        | Request::PluginUninstall { .. }
        | Request::PluginExec { .. }
        | Request::PluginJobStatus { .. }
        | Request::PluginServiceControl { .. }) => {
            tokio::task::spawn_blocking(move || network.lock().unwrap().plugin(request))
                .await
                .map_err(io::Error::other)?
        }
        request @ (Request::RenderArtifacts { .. }
        | Request::SetHostname { .. }
        | Request::WriteLegacyConfig {
            file: ndimon_privd_proto::LegacyConfigFile::Auth,
            ..
        }) => tokio::task::spawn_blocking(move || host.handle(request))
            .await
            .map_err(io::Error::other)?,
        Request::ServiceControl {
            unit: unit @ (ManagedUnit::Control | ManagedUnit::Decoder | ManagedUnit::Finder),
            action: UnitAction::Restart,
        } => {
            println!("simulated {unit:?} restart");
            Ok(ActionOutput::default())
        }
        Request::SetRebootSchedule { .. } => {
            println!("simulated reboot timer configuration");
            Ok(ActionOutput::default())
        }
        _ => Err(Error::PermissionDenied),
    };
    codec::write_async(
        &mut socket,
        &Reply {
            v: VERSION,
            id: envelope.id,
            result: match result {
                Ok(out) => ReplyResult::Ok(out),
                Err(error) => ReplyResult::Err(error),
            },
        },
    )
    .await
}
#[derive(Debug, Default)]
struct FollowerState {
    revision: Option<u64>,
    config: Value,
}
async fn fake_follower(
    socket: UnixStream,
    root: PathBuf,
    state: Arc<Mutex<FollowerState>>,
    finder: bool,
) -> io::Result<()> {
    let mut socket = BufReader::new(socket);
    let mut line = String::new();
    socket.read_line(&mut line).await?;
    let command: Value = serde_json::from_str(&line)?;
    let action = command["action"].as_str().unwrap_or("");
    let mut audit = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(if finder {
            "finder-requests.jsonl"
        } else {
            "decoder-requests.jsonl"
        }))?;
    let mut audit_line = serde_json::to_vec(&command)?;
    audit_line.push(b'\n');
    audit.write_all(&audit_line)?;
    println!("{} {action}", if finder { "finder" } else { "decoder" });
    let audio: Value = fs::read(root.join("etc/ndimon-audio-settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(json!({}));
    let response = match action {
        "status" | "subscribe" => {
            let state = state.lock().unwrap();
            if finder {
                json!({"revision":state.revision,"list":{"NI10 source":"192.0.2.8:5961"}})
            } else {
                json!({"revision":state.revision,"connected":true,"source_name":"NI10 source","outputs":[{"output_index":0,"connected":true,"source_name":"NI10 source","output_alias":"Smoke display","fps":59.94}],"audio":{"running":true,"primary_output":1,"settings":audio}})
            }
        }
        "list" if finder => json!({"list":{"NI10 source":"192.0.2.8:5961"}}),
        "apply_config" => {
            let mut state = state.lock().unwrap();
            let revision = command["revision"].as_u64();
            let config = command["config"].as_object();
            if revision.is_none()
                || config.is_none_or(|c| {
                    c.keys().any(|key| {
                        if finder {
                            key != "ndi"
                        } else {
                            !["device", "outputs", "ndi", "display", "network"]
                                .contains(&key.as_str())
                        }
                    })
                })
            {
                json!({"ok":false,"errors":["invalid follower config"]})
            } else if state.revision > revision && command["reset"] != true {
                json!({"ok":false,"errors":["revision: older than last applied revision"]})
            } else {
                state.revision = revision;
                if !state.config.is_object() {
                    state.config = json!({});
                }
                for (key, value) in config.unwrap() {
                    state.config[key] = value.clone();
                }
                json!({"ok":true,"revision":revision,"applied":config.unwrap().keys().collect::<Vec<_>>(),"refused":[],"restart_required":false})
            }
        }
        "connect" => {
            let envelope: Value =
                serde_json::from_slice(&fs::read(root.join("var/lib/ndimon/state/state.json"))?)?;
            let id = command["output"]
                .as_u64()
                .ok_or_else(|| io::Error::other("connect output missing"))?
                + 1;
            let source = &envelope["document"]["outputs"][id.to_string()]["source"];
            if source["name"] != command["source_name"] {
                return Err(io::Error::other(
                    "source connect preceded native selection publication",
                ));
            }
            json!({"ok":true})
        }
        "get_status_all" => {
            json!([{"ch_num":1,"connected":true,"audio_available":true,"drm_ready":true,"output_alias":"Smoke display"}])
        }
        "reload_audio" => {
            json!({"ok":true,"applied":["audio"],"refused":[],"restart_required":false})
        }
        "check_audio" => json!({"ok":true,"video_delay_cap_ms":3166}),
        _ => json!({"ok":true}),
    };
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    socket.get_mut().write_all(&bytes).await?;
    if action == "subscribe" {
        std::future::pending::<()>().await;
    }
    Ok(())
}

async fn fake_tessera(mut socket: UnixStream, root: PathBuf) -> io::Result<()> {
    use ndimon_tessera::wire::{Command, Query, RequestBody};
    loop {
        let request: ndimon_tessera::wire::Request = match codec::read_async(&mut socket).await {
            Ok(request) => request,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        let state: Value =
            serde_json::from_slice(&fs::read(root.join("var/lib/ndimon/state/state.json"))?)?;
        let device = &state["document"]["tessera"]["device"];
        let channels = |field: &str| {
            (1..=device[field].as_u64().unwrap_or(2))
                .map(|n| json!({"channel":n,"label":format!("Channel {n}")}))
                .collect::<Vec<_>>()
        };
        let subscribed = matches!(request.body, RequestBody::SubscribeEvents);
        let result = match &request.body {
            RequestBody::Query(Query::Status) => {
                json!({"ok":{"status":{"version":"0.2.2","devices":[{"key":"main","name":device["name"],"sample_rate":48000,"encoding":24,"latency":5000,"rx_channels":channels("rx_channels"),"tx_channels":channels("tx_channels")}],"clock_settings":{"role":"auto","priority":128},"clocks":[]}}})
            }
            RequestBody::Query(Query::Metrics) => {
                // One daemon process: the control plane tells a restarted
                // daemon from its uptime and reloads it.
                static STARTED: std::sync::OnceLock<std::time::Instant> =
                    std::sync::OnceLock::new();
                let uptime = STARTED.get_or_init(std::time::Instant::now).elapsed();
                json!({"ok":{"metrics":{"daemon":{"uptime_ms":uptime.as_millis() as u64},"flows":[]}}})
            }
            RequestBody::SubscribeEvents => json!({"ok":"done"}),
            body @ (RequestBody::Command(Command::ReloadConfig)
            | RequestBody::ConfirmedCommand {
                command: Command::ReloadConfig,
                ..
            }) => {
                let rendered = fs::read(root.join("etc/tessera/config.d/61-ndimon-flows.toml"))?;
                let document: ndimon_types::DesiredState =
                    serde_json::from_value(state["document"].clone())?;
                if rendered
                    != ndimon_render::tessera::flows(&document)
                        .map_err(io::Error::other)?
                        .as_bytes()
                {
                    return Err(io::Error::other(
                        "Tessera reload preceded candidate publication",
                    ));
                }
                let mut audit = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("tessera-requests.jsonl"))?;
                writeln!(
                    audit,
                    "{}",
                    json!({"body":body,"flows":document.tessera.as_ref().map(|t| &t.flows)})
                )?;
                println!("typed Tessera ReloadConfig after rendered flow publication");
                if matches!(body, RequestBody::Command(_))
                    && document
                        .tessera
                        .as_ref()
                        .is_some_and(|t| !t.flows.is_empty())
                {
                    json!({"err":{"confirmation_required":{"impact":{"token":"ni4-flow-token","restart":{"scope":"device","reason":"flow configuration changed"}}}}})
                } else if matches!(body,RequestBody::ConfirmedCommand {expect:Some(token),..} if token != "ni4-flow-token")
                {
                    json!({"err":{"invalid":{"field":"expect","reason":"wrong fixture token"}}})
                } else {
                    json!({"ok":"done"})
                }
            }
            _ => json!({"err":{"unsupported":{"what":"fixture command"}}}),
        };
        codec::write_async(&mut socket, &json!({"v":1,"id":request.id,"result":result})).await?;
        if subscribed {
            std::future::pending::<()>().await;
        }
    }
}
