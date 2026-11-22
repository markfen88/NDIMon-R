//! Legacy tessera import and projection; ownership remains table driven.
use crate::{LegacyFiles, PathSpec};
use ndimon_types::*;

/// The vendor token of the native device role: the compatibility runtime's
/// `--vendor` (`/usr/lib/tessera/controller.args`), so controllers see the same
/// device after the switch.
pub const NATIVE_VENDOR: &str = "Audinate";
/// Generated systemd drop-ins whose change needs `daemon-reload` and a restart
/// of `tesserad` (the role is chosen when the daemon starts).
pub const NATIVE_ROLE_PATHS: [&str; 2] = [
    "/etc/systemd/system/tesserad.service.d/71-ndimon-native-device.conf",
    "/etc/systemd/system/tessera-controller@main.service.d/71-ndimon-native-device.conf",
];
pub fn render_path(
    spec: &PathSpec,
    state: &DesiredState,
    _revision: u64,
    _files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    render_artifact(spec.path_id, state)
}

pub fn flows(state: &DesiredState) -> Result<String, String> {
    String::from_utf8(render_artifact("tessera-flows", state)?).map_err(|e| e.to_string())
}
pub fn include(_spec: &PathSpec, state: &DesiredState) -> bool {
    state.tessera.is_some() && state.plugins.get("tessera").is_some_and(|p| p.installed)
}

pub(crate) fn render_artifact(id: &str, state: &DesiredState) -> Result<Vec<u8>, String> {
    let t = state.tessera.as_ref().ok_or("Tessera is not configured")?;
    let q = |s: &str| serde_json::to_string(s).map_err(|e| e.to_string());
    let text = match id {
        "tessera-device" => format!(
            "[[device]]\nid = \"main\"\nname = {}\nsample_rate = {}\nencoding = {}\nlatency_us = {}\nrx_channels = {}\ntx_channels = {}\nlocked = [\"name\", \"sample_rate\", \"encoding\", \"latency\", \"rx_channels\", \"tx_channels\", \"rx_channel_labels\", \"tx_channel_labels\"]\nrx_channel_labels = {}\ntx_channel_labels = {}\n\n[clock]\nrole = {}\npriority = {}\nlocked = [\"role\", \"priority\"]\n{}\n[discovery]\nadvertise_devices = {}\n{}",
            q(&t.device.name)?,
            t.device.sample_rate,
            t.device.encoding,
            t.device.latency_us,
            t.device.rx_channels,
            t.device.tx_channels,
            labels(&t.device.rx_labels)?,
            labels(&t.device.tx_labels)?,
            serde_json::to_string(&t.clock.role).map_err(|e| e.to_string())?,
            t.clock.priority,
            // Flows are not locked: Tessera's controller compatibility runtime
            // exits while `flows` is locked, and controller routing (Dante
            // Controller, Tessera portals, this plugin) installs its flows as
            // controller flows. Static flows stay host configuration.
            if t.routing && !t.routing_sources.is_empty() {
                format!(
                    "\n[control]\nrouting_sources = {}\n",
                    serde_json::to_string(&t.routing_sources).map_err(|e| e.to_string())?
                )
            } else {
                String::new()
            },
            t.device.advertise_devices,
            if t.routing {
                "browse_devices = true\n"
            } else {
                ""
            }
        ),
        "tessera-flows" => {
            let mut out =
                String::from("# NDIMon-R Tessera plugin; static flows, all device channels\n");
            for (i, f) in t.flows.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                out.push_str(&format!(
                    "[[flow]]\nid = {}\ndirection = {}\ndevice = \"main\"\nchannels = []\n",
                    q(&f.id)?,
                    serde_json::to_string(&f.direction).map_err(|e| e.to_string())?
                ));
                if f.any_source {
                    out.push_str("any_source = true\n");
                } else {
                    out.push_str(&format!(
                        "peer = {}\n",
                        q(f.peer.as_deref().ok_or("flow peer missing")?)?
                    ));
                }
                out.push_str(&format!("port = {}\n", f.port));
                if let Some(n) = f.frames_per_packet {
                    out.push_str(&format!("frames_per_packet = {n}\n"));
                }
            }
            if t.flows.is_empty() {
                out.push('\n');
            }
            out
        }
        "tessera-probe-flows" => String::new(),
        // Tessera 0.5: the daemon serves the device role itself (controllers,
        // flow requests, routing of this device's own inputs) in place of the
        // Controller compatibility runtime, which cannot execute receiver
        // routing. Both bind the same ports, so the runtime is kept from
        // starting while the role is on. Older daemons keep the runtime.
        "tessera-native-device" => {
            if t.routing {
                format!(
                    "# NDIMon-R Tessera plugin; native device role (Tessera 0.5)\n[Service]\nEnvironment=TESSERA_NATIVE_DEVICE=true\nEnvironment=TESSERA_NATIVE_VENDOR={NATIVE_VENDOR}\n"
                )
            } else {
                "# NDIMon-R Tessera plugin; this runtime uses the Controller compatibility runtime\n".into()
            }
        }
        "tessera-controller-native" => {
            if t.routing {
                "# NDIMon-R Tessera plugin; tesserad serves the device role itself\n[Unit]\nConditionPathExists=/run/ndimon/tessera-controller-compatibility\n".into()
            } else {
                "# NDIMon-R Tessera plugin; the Controller compatibility runtime serves the device role\n".into()
            }
        }
        "tessera-network" => {
            let row = state
                .network
                .networks
                .iter()
                .find(|n| n.id == state.network.roles.audio)
                .ok_or("audio network role is not adopted")?;
            let interface = row
                .vlan_id
                .map(|tag| format!("{}.{tag}", state.network.interface))
                .unwrap_or_else(|| state.network.interface.clone());
            format!(
                "[network]\ninterfaces = [{}]\nlocked = true\n",
                q(&interface)?
            )
        }
        "alsa" => format!(
            "# NDIMon-R Tessera plugin\npcm.ndimon_net_in {{ type tessera device \"main\" first_channel 1 channels {} client \"ndimon-r\" }}\npcm.ndimon_net_out {{ type tessera device \"main\" first_channel 1 channels {} client \"ndimon-r\" }}\n",
            t.device.rx_channels, t.device.tx_channels
        ),
        "alsa-ranges" => {
            let mut ranges = std::collections::BTreeSet::new();
            for s in state.audio.sources.values() {
                if s.enabled && s.resource == "tessera:main:rx" {
                    ranges.insert(("rx", s.channel_start, s.channels));
                }
            }
            for s in state.audio.sinks.values() {
                if s.enabled && s.resource == "tessera:main:tx" {
                    ranges.insert(("tx", s.channel_start, s.channels));
                }
            }
            let mut out = String::from(
                "# NDIMon-R Tessera endpoint ranges; generated from the audio graph\n",
            );
            for (direction, start, count) in ranges {
                let endpoint = TesseraEndpoint::new(
                    if direction == "rx" {
                        Direction::Rx
                    } else {
                        Direction::Tx
                    },
                    start,
                    count,
                );
                out.push_str(&format!("pcm.{} {{ type tessera device \"main\" first_channel {start} channels {count} client \"ndimon-r\" }}\n",endpoint.pcm()));
            }
            out
        }
        _ => return Err("unknown Tessera artifact".into()),
    };
    Ok(text.into_bytes())
}

fn labels(labels: &std::collections::BTreeMap<u16, String>) -> Result<String, String> {
    let entries = labels
        .iter()
        .map(|(channel, label)| {
            serde_json::to_string(label)
                .map(|s| format!("\"{channel}\" = {s}"))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("{{ {} }}", entries.join(", ")))
}
