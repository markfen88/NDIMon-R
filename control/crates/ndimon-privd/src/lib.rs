//! Privileged action handlers. Tests inject a filesystem root and a runner.
pub mod artifacts;
pub mod cpu;
pub mod display;
pub mod network;
pub mod plugins;
pub mod policy;
pub mod process;
pub mod server;
pub mod store;
pub mod system;
pub mod tessera;
pub mod update;
use ndimon_privd_proto::{ActionOutput, Error, Request};
use process::{SharedRunner, args};
use std::sync::Arc;
#[derive(Debug, Clone)]
pub struct Host {
    pub root: store::Root,
    pub runner: SharedRunner,
    pub ndimon_gid: u32,
    pub renderer: Arc<dyn artifacts::Renderer>,
}
impl Host {
    pub fn production() -> Result<Self, Error> {
        let group = nix::unistd::Group::from_name("ndimon")
            .map_err(|e| Error::failed("ndimon group", e))?
            .ok_or_else(|| Error::failed("ndimon group", "missing"))?;
        Ok(Self {
            root: store::Root::production(),
            runner: Arc::new(process::Processes),
            ndimon_gid: group.gid.as_raw(),
            renderer: Arc::new(artifacts::StateRenderer),
        })
    }
    /// An exhaustive match is the coverage gate: adding a request requires a
    /// handler here, in addition to a payload-free audit label in the protocol.
    pub fn handle(&self, request: Request) -> Result<ActionOutput, Error> {
        request.validate()?;
        use Request::*;
        match request {
            WriteLegacyConfig { file, json } => {
                artifacts::legacy_json(self, file, &json)?;
                Ok(ActionOutput::default())
            }
            WriteLegacyLogo { file, base64 } => {
                artifacts::legacy_logo(self, file, &base64)?;
                Ok(ActionOutput::default())
            }
            RemoveLogo { file } => {
                display::guard_legacy(self)?;
                self.root.remove(&format!("/etc/{}", file.filename()))?;
                Ok(ActionOutput::default())
            }
            RenderArtifacts { revision } => {
                let data = artifacts::render(self, revision)?;
                Ok(ActionOutput {
                    data: Some(data),
                    ..ActionOutput::default()
                })
            }
            ServiceControl { unit, action } => {
                if unit == ndimon_privd_proto::ManagedUnit::Control {
                    // Let the HTTP caller receive its acknowledgement before
                    // systemd stops this request's control process.
                    system::systemctl(self, &["--no-block", action.as_str(), unit.as_str()])
                } else {
                    system::systemctl(self, &[action.as_str(), unit.as_str()])
                }
            }
            RestartFinder => system::systemctl(self, &["restart", "ndimon-finder.service"]),
            RestartStack => system::systemctl(
                self,
                &[
                    "--no-block",
                    "restart",
                    "ndimon-r.service",
                    "ndimon-finder.service",
                    "ndimon-control.service",
                ],
            ),
            StopCore => {
                system::systemctl(self, &["stop", "ndimon-r.service", "ndimon-finder.service"])
            }
            StartCore => system::systemctl(
                self,
                &["start", "ndimon-finder.service", "ndimon-r.service"],
            ),
            Reboot => self.runner.run("/sbin/reboot", &[], None),
            SetHostname { name } => self.runner.run(
                "/usr/bin/hostnamectl",
                &args(&["set-hostname", name.as_str()]),
                None,
            ),
            SetTimeSync { server } => system::ntp(self, server.as_ref()),
            SetRebootSchedule { schedule } => system::reboot_schedule(self, &schedule),
            StartUpdate => Err(Error::Unsupported {
                what: "unsigned updates are disabled; install a signed release".into(),
            }),
            req @ InstallUpdate { .. } => self.start_job(req),
            CheckUpdate => update::check(self),
            CpuTuning { profile } => cpu::tune(self, profile),
            req @ NetworkNative { .. } => network::handle(self, req),
            req @ (NetworkStatus
            | NetworkCheckAddress { .. }
            | NetworkApply { .. }
            | NetworkConfirm { .. }
            | NetworkRollback
            | NetworkVlanList
            | NetworkVlanApply { .. }
            | NetworkVlanRemove { .. }
            | NetworkProfilesStatus
            | NetworkProfilesPreview { .. }
            | NetworkProfilesStage { .. }
            | NetworkProfilesConfirm { .. }
            | NetworkProfilesRevert { .. }
            | NetworkProfilesRollback
            | NetworkProfilesBoot
            | NetworkProfilesSyncTessera) => network::handle(self, req),
            req @ PluginInspect { .. }
            | req @ PluginInstall { .. }
            | req @ PluginUninstall { .. }
            | req @ PluginExec { .. }
            | req @ TesseraBundle { .. }
            | req @ TesseraEnable
            | req @ TesseraDisable
            | req @ TesseraWriteFlows { .. } => self.start_job(req),
            PluginRuntimeStart { name } => plugins::runtime::start(self, &name),
            PluginRuntimeStop { name } => plugins::runtime::stop(self, &name),
            PluginRuntimeStatus { name } => plugins::runtime::status(self, &name),
            PluginResetState => plugins::reset_state(self),
            PluginServiceControl { unit, action } => plugins::service(self, &unit, action),
            PluginJobStatus { job } => plugins::job_status(self, &job),
            PluginJobCancel { job } => plugins::job_cancel(self, &job),
            TesseraWriteAlsa { rx, tx } => {
                plugins::with_lock(self, || tessera::write_alsa(self, rx, tx))
            }
            TesseraWritePcms { ranges } => {
                plugins::with_lock(self, || tessera::write_pcms(self, &ranges))
            }
            TesseraService { action } => {
                system::systemctl(self, &[action.as_str(), "tesserad.service"])
            }
        }
    }
    fn start_job(&self, request: Request) -> Result<ActionOutput, Error> {
        let name = match &request {
            Request::PluginInspect { name, .. }
            | Request::PluginInstall { name, .. }
            | Request::PluginUninstall { name, .. }
            | Request::PluginExec { name, .. } => name.as_str(),
            Request::TesseraBundle { .. }
            | Request::TesseraEnable
            | Request::TesseraDisable
            | Request::TesseraWriteFlows { .. } => "tessera",
            Request::InstallUpdate { .. } => "update",
            _ => return Err(Error::invalid("job", "request is not a long job")),
        };
        let id = store::next_id();
        let unit = format!("ndimon-plugin-{name}-{id}.service");
        let path = format!("/run/ndimon/privd-jobs/{id}.json");
        let jobs = self.root.directory("/run/ndimon/privd-jobs", true, true)?;
        use std::os::unix::fs::PermissionsExt;
        jobs.0
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(store::io_error)?;
        self.root.atomic(
            &path,
            &serde_json::to_vec(&request).map_err(|e| Error::failed("job encode", e))?,
            nix::unistd::getegid().as_raw(),
            0o600,
        )?;
        let mut argv = args(&["--quiet", "--collect"]);
        argv.push(format!("--unit={unit}"));
        let signed_update = matches!(request, Request::InstallUpdate { .. });
        let executable = if signed_update {
            std::env::current_exe()
                .map_err(store::io_error)?
                .to_string_lossy()
                .into_owned()
        } else {
            "/usr/libexec/ndimon/ndimon-privd".into()
        };
        argv.extend(args(&[
            if signed_update {
                "--property=RuntimeMaxSec=1800"
            } else {
                "--property=RuntimeMaxSec=600"
            },
            "--property=TimeoutStopSec=30",
            "--property=ProtectSystem=no",
            "--",
            &executable,
            "--job",
            &id.to_string(),
        ]));
        let result = self.runner.run("/usr/bin/systemd-run", &argv, None);
        let marker = format!("NDIMON_PLUGIN_JOB={unit}\n");
        match result {
            Ok(mut output) => {
                output.stdout = marker + &output.stdout;
                output.job = Some(unit.try_into().map_err(|e| Error::invalid("job", e))?);
                Ok(output)
            }
            Err(Error::Failed {
                step,
                reason,
                exit_code,
            }) => Err(Error::Failed {
                step,
                reason: marker + &reason,
                exit_code,
            }),
            Err(e) => Err(e),
        }
    }
    /// Only root can enter this mode; its numeric argument identifies a
    /// root-owned private request snapshot, never a supplied pathname.
    pub fn run_job(&self, id: u64) -> Result<ActionOutput, Error> {
        let path = format!("/run/ndimon/privd-jobs/{id}.json");
        let (dir, name) = self.root.parent(&path, false, true)?;
        let file = dir.open(&name)?;
        use std::os::unix::fs::MetadataExt;
        let info = file.metadata().map_err(store::io_error)?;
        if info.uid() != nix::unistd::geteuid().as_raw() || info.mode() & 0o077 != 0 {
            return Err(Error::PermissionDenied);
        }
        let bytes = dir
            .read(&name, ndimon_privd_proto::MAX_FRAME_BYTES)?
            .ok_or(Error::PermissionDenied)?;
        let req: Request =
            serde_json::from_slice(&bytes).map_err(|e| Error::invalid("job", e.to_string()))?;
        let result = match req {
            Request::InstallUpdate { version, force } => update::install(self, &version, force, id),
            Request::PluginInspect {
                name,
                archive,
                expected_version,
            } => plugins::admit(self, &name, &archive, expected_version.as_ref()),
            Request::TesseraWriteFlows { flows } => {
                plugins::with_lock(self, || tessera::write_flows(self, &flows))
            }
            Request::PluginInstall {
                name,
                archive,
                expected_version,
                force,
            } => plugins::install(self, &name, &archive, expected_version.as_ref(), force),
            Request::PluginUninstall { name, mode } => plugins::uninstall(self, &name, mode),
            Request::PluginExec { name, command } => plugins::execute(self, &name, &command),
            Request::TesseraBundle { action } => {
                plugins::with_lock(self, || tessera::bundle(self, &action))
            }
            Request::TesseraEnable => plugins::with_lock(self, || tessera::enable(self)),
            Request::TesseraDisable => plugins::with_lock(self, || tessera::disable(self)),
            _ => Err(Error::invalid("job", "not a worker request")),
        };
        let encoded = serde_json::to_vec(&match &result {
            Ok(out) => ndimon_privd_proto::ReplyResult::Ok(out.clone()),
            Err(err) => ndimon_privd_proto::ReplyResult::Err(err.clone()),
        })
        .map_err(|e| Error::failed("job result", e))?;
        dir.atomic(
            &format!("{id}.result.json"),
            &encoded,
            nix::unistd::geteuid().as_raw(),
            nix::unistd::getegid().as_raw(),
            0o600,
        )?;
        let _ = dir.remove(&name);
        result
    }
}
