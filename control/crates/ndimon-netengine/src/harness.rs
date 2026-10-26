//! Non-shipped nspawn fixture adapter; enabled only by the harness feature.
use crate::store::Root;
use crate::{CommandOutput, CommandRunner, CommandSpec, Processes, Result, Tool};
use ndimon_types::{ApiError, Domain};
use serde::{Deserialize, Serialize};
fn error(reason: &str) -> ApiError {
    ApiError::ApplyFailed {
        domain: Domain::Network,
        step: "harness".into(),
        reason: reason.into(),
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Fault {
    tool: String,
    arg: String,
    remaining: u32,
}
#[derive(Debug)]
pub struct ContainerProcesses;
impl CommandRunner for ContainerProcesses {
    fn run(&self, c: &CommandSpec) -> Result<CommandOutput> {
        let root = Root::production();
        if let Some(text) = root.text("/run/ndimon-harness-fault.json")? {
            let mut f: Fault = serde_json::from_str(&text).map_err(|_| error("fault_format"))?;
            if f.remaining > 0 && f.tool == format!("{:?}", c.tool) && c.args.contains(&f.arg) {
                f.remaining -= 1;
                root.replace(
                    "/run/ndimon-harness-fault.json",
                    Some(&Root::image(
                        &serde_json::to_string(&f).map_err(|_| error("fault_format"))?,
                        0o600,
                    )),
                )?;
                return Err(error("injected_activation_failure"));
            }
        }
        // nspawn's Ethernet test link is a veth, not physical hardware. Normalize
        // ONLY that one link-kind probe; all addresses/backend commands are real.
        // The outer harness preloads 8021q; a container must not load host modules.
        if c.tool == Tool::Modprobe && c.args == ["8021q"] {
            return Ok(CommandOutput {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            });
        }
        let mut out = Processes.run(c)?;
        if out.code != 0 {
            eprintln!(
                "BACKEND {:?} {:?}: {}",
                c.tool,
                c.args,
                out.stderr.chars().take(1500).collect::<String>()
            );
        }
        if c.tool == Tool::Ip
            && c.args.iter().any(|a| a == "link")
            && c.args.iter().any(|a| a == "show")
            && out.code == 0
        {
            let mut v: serde_json::Value =
                serde_json::from_str(&out.stdout).map_err(|_| error("ip_format"))?;
            for link in v.as_array_mut().ok_or_else(|| error("ip_format"))? {
                if link["ifname"] == "end0" && link["linkinfo"]["info_kind"] == "veth" {
                    link.as_object_mut()
                        .ok_or_else(|| error("ip_format"))?
                        .remove("linkinfo");
                }
            }
            out.stdout = serde_json::to_string(&v).map_err(|_| error("ip_format"))?;
        }
        Ok(out)
    }
}
