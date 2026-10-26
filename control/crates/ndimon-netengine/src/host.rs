//! NE-4.15: a fixed executable set, argv only, bounded I/O, deterministic locale.
use crate::store::Root;
use crate::{MAX_PRIVATE, Result, failed, unsupported};
use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ip,
    Networkctl,
    Netplan,
    Nmcli,
    Ifup,
    Ifdown,
    Ifquery,
    Dhclient,
    Systemctl,
    SystemdRun,
    Nft,
    Arping,
    Modprobe,
}
impl Tool {
    pub fn program(self) -> &'static str {
        match self {
            Self::Ip => "/usr/sbin/ip",
            Self::Networkctl => "/usr/bin/networkctl",
            Self::Netplan => "/usr/sbin/netplan",
            Self::Nmcli => "/usr/bin/nmcli",
            Self::Ifup => "/usr/sbin/ifup",
            Self::Ifdown => "/usr/sbin/ifdown",
            Self::Ifquery => "/usr/sbin/ifquery",
            Self::Dhclient => "/usr/sbin/dhclient",
            Self::Systemctl => "/usr/bin/systemctl",
            Self::SystemdRun => "/usr/bin/systemd-run",
            Self::Nft => "/usr/sbin/nft",
            Self::Arping => "/usr/bin/arping",
            Self::Modprobe => "/usr/sbin/modprobe",
        }
    }
}
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub tool: Tool,
    pub args: Vec<String>,
    pub input: Option<Vec<u8>>,
    pub timeout_ms: u64,
    pub output_limit: usize,
}
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}
pub trait CommandRunner: std::fmt::Debug + Send + Sync {
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput>;
}
#[derive(Debug)]
pub struct Processes;
impl CommandRunner for Processes {
    fn run(&self, c: &CommandSpec) -> Result<CommandOutput> {
        if c.timeout_ms == 0
            || c.timeout_ms > 180_000
            || c.output_limit > MAX_PRIVATE
            || c.input.as_ref().is_some_and(|i| i.len() > MAX_PRIVATE)
        {
            return Err(failed("command", "invalid_limits"));
        }
        // GNU timeout owns the process group, including descendants. The apply
        // service also has KillMode=control-group and a 90-second runtime bound.
        let mut child = Command::new("/usr/bin/timeout")
            .args([
                "--kill-after=2s",
                &format!("{}s", c.timeout_ms as f64 / 1000.0),
                c.tool.program(),
            ])
            .args(&c.args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C")
            .stdin(if c.input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| unsupported("required_tool_missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| failed("command", "missing_output"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| failed("command", "missing_output"))?;
        let limit = c.output_limit;
        let out = std::thread::spawn(move || bounded(stdout, limit));
        let err = std::thread::spawn(move || bounded(stderr, limit));
        let mut input_failed = false;
        if let Some(bytes) = &c.input {
            if let Some(mut stdin) = child.stdin.take() {
                input_failed = stdin.write_all(bytes).is_err();
            } else {
                input_failed = true;
            }
        }
        let status = child.wait().map_err(|_| failed("command", "wait_failed"))?;
        let stdout = out
            .join()
            .map_err(|_| failed("command", "output_failed"))??;
        let stderr = err
            .join()
            .map_err(|_| failed("command", "output_failed"))??;
        if input_failed {
            return Err(failed("command", "input_failed"));
        }
        let code = status.code().unwrap_or(128);
        if matches!(code, 124 | 137) {
            return Err(failed("command", "timeout"));
        }
        if matches!(code, 126 | 127) {
            return Err(unsupported("required_tool_missing"));
        }
        Ok(CommandOutput {
            code,
            stdout: String::from_utf8(stdout).map_err(|_| failed("command", "invalid_output"))?,
            stderr: String::from_utf8(stderr).map_err(|_| failed("command", "invalid_output"))?,
        })
    }
}
fn bounded(mut stream: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0u8; 8192];
    let mut oversized = false;
    loop {
        let n = stream
            .read(&mut buffer)
            .map_err(|_| failed("command", "read_failed"))?;
        if n == 0 {
            break;
        }
        if bytes.len() + n > limit {
            oversized = true;
        }
        if !oversized {
            bytes.extend_from_slice(&buffer[..n]);
        }
    }
    if oversized {
        Err(failed("command", "output_too_large"))
    } else {
        Ok(bytes)
    }
}

pub trait Clock: std::fmt::Debug + Send + Sync {
    fn now_ms(&self) -> Result<u64>;
    fn boot_id(&self) -> Result<String>;
    fn sleep_ms(&self, ms: u64);
}
#[derive(Debug)]
pub struct LinuxClock;
impl Clock for LinuxClock {
    fn now_ms(&self) -> Result<u64> {
        let text = std::fs::read_to_string("/proc/uptime")
            .map_err(|_| failed("clock", "uptime_unavailable"))?;
        let value = text
            .split_whitespace()
            .next()
            .ok_or_else(|| failed("clock", "uptime_unavailable"))?;
        // /proc/uptime has centisecond precision; avoid a floating point journal.
        let (seconds, fraction) = value.split_once('.').unwrap_or((value, "0"));
        let seconds: u64 = seconds
            .parse()
            .map_err(|_| failed("clock", "invalid_uptime"))?;
        let fraction: u64 = format!("{fraction:0<3}")[..3]
            .parse()
            .map_err(|_| failed("clock", "invalid_uptime"))?;
        seconds
            .checked_mul(1000)
            .and_then(|v| v.checked_add(fraction))
            .ok_or_else(|| failed("clock", "invalid_uptime"))
    }
    fn boot_id(&self) -> Result<String> {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map(|v| v.trim().into())
            .map_err(|_| failed("clock", "boot_id_unavailable"))
    }
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}
#[derive(Debug, Clone)]
pub struct Host {
    pub root: Root,
    pub runner: Arc<dyn CommandRunner>,
    pub clock: Arc<dyn Clock>,
    pub(crate) deadline: Option<u64>,
    pub(crate) handoff: bool,
    pub(crate) handoff_paths: Vec<String>,
    pub(crate) boot_recovery: bool,
}
impl Default for Host {
    fn default() -> Self {
        Self {
            root: Root::production(),
            runner: Arc::new(Processes),
            clock: Arc::new(LinuxClock),
            deadline: None,
            handoff: false,
            handoff_paths: Vec::new(),
            boot_recovery: false,
        }
    }
}
impl Host {
    pub fn fixture(root: Root, runner: Arc<dyn CommandRunner>, clock: Arc<dyn Clock>) -> Self {
        Self {
            root,
            runner,
            clock,
            deadline: None,
            handoff: false,
            handoff_paths: Vec::new(),
            boot_recovery: false,
        }
    }
    pub(crate) fn budget(&self, ms: u64) -> Result<Self> {
        let mut h = self.clone();
        h.deadline = Some(self.clock.now_ms()?.saturating_add(ms));
        Ok(h)
    }
    pub(crate) fn output(
        &self,
        tool: Tool,
        args: &[String],
        timeout_ms: u64,
        input: Option<&str>,
    ) -> Result<CommandOutput> {
        let remaining = self
            .deadline
            .map(|d| self.clock.now_ms().map(|now| d.saturating_sub(now)))
            .transpose()?
            .unwrap_or(timeout_ms.saturating_add(2000));
        let timeout_ms = timeout_ms.min(remaining.saturating_sub(2000));
        if timeout_ms == 0 {
            return Err(failed("command", "apply_deadline"));
        }
        self.runner.run(&CommandSpec {
            tool,
            args: args.to_vec(),
            timeout_ms,
            output_limit: MAX_PRIVATE,
            input: input.map(|v| v.as_bytes().to_vec()),
        })
    }
    pub(crate) fn run(&self, tool: Tool, args: &[&str], seconds: u64) -> Result<String> {
        self.run_owned(
            tool,
            &args.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
            seconds,
            None,
        )
    }
    pub(crate) fn run_owned(
        &self,
        tool: Tool,
        args: &[String],
        seconds: u64,
        input: Option<&str>,
    ) -> Result<String> {
        let output = self.output(tool, args, seconds * 1000, input)?;
        if output.code != 0 {
            return Err(failed(
                "command",
                &format!("{:?}_exit_{}", tool, output.code).to_lowercase(),
            ));
        }
        Ok(output.stdout)
    }
    pub(crate) fn json(&self, tool: Tool, args: &[&str]) -> Result<Value> {
        serde_json::from_str(&self.run(tool, args, 20)?)
            .map_err(|_| failed("probe", "invalid_json"))
    }
    pub(crate) fn active(&self, unit: &str) -> Result<bool> {
        let args = ["is-active".into(), "--quiet".into(), unit.into()];
        match self.output(Tool::Systemctl, &args, 8000, None)?.code {
            0 => Ok(true),
            3 | 4 => Ok(false),
            _ => Err(failed("probe", "service_state")),
        }
    }
    /// Enumerate tables successfully before interpreting table absence. No
    /// localized stderr parsing and no failed probe treated as an empty ruleset.
    pub(crate) fn table(&self, name: &str) -> Result<Option<String>> {
        let tables = self.json(Tool::Nft, &["-j", "list", "tables"])?;
        let entries = tables["nftables"]
            .as_array()
            .ok_or_else(|| failed("probe", "invalid_nft_output"))?;
        if entries
            .iter()
            .any(|t| t["table"]["family"] == "inet" && t["table"]["name"] == name)
        {
            Ok(Some(self.run(
                Tool::Nft,
                &["list", "table", "inet", name],
                20,
            )?))
        } else {
            Ok(None)
        }
    }
    pub(crate) fn set_table(&self, text: Option<&str>) -> Result<()> {
        let mut script = String::new();
        if self.table("ndimon_management")?.is_some() {
            script.push_str("delete table inet ndimon_management\n");
        }
        if let Some(text) = text {
            script.push_str(text);
        }
        if !script.is_empty() {
            self.run_owned(Tool::Nft, &["-f".into(), "-".into()], 20, Some(&script))?;
        }
        Ok(())
    }
    pub(crate) fn ownership(&self) -> Result<()> {
        // Only an internal host clone holding BOTH writer locks may use this.
        // Its recoverable handoff journal owns the complete removal/import set.
        if self.handoff {
            return Ok(());
        }
        if self.root.exists(crate::handoff::EXTERNAL)?
            || self.root.exists(crate::handoff::QUIESCE)?
        {
            return Err(crate::locked("handoff"));
        }
        let received = crate::handoff::received(self)?;
        for marker in [
            "/var/lib/tessera-network/committed.json",
            "/var/lib/tessera-network/pending.json",
            "/etc/tessera/config.d/59-standalone-network.toml",
            "/etc/netplan/90-tessera-network.yaml",
        ] {
            if marker.ends_with("committed.json") && received {
                continue;
            }
            if self.root.exists(marker)? {
                return Err(crate::locked("tessera_standalone"));
            }
        }
        for path in self.root.list("/etc/systemd/network", Some(".network"))? {
            if path
                .rsplit('/')
                .next()
                .is_some_and(|p| p.starts_with("00-tessera-"))
            {
                return Err(crate::locked("tessera_standalone"));
            }
        }
        if self.table("tessera_network")?.is_some() {
            return Err(crate::locked("tessera_standalone"));
        }
        Ok(())
    }
}

pub use ndimon_types::NetworkSessionPath as SessionPath;

// The trusted shared context intentionally has no public deserializer. Only
// this private, root-owned journal adapter may reconstruct it after a restart.
pub(crate) mod session_journal {
    use super::SessionPath;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::net::IpAddr;
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Saved {
        peer: IpAddr,
        local: IpAddr,
    }
    pub fn serialize<S: Serializer>(
        value: &Option<SessionPath>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .map(|v| Saved {
                peer: v.peer,
                local: v.local,
            })
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SessionPath>, D::Error> {
        Ok(Option::<Saved>::deserialize(deserializer)?.map(|v| SessionPath::new(v.peer, v.local)))
    }
}
