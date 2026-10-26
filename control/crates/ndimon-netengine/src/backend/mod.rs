//! Strict adoption and deterministic plans. Never infer ownership from a prefix.
mod ifupdown;
pub(crate) mod netplan;
pub(crate) mod networkd;
mod nm;
use crate::host::{Host, Tool};
use crate::store::{FileChange, Root};
use crate::validation::{assigned, ifname, physical, subnet};
use crate::{BANNER, BINDING, Result, TESSERA_BINDING, failed, unsupported};
use ndimon_types::{AddressMode, ApiError, NetworkBackend, NetworkProfile, NetworkProfiles};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

pub const MEDIA: &[&str] = &[
    "tesserad.service",
    "ndimon-finder.service",
    "ndimon-r.service",
];
pub const ROOTS: &[&str] = &[
    "/usr/lib/systemd/network",
    "/usr/local/lib/systemd/network",
    "/run/systemd/network",
    "/etc/systemd/network",
];
pub use ndimon_types::{Drift as Foreign, NetworkBackendMetadata as Metadata};

/// Refusal codes that describe the content of a configuration file. Adoption
/// appends the key and file to these (NE-2.12a) so an operator can find them.
const CONTENT_REFUSALS: &[&str] = &[
    "unrepresentable_configuration",
    "custom_routes",
    "custom_hooks",
    "mixed_dhcp_static",
    "multiple_gateways",
    "ipv6_configuration",
    "policy_routing",
];
/// `unrepresentable_configuration` naming the offending key.
pub(crate) fn unrepresentable(key: impl std::fmt::Display) -> ApiError {
    unsupported(&format!("unrepresentable_configuration: {key}"))
}
/// Name the source of a content refusal: `code: key in file`. Other errors pass
/// through, so classified reasons (`unknown_backend`, I/O faults) stay exact.
pub(crate) fn located(error: ApiError, source: &str) -> ApiError {
    match error {
        ApiError::Unsupported { what } => {
            let (code, key) = what.split_once(": ").unwrap_or((what.as_str(), ""));
            if !CONTENT_REFUSALS.contains(&code) {
                return ApiError::Unsupported { what };
            }
            unsupported(&if key.is_empty() {
                format!("{code}: {source}")
            } else {
                format!("{code}: {key} in {source}")
            })
        }
        other => other,
    }
}
/// NE-2.12a: import one backend search list. A list or entry the row cannot hold
/// (a routing-only `~domain`, an IDN, more than six) is refused, never trimmed.
pub(crate) fn search_list(values: Vec<String>, key: &str) -> Result<Vec<String>> {
    if values.len() > ndimon_types::MAX_DNS_SEARCH
        || values
            .iter()
            .any(|d| !ndimon_types::valid_dns_search_domain(d))
    {
        return Err(unrepresentable(key));
    }
    Ok(values)
}
/// The before-image of one file that holds a Netplan-owned NetworkManager
/// profile. `nmcli connection modify` makes NetworkManager (and its Netplan
/// plugin) move, rewrite or add such files, so Revert puts the bytes back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NmFile {
    pub path: String,
    pub before: Option<crate::store::Image>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NmChange {
    pub uuid: String,
    pub interface: String,
    pub tag: Option<u16>,
    pub before: Option<BTreeMap<String, String>>,
    pub active: bool,
    pub after: BTreeMap<String, String>,
    pub remove_after_commit: bool,
    /// Persisted profile files to restore exactly; empty unless Netplan owns it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<NmFile>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub files: Vec<FileChange>,
    pub nm: Vec<NmChange>,
    pub previous: NetworkProfiles,
    pub candidate: NetworkProfiles,
    pub previous_metadata: Metadata,
    pub metadata: Metadata,
    pub old_table: Option<String>,
    pub active_services: Vec<String>,
    pub affected_services: Vec<String>,
    pub portal_port: u16,
    pub drift: Vec<Foreign>,
}
impl Plan {
    pub(crate) fn transitional_metadata(&self) -> Metadata {
        let mut metadata = self.metadata.clone();
        // Removed NM UUIDs remain owned until post-commit pruning. Candidate
        // metadata intentionally omits them from the next authoritative state.
        for (interface, uuid) in &self.previous_metadata.nm_owned {
            metadata
                .nm_owned
                .entry(interface.clone())
                .or_insert_with(|| uuid.clone());
        }
        metadata
    }
}

pub fn names(p: &NetworkProfiles) -> Vec<String> {
    let mut out = vec![p.interface.clone()];
    out.extend(
        p.networks
            .iter()
            .filter(|n| n.vlan_id.is_some())
            .map(|n| ifname(p, n)),
    );
    out
}
fn rows(v: &Value) -> Result<&Vec<Value>> {
    v.as_array()
        .ok_or_else(|| failed("probe", "invalid_output"))
}
impl Host {
    pub fn adopt(&self, selected: Option<&str>) -> Result<(NetworkProfiles, Metadata)> {
        self.adopt_known(selected, &[])
    }
    /// The port first adoption would take: the selected one, else the single
    /// device that carries the IPv4 default route.
    fn management_port(&self, selected: Option<&str>, links: &Value) -> Result<String> {
        let routes = self.json(Tool::Ip, &["-j", "-4", "route", "show", "default"])?;
        let mut candidates: BTreeSet<String> = rows(&routes)?
            .iter()
            .filter_map(|r| r["dev"].as_str())
            .map(str::to_owned)
            .collect();
        if let Some(s) = selected {
            return Ok(s.to_owned());
        }
        if candidates.len() != 1 {
            return Err(unsupported(
                if rows(links)?.iter().any(|l| l["link_type"] == "ether") {
                    "ambiguous_management_port"
                } else {
                    "no_wired_interface"
                },
            ));
        }
        candidates
            .pop_first()
            .ok_or_else(|| unsupported("no_wired_interface"))
    }
    /// Read-only view of a host whose adoption was refused: the port and its
    /// global IPv4 addresses. `None` when not even the port can be determined.
    pub(crate) fn observe_port(
        &self,
        selected: Option<&str>,
    ) -> Option<(String, Vec<(Ipv4Addr, u8)>)> {
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"]).ok()?;
        let interface = self.management_port(selected, &links).ok()?;
        let live = self.json(Tool::Ip, &["-j", "-4", "address", "show"]).ok()?;
        let addresses = rows(&live)
            .ok()?
            .iter()
            .filter(|i| i["ifname"] == interface)
            .filter_map(|i| i["addr_info"].as_array())
            .flatten()
            .filter(|a| a["family"] == "inet" && a["scope"] == "global")
            .filter_map(|a| {
                Some((
                    a["local"].as_str()?.parse().ok()?,
                    u8::try_from(a["prefixlen"].as_u64()?).ok()?,
                ))
            })
            .collect();
        Some((interface, addresses))
    }
    pub(crate) fn adopt_known(
        &self,
        selected: Option<&str>,
        owned: &[String],
    ) -> Result<(NetworkProfiles, Metadata)> {
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        let interface = self.management_port(selected, &links)?;
        self.check_links(&interface, owned, &links)?;
        // Inspect addresses even for DHCP: discovery may not silently truncate.
        let addresses = self.json(Tool::Ip, &["-j", "-4", "address", "show"])?;
        let live = rows(&addresses)?
            .iter()
            .filter(|i| i["ifname"] == interface)
            .filter_map(|i| i["addr_info"].as_array())
            .flatten()
            .filter(|a| a["family"] == "inet" && a["scope"] == "global")
            .collect::<Vec<_>>();
        if live.len() > 1 {
            return Err(unsupported("multiple_addresses"));
        }
        if live.iter().any(|a| {
            a["prefixlen"]
                .as_u64()
                .is_none_or(|p| !(8..=30).contains(&p))
                || a["local"]
                    .as_str()
                    .and_then(|s| s.parse::<Ipv4Addr>().ok())
                    .is_none_or(|a| !crate::validation::unicast(a))
        }) {
            return Err(unsupported("unrepresentable_configuration"));
        }
        let nm_active = self.active("NetworkManager.service")?;
        let nd_active = self.active("systemd-networkd.service")?;
        let nm_uuid = if nm_active {
            let s = self.run(
                Tool::Nmcli,
                &["-g", "GENERAL.CON-UUID", "device", "show", &interface],
                20,
            )?;
            let s = s.trim();
            if s.is_empty() || s == "--" {
                None
            } else {
                Some(s.to_owned())
            }
        } else {
            None
        };
        let active = if nd_active {
            let s = self.run(
                Tool::Networkctl,
                &["status", "--no-pager", "--", &interface],
                8,
            )?;
            networkd::selected(&s).map(str::to_owned)
        } else {
            None
        };
        if nm_uuid.is_some() && active.is_some() {
            return Err(unsupported("ambiguous_backend"));
        }
        let (backend, row, metadata) = if let Some(uuid) = nm_uuid {
            let (row, meta) = self
                .adopt_nm(&interface, &uuid)
                .map_err(|e| located(e, &format!("NetworkManager connection {uuid}")))?;
            (NetworkBackend::Networkmanager, row, meta)
        } else if let Some(path) = active {
            if path.starts_with("/run/systemd/network/10-netplan-") {
                let (row, mut meta) = self.adopt_netplan(&interface, &links)?;
                meta.parent_source = path;
                (NetworkBackend::Netplan, row, meta)
            } else {
                let (row, meta) = self.adopt_networkd(&interface, &path)?;
                (NetworkBackend::Networkd, row, meta)
            }
        } else {
            let (row, meta) = self.adopt_ifupdown(&interface)?;
            (NetworkBackend::Ifupdown, row, meta)
        };
        let mut p = NetworkProfiles::default();
        p.interface = interface;
        p.backend = Some(backend);
        p.networks.push(row);
        p.roles.management = "untagged".into();
        p.roles.ndi = "untagged".into();
        p.roles.audio = "untagged".into();
        let p = crate::validation::validate(&p, &p)?;
        Ok((p, metadata))
    }
    /// Import ownership only from a validated previous document, never a name.
    pub(crate) fn imported_metadata(&self, p: &NetworkProfiles) -> Result<Metadata> {
        crate::validation::validate(p, p)?;
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        self.check_links(&p.interface, &names(p), &links)?;
        let name = ifname(p, assigned(p, &p.roles.management)?);
        match p.backend {
            Some(NetworkBackend::Networkd) => {
                let status =
                    self.run(Tool::Networkctl, &["status", "--no-pager", "--", &name], 8)?;
                let source = networkd::selected(&status)
                    .ok_or_else(|| unsupported("missing_active_configuration"))?;
                let mut m = self.import_networkd(p, source)?;
                let status = self.run(
                    Tool::Networkctl,
                    &["status", "--no-pager", "--", &p.interface],
                    8,
                )?;
                m.parent_source = networkd::selected(&status)
                    .ok_or_else(|| unsupported("missing_active_configuration"))?
                    .into();
                Ok(m)
            }
            Some(NetworkBackend::Netplan) => {
                let (_, mut m) = self.adopt_netplan(&p.interface, &links)?;
                let status = self.run(
                    Tool::Networkctl,
                    &["status", "--no-pager", "--", &p.interface],
                    8,
                )?;
                m.parent_source = networkd::selected(&status)
                    .ok_or_else(|| unsupported("missing_active_configuration"))?
                    .into();
                Ok(m)
            }
            Some(NetworkBackend::Networkmanager) => self.import_nm(p),
            Some(NetworkBackend::Ifupdown) => self.import_ifupdown(p),
            _ => Err(unsupported("unknown_backend")),
        }
    }
    pub(crate) fn check_links(&self, base: &str, owned: &[String], links: &Value) -> Result<()> {
        if !physical(base) {
            return Err(unsupported("wired_port_required"));
        }
        let link = rows(links)?
            .iter()
            .find(|l| l["ifname"] == base)
            .ok_or_else(|| unsupported("no_wired_interface"))?;
        // Kernel-owned sysfs links are expected here; configuration/journal
        // storage still uses Root's strict no-symlink traversal.
        if link["link_type"] != "ether"
            || self
                .root
                .path(&format!("/sys/class/net/{base}/wireless"))?
                .exists()
        {
            return Err(unsupported("wired_port_required"));
        }
        if link["linkinfo"]["info_kind"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        {
            return Err(unsupported("unsupported_link_kind"));
        }
        if !link["master"].is_null() {
            return Err(unsupported("attached_interfaces"));
        }
        let index = link["ifindex"]
            .as_u64()
            .ok_or_else(|| unsupported("wired_port_required"))?;
        for l in rows(links)? {
            if l["ifname"] == base {
                continue;
            }
            if l["link"] == base || l["link_index"].as_u64() == Some(index) || l["master"] == base {
                let name = l["ifname"].as_str().unwrap_or("");
                if !owned.iter().any(|s| s == name) {
                    return Err(unsupported(if l["linkinfo"]["info_kind"] == "vlan" {
                        "foreign_vlan"
                    } else {
                        "attached_interfaces"
                    }));
                }
                let expected_tag = name
                    .rsplit_once('.')
                    .and_then(|(_, t)| t.parse::<u64>().ok());
                if l["linkinfo"]["info_kind"] != "vlan"
                    || !(l["link"] == base || l["link_index"].as_u64() == Some(index))
                    || !l["link_netnsid"].is_null()
                    || expected_tag != l["linkinfo"]["info_data"]["id"].as_u64()
                {
                    return Err(unsupported("foreign_vlan"));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn plan(
        &self,
        previous: NetworkProfiles,
        candidate: NetworkProfiles,
        metadata: Metadata,
        portal_port: u16,
    ) -> Result<Plan> {
        self.ownership()?;
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        let owned = previous
            .networks
            .iter()
            .filter(|n| n.vlan_id.is_some())
            .map(|n| ifname(&previous, n))
            .collect::<Vec<_>>();
        self.check_links(&candidate.interface, &owned, &links)?;
        for n in &candidate.networks {
            let name = ifname(&candidate, n);
            if n.vlan_id.is_some()
                && !owned.contains(&name)
                && rows(&links)?.iter().any(|l| l["ifname"] == name)
            {
                return Err(unsupported("foreign_vlan"));
            }
        }
        self.run(Tool::Arping, &["-V"], 5)?;
        if !self
            .root
            .path("/sys/fs/cgroup/cgroup.controllers")?
            .is_file()
        {
            return Err(unsupported("guard_unavailable"));
        }
        // Production uses socket activation on port 80. Verify the actual
        // wildcard listener, rather than trusting a browser's Host header.
        let sockets = self.json(Tool::Ip, &["-j", "-4", "address", "show"])?;
        let _ = rows(&sockets)?;
        self.verify_listener(portal_port)?;
        let old_table = self.table("ndimon_management")?;
        let mut files = Vec::new();
        let mut nm = Vec::new();
        let drift = match candidate.backend {
            Some(NetworkBackend::Networkd) => {
                self.render_networkd(&previous, &candidate, &metadata, true, &mut files)?;
                self.networkd_drift(&candidate)?
            }
            Some(NetworkBackend::Netplan) => {
                self.render_netplan(&previous, &candidate, &metadata, &mut files)?;
                let mut drift = self.networkd_drift(&candidate)?;
                drift.extend(self.netplan_drift(&candidate)?);
                drift
            }
            Some(NetworkBackend::Networkmanager) => {
                nm = self.plan_nm(&previous, &candidate, &metadata)?;
                self.nm_drift(&candidate, &metadata)?
            }
            Some(NetworkBackend::Ifupdown) => {
                self.render_ifupdown(&candidate, &metadata, &mut files)?;
                self.ifupdown_drift(&candidate)?
            }
            _ => return Err(unsupported("unknown_backend")),
        };
        if drift.iter().any(|d| d.blocking) {
            return Err(unsupported("foreign_connection"));
        }
        if candidate.networks.iter().any(|n| n.vlan_id.is_some()) {
            add(
                &self.root,
                &mut files,
                "/etc/modules-load.d/ndimon-8021q.conf",
                Some("8021q\n"),
                0o644,
            )?;
        }
        for slice in ["ndimonndi.slice", "ndimontessera.slice"] {
            add(
                &self.root,
                &mut files,
                &format!("/etc/systemd/system/{slice}"),
                Some("[Unit]\nDescription=NDIMon network traffic boundary\n[Slice]\n"),
                0o644,
            )?;
        }
        add(
            &self.root,
            &mut files,
            BINDING,
            Some(
                &ndimon_render::network::bindings(&candidate)
                    .map_err(|_| unsupported("invalid_binding"))?,
            ),
            0o644,
        )?;
        for unit in ["ndimon-r", "ndimon-finder"] {
            add(
                &self.root,
                &mut files,
                &format!("/etc/systemd/system/{unit}.service.d/70-ndimon-network.conf"),
                Some(&dropin("ndimonndi.slice")),
                0o644,
            )?;
        }
        if self.root.exists("/etc/tessera/config.d")? {
            self.tessera_files(&candidate, &mut files)?;
        }
        // Banner is a planned object too; applying updates its content only
        // after its original before-image is durable.
        files.push(FileChange {
            path: BANNER.into(),
            before: self.root.snapshot(BANNER)?,
            after: Some(Root::image(
                "Pending network change — the new address will appear above\n",
                0o644,
            )),
        });
        let mut affected_services = Vec::new();
        let mut active_services = Vec::new();
        for unit in MEDIA {
            if self.active(unit)? {
                active_services.push((*unit).into());
            }
        }
        let affected = affected_roles(&previous, &candidate);
        // Nothing restarts for Tessera on a device that does not have it.
        if affected.contains(&"audio".into()) && self.root.exists("/etc/tessera/config.d")? {
            affected_services.push("tesserad.service".into());
        }
        if affected.contains(&"ndi".into()) {
            affected_services.extend(["ndimon-finder.service".into(), "ndimon-r.service".into()]);
        }
        // First adoption may only change service placement. A live consumer
        // must enter its persistent slice before candidate guards protect it.
        for change in files
            .iter()
            .filter(|c| c.path.ends_with("70-ndimon-network.conf"))
        {
            for unit in [
                "tesserad.service",
                "ndimon-finder.service",
                "ndimon-r.service",
            ] {
                if change.path.contains(&format!("/{unit}.d/"))
                    && !affected_services.iter().any(|s| s == unit)
                {
                    affected_services.push(unit.into());
                }
            }
        }
        Ok(Plan {
            files,
            nm,
            previous,
            candidate,
            previous_metadata: metadata.clone(),
            metadata,
            old_table,
            active_services,
            affected_services,
            portal_port,
            drift,
        })
    }
    fn verify_listener(&self, port: u16) -> Result<()> {
        // Read kernel socket tables (no additional executable or shell). Only
        // wildcard IPv4 LISTEN proves that the new management IP is accepted.
        let path = self.root.path("/proc/net/tcp")?;
        let text = std::fs::read_to_string(path)
            .map_err(|_| unsupported("management_listener_unavailable"))?;
        if !text.lines().skip(1).any(|l| {
            let fields = l.split_whitespace().collect::<Vec<_>>();
            fields.len() > 3 && fields[1] == format!("00000000:{port:04X}") && fields[3] == "0A"
        }) {
            return Err(unsupported("management_listener_unavailable"));
        }
        Ok(())
    }
    pub(crate) fn tessera_files(
        &self,
        p: &NetworkProfiles,
        files: &mut Vec<FileChange>,
    ) -> Result<()> {
        add(
            &self.root,
            files,
            TESSERA_BINDING,
            Some(
                &ndimon_render::network::tessera_binding(p)
                    .map_err(|_| unsupported("invalid_binding"))?,
            ),
            0o644,
        )?;
        add(
            &self.root,
            files,
            "/etc/systemd/system/tesserad.service.d/70-ndimon-network.conf",
            Some(&dropin("ndimontessera.slice")),
            0o644,
        )
    }
    pub(crate) fn activate(
        &self,
        p: &NetworkProfiles,
        old: &NetworkProfiles,
        metadata: &Metadata,
        nm: &[NmChange],
        restore: bool,
    ) -> Result<()> {
        match p.backend {
            Some(NetworkBackend::Networkd | NetworkBackend::Netplan) => {
                if p.backend == Some(NetworkBackend::Netplan) {
                    self.run(Tool::Netplan, &["generate"], 30)?;
                    self.run(Tool::Netplan, &["apply"], 40)?;
                }
                self.run(Tool::Networkctl, &["reload"], 20)?;
                self.wait_links(p)?;
                let mut args = vec!["reconfigure".into(), "--".into()];
                args.extend(names(p));
                self.run_owned(Tool::Networkctl, &args, 30, None)?;
            }
            Some(NetworkBackend::Networkmanager) => self.activate_nm(nm, restore)?,
            Some(NetworkBackend::Ifupdown) => self.activate_ifupdown(p, old, restore)?,
            _ => return Err(unsupported("unknown_backend")),
        }
        self.remove_links(old, p)?;
        let _ = metadata;
        Ok(())
    }
    fn wait_links(&self, p: &NetworkProfiles) -> Result<()> {
        let end = self
            .clock
            .now_ms()?
            .saturating_add(10_000)
            .min(self.deadline.unwrap_or(u64::MAX));
        loop {
            let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
            if names(p).iter().all(|name| {
                rows(&links).is_ok_and(|links| links.iter().any(|l| l["ifname"] == *name))
            }) {
                return Ok(());
            }
            if self.clock.now_ms()? >= end {
                return Err(failed("verify_backend", "link_not_ready"));
            }
            self.clock.sleep_ms(250);
        }
    }
    pub(crate) fn check_plan(&self, plan: &Plan) -> Result<()> {
        for change in &plan.files {
            if self.root.snapshot(&change.path)? != change.before {
                return Err(failed("preflight", "configuration_changed"));
            }
        }
        if self.table("ndimon_management")? != plan.old_table {
            return Err(failed("preflight", "guard_changed"));
        }
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        let owned = plan
            .previous
            .networks
            .iter()
            .filter(|n| n.vlan_id.is_some())
            .map(|n| ifname(&plan.previous, n))
            .collect::<Vec<_>>();
        self.check_links(&plan.candidate.interface, &owned, &links)?;
        let drift = match plan.candidate.backend {
            Some(NetworkBackend::Networkd | NetworkBackend::Netplan) => {
                self.networkd_drift(&plan.candidate)?
            }
            Some(NetworkBackend::Networkmanager) => {
                self.check_nm_snapshots(&plan.nm)?;
                self.nm_drift(&plan.candidate, &plan.transitional_metadata())?
            }
            Some(NetworkBackend::Ifupdown) => self.ifupdown_drift(&plan.candidate)?,
            _ => return Err(unsupported("unknown_backend")),
        };
        if drift.iter().any(|d| d.blocking) {
            return Err(failed("preflight", "foreign_configuration_selected"));
        }
        self.verify_listener(plan.portal_port)
    }
    pub(crate) fn remove_links(&self, old: &NetworkProfiles, new: &NetworkProfiles) -> Result<()> {
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        for n in old
            .networks
            .iter()
            .filter(|n| n.vlan_id.is_some() && !new.networks.iter().any(|m| m.vlan_id == n.vlan_id))
        {
            let name = ifname(old, n);
            if rows(&links)?.iter().any(|l| l["ifname"] == name) {
                if let Err(error) = self.run(Tool::Ip, &["link", "delete", "dev", &name], 20) {
                    // NM can finish deleting a deactivated VLAN after our
                    // enumeration. Accept only freshly proven absence, never
                    // an arbitrary failed delete or localized stderr text.
                    let current = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
                    if rows(&current)?.iter().any(|l| l["ifname"] == name) {
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn slices(&self) -> Result<()> {
        self.run(Tool::Systemctl, &["daemon-reload"], 20)?;
        self.run(
            Tool::Systemctl,
            &["start", "ndimonndi.slice", "ndimontessera.slice"],
            20,
        )?;
        Ok(())
    }
    pub(crate) fn stop_consumers(&self, plan: &Plan, recovery: bool) -> Result<()> {
        for unit in MEDIA {
            if (recovery || plan.affected_services.iter().any(|s| s == unit))
                && self.active(unit)?
            {
                self.run(Tool::Systemctl, &["stop", unit], 40)?;
            }
        }
        Ok(())
    }
    pub(crate) fn restore_consumers(&self, plan: &Plan, recovery: bool) -> Result<()> {
        self.run(Tool::Systemctl, &["daemon-reload"], 20)?;
        for unit in MEDIA {
            if (recovery || plan.affected_services.iter().any(|s| s == unit))
                && plan.active_services.iter().any(|s| s == unit)
            {
                self.start_consumer(unit)?;
            }
        }
        Ok(())
    }
    /// Best effort, for the operator's way out of a failed recovery: remove what
    /// the failed change created (its NetworkManager connections and VLAN links),
    /// so the committed network can be changed again instead of meeting its
    /// leftovers as `foreign_vlan`.
    pub(crate) fn discard_created(&self, plan: &Plan) {
        if plan.candidate.backend == Some(NetworkBackend::Networkmanager) {
            let _ = self.discard_created_nm(&plan.nm);
        }
        let _ = self.remove_links(&plan.candidate, &plan.previous);
    }
    /// The failure path of a recovery: start again every unit that ran when the
    /// change was planned and does not now. Every unit is tried, none is waited
    /// for (a start that hangs must not hold the retained failure back), and the
    /// first error is returned.
    pub(crate) fn resume_consumers(&self, plan: &Plan) -> Result<()> {
        let mut first = self.run(Tool::Systemctl, &["daemon-reload"], 20).err();
        for unit in MEDIA {
            if plan.active_services.iter().any(|s| s == unit) && !self.active(unit).unwrap_or(false)
            {
                if let Err(error) = self.run(Tool::Systemctl, &["start", "--no-block", unit], 40) {
                    first.get_or_insert(error);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
    pub(crate) fn start_consumer(&self, unit: &str) -> Result<()> {
        if self.boot_recovery {
            // These units require/order after the boot oneshot that is running
            // us. Guards/addressing are restored before queuing their starts;
            // waiting here would wait on our own exit.
            self.run(Tool::Systemctl, &["start", "--no-block", unit], 40)?;
        } else {
            self.run(Tool::Systemctl, &["start", unit], 40)?;
        }
        Ok(())
    }
    pub(crate) fn readiness(
        &self,
        p: &NetworkProfiles,
        metadata: &Metadata,
        recovery: bool,
        owned: bool,
    ) -> Result<Ipv4Addr> {
        let end = self
            .clock
            .now_ms()?
            .saturating_add(45_000)
            .min(self.deadline.unwrap_or(u64::MAX));
        loop {
            match self.verify_addresses(p, metadata, recovery, owned) {
                Ok(a) => return Ok(a),
                Err(e)
                    if matches!(&e, ndimon_types::ApiError::ApplyFailed { reason, .. } if reason.starts_with("address_not_ready"))
                        && self.clock.now_ms()? < end =>
                {
                    self.clock.sleep_ms(250)
                }
                Err(e) => return Err(e),
            }
        }
    }
    pub(crate) fn management_address(&self, p: &NetworkProfiles) -> Result<Option<Ipv4Addr>> {
        let name = ifname(p, assigned(p, &p.roles.management)?);
        let live = self.json(Tool::Ip, &["-j", "-4", "address", "show"])?;
        let addresses = rows(&live)?
            .iter()
            .filter(|i| i["ifname"] == name)
            .filter_map(|i| i["addr_info"].as_array())
            .flatten()
            .filter(|a| a["family"] == "inet" && a["scope"] == "global")
            .collect::<Vec<_>>();
        if addresses.len() != 1 {
            return Ok(None);
        }
        Ok(addresses[0]["local"].as_str().and_then(|a| a.parse().ok()))
    }
    fn verify_addresses(
        &self,
        p: &NetworkProfiles,
        metadata: &Metadata,
        recovery: bool,
        owned: bool,
    ) -> Result<Ipv4Addr> {
        let live = self.json(Tool::Ip, &["-j", "-4", "address", "show"])?;
        let mut subnets = Vec::new();
        let mut management = None;
        for n in &p.networks {
            let name = ifname(p, n);
            let addresses = rows(&live)?
                .iter()
                .find(|i| i["ifname"] == name)
                .and_then(|i| i["addr_info"].as_array())
                .into_iter()
                .flatten()
                .filter(|a| a["family"] == "inet" && a["scope"] == "global")
                .collect::<Vec<_>>();
            let required = n.mode == AddressMode::Static
                || n.id == p.roles.management
                || (!recovery && (n.id == p.roles.ndi || n.id == p.roles.audio));
            if addresses.is_empty() {
                if required {
                    // The atom first (the wait loop above keys on it), then which
                    // interface is still without an address.
                    return Err(failed(
                        "verify_addresses",
                        &format!("address_not_ready: {name} has no IPv4 address"),
                    ));
                } else {
                    continue;
                }
            }
            if addresses.len() != 1 {
                return Err(failed("verify_addresses", "unexpected_address_count"));
            }
            let a: Ipv4Addr = addresses[0]["local"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| failed("verify_addresses", "unusable_unicast_address"))?;
            let prefix = addresses[0]["prefixlen"]
                .as_u64()
                .filter(|p| (8..=30).contains(p))
                .ok_or_else(|| failed("verify_addresses", "unexpected_static_prefix"))?
                as u8;
            if !crate::validation::unicast(a) {
                return Err(failed("verify_addresses", "unusable_unicast_address"));
            }
            if n.mode == AddressMode::Static && (a.to_string() != n.address || prefix != n.prefix) {
                return Err(failed(
                    "verify_addresses",
                    &if a.to_string() == n.address {
                        format!(
                            "unexpected_static_prefix: {name} has {a}/{prefix}, expected {}/{}",
                            n.address, n.prefix
                        )
                    } else {
                        format!(
                            "address_not_ready: {name} has {a}/{prefix}, expected {}/{}",
                            n.address, n.prefix
                        )
                    },
                ));
            }
            let (low, high) = subnet(a, prefix);
            if subnets.iter().any(|(l, h)| low <= *h && *l <= high) {
                return Err(failed("verify_addresses", "live_subnets_overlap"));
            }
            subnets.push((low, high));
            if n.id == p.roles.management {
                management = Some(a);
            }
        }
        // All-tagged parents must have no lingering global IPv4 at all.
        if !p.networks.iter().any(|n| n.vlan_id.is_none())
            && rows(&live)?
                .iter()
                .filter(|i| i["ifname"] == p.interface)
                .any(|i| {
                    i["addr_info"].as_array().is_some_and(|a| {
                        a.iter()
                            .any(|a| a["family"] == "inet" && a["scope"] == "global")
                    })
                })
        {
            return Err(failed("verify_addresses", "unexpected_address_count"));
        }
        let management_name = ifname(p, assigned(p, &p.roles.management)?);
        let routes = self.json(Tool::Ip, &["-j", "-4", "route", "show", "default"])?;
        let management_row = assigned(p, &p.roles.management)?;
        let management_routes = rows(&routes)?
            .iter()
            .filter(|r| r["dev"] == management_name)
            .collect::<Vec<_>>();
        if management_routes.len() > 1 {
            return Err(failed("verify_addresses", "unexpected_default_routes"));
        }
        if management_row.mode == AddressMode::Static {
            if management_row.gateway.is_empty() {
                if !management_routes.is_empty() {
                    return Err(failed("verify_addresses", "unexpected_default_routes"));
                }
            } else if management_routes.len() != 1
                || management_routes[0]["gateway"] != management_row.gateway
            {
                return Err(failed("verify_addresses", "address_not_ready"));
            }
        }
        for r in rows(&routes)? {
            let name = r["dev"].as_str().unwrap_or("");
            if names(p).iter().any(|n| n == name) && name != management_name {
                return Err(failed("verify_addresses", "default_route_on_media"));
            }
            if name == management_name {
                let expected = metadata.metric(assigned(p, &p.roles.management)?);
                if r["metric"].as_u64().unwrap_or(0) != expected as u64 {
                    return Err(failed("verify_backend", "route_metric_changed"));
                }
            }
        }
        if owned {
            match p.backend {
                Some(NetworkBackend::Networkd | NetworkBackend::Netplan) => {
                    for name in names(p) {
                        let s =
                            self.run(Tool::Networkctl, &["status", "--no-pager", "--", &name], 8)?;
                        if networkd::selected(&s)
                            != Some(
                                format!("/etc/systemd/network/00-ndimon-{name}.network").as_str(),
                            )
                        {
                            return Err(failed("verify_backend", "foreign_configuration_selected"));
                        }
                    }
                }
                Some(NetworkBackend::Networkmanager) => self.verify_nm(p, metadata)?,
                Some(NetworkBackend::Ifupdown) => self.verify_ifupdown(p)?,
                _ => return Err(unsupported("unknown_backend")),
            }
        }
        if !owned
            && !metadata.parent_source.is_empty()
            && matches!(
                p.backend,
                Some(NetworkBackend::Networkd | NetworkBackend::Netplan)
            )
        {
            let status = self.run(
                Tool::Networkctl,
                &["status", "--no-pager", "--", &p.interface],
                8,
            )?;
            if networkd::selected(&status) != Some(metadata.parent_source.as_str()) {
                return Err(failed("verify_backend", "foreign_configuration_selected"));
            }
        }
        management.ok_or_else(|| failed("verify_addresses", "address_not_ready"))
    }
    pub(crate) fn arp(&self, p: &NetworkProfiles) -> Result<()> {
        for n in p.networks.iter().filter(|n| n.mode == AddressMode::Static) {
            self.run(
                Tool::Arping,
                &["-D", "-I", &ifname(p, n), "-c", "2", "-w", "3", &n.address],
                10,
            )
            .map_err(|_| failed("verify_addresses", "duplicate_ipv4"))?;
        }
        Ok(())
    }
}
pub(crate) fn add(
    root: &Root,
    changes: &mut Vec<FileChange>,
    path: &str,
    text: Option<&str>,
    mode: u32,
) -> Result<()> {
    if let Some(change) = root.change(path, text, mode)? {
        if changes.iter().any(|c| c.path == path) {
            return Err(failed("plan", "duplicate_artifact"));
        }
        changes.push(change);
    }
    Ok(())
}
pub fn affected_roles(old: &NetworkProfiles, new: &NetworkProfiles) -> Vec<String> {
    [
        ("management", &old.roles.management, &new.roles.management),
        ("ndi", &old.roles.ndi, &new.roles.ndi),
        ("audio", &old.roles.audio, &new.roles.audio),
    ]
    .into_iter()
    .filter(|(_, a, b)| {
        a != b
            || old.networks.iter().find(|n| n.id == **a)
                != new.networks.iter().find(|n| n.id == **b)
    })
    .map(|(role, _, _)| role.into())
    .collect()
}
pub(crate) fn dropin(slice: &str) -> String {
    ndimon_render::network::dropin(slice)
}

pub fn firewall(p: &NetworkProfiles, port: u16) -> Result<String> {
    let management = ifname(p, assigned(p, &p.roles.management)?);
    let all = p
        .networks
        .iter()
        .map(|n| format!("\"{}\"", ifname(p, n)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut s = format!(
        "table inet ndimon_management {{\n chain input {{\n type filter hook input priority -5; policy accept;\n iifname != {{ \"lo\", \"{management}\" }} tcp dport {port} reject;\n }}\n chain forward {{\n type filter hook forward priority -5; policy accept;\n iifname {{ {all} }} reject;\n oifname {{ {all} }} reject;\n }}\n"
    );
    for (chain, hook, direction) in [
        ("media_output", "output", "oifname"),
        ("media_input", "input", "iifname"),
    ] {
        s.push_str(&format!(
            " chain {chain} {{\n type filter hook {hook} priority 0; policy accept;\n"
        ));
        for (id, slice) in [
            (&p.roles.ndi, "ndimonndi.slice"),
            (&p.roles.audio, "ndimontessera.slice"),
        ] {
            let name = ifname(p, assigned(p, id)?);
            s.push_str(&format!(" socket cgroupv2 level 1 \"{slice}\" {direction} != {{ \"lo\", \"{name}\" }} counter reject;\n"));
        }
        s.push_str(" }\n");
    }
    s.push_str("}\n");
    Ok(s)
}
