use super::*;
#[derive(Debug)]
struct Stanza {
    path: String,
    name: String,
    family: String,
    method: String,
    values: BTreeMap<String, String>,
    active: bool,
}
impl Host {
    pub(super) fn import_ifupdown(&self, p: &NetworkProfiles) -> Result<Metadata> {
        self.isc_dhclient()?;
        if self.ifupdown_drift(p)?.iter().any(|d| d.blocking) {
            return Err(unsupported("foreign_connection"));
        }
        let management = ifname(p, assigned(p, &p.roles.management)?);
        let stanzas = self.stanzas()?;
        let stanza = stanzas
            .iter()
            .find(|s| s.name == management && s.family == "inet")
            .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
        if let Some(key) = stanza.values.keys().find(|k| {
            ![
                "address",
                "gateway",
                "dns-nameservers",
                "dns-search",
                "metric",
                "vlan-raw-device",
                "privext",
            ]
            .contains(&k.as_str())
        }) {
            return Err(located(
                unsupported(&format!("custom_hooks: {key}")),
                &stanza.path,
            ));
        }
        let metric = stanza
            .values
            .get("metric")
            .map(|s| s.parse::<u32>())
            .transpose()
            .map_err(|_| unsupported("unrepresentable_configuration"))?
            .unwrap_or(0);
        Ok({
            let mut metadata = Metadata::default();
            metadata.parent_source = format!("/etc/network/interfaces.d/ndimon-{}", p.interface);
            metadata.dhcp_metric = metric;
            metadata.static_metric = metric;
            metadata
        })
    }
    fn interface_sources(
        &self,
        path: &str,
        seen: &mut BTreeSet<String>,
        depth: usize,
        out: &mut Vec<(String, String)>,
    ) -> Result<()> {
        if depth > 16 || !seen.insert(path.into()) {
            return Err(unsupported("unsafe_configuration"));
        }
        let text = self
            .root
            .text(path)?
            .ok_or_else(|| unsupported("ifupdown_include_missing"))?;
        out.push((path.into(), text.clone()));
        for line in text
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
        {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if parts.first() == Some(&"source") {
                for pattern in &parts[1..] {
                    let path = std::path::Path::new(pattern);
                    let dir = path
                        .parent()
                        .and_then(|p| p.to_str())
                        .ok_or_else(|| unsupported("ifupdown_include_missing"))?;
                    let name = path
                        .file_name()
                        .and_then(|p| p.to_str())
                        .ok_or_else(|| unsupported("ifupdown_include_missing"))?;
                    let pattern = glob::Pattern::new(name)
                        .map_err(|_| unsupported("unsafe_configuration"))?;
                    for child in self.root.list(dir, None)? {
                        if pattern.matches(child.rsplit('/').next().unwrap_or("")) {
                            self.interface_sources(&child, seen, depth + 1, out)?;
                        }
                    }
                }
            } else if parts.first() == Some(&"source-directory") {
                for dir in &parts[1..] {
                    for child in self.root.list(dir, None)? {
                        let name = child.rsplit('/').next().unwrap_or("");
                        if name
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                        {
                            self.interface_sources(&child, seen, depth + 1, out)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn stanzas(&self) -> Result<Vec<Stanza>> {
        let mut sources = Vec::new();
        self.interface_sources(
            "/etc/network/interfaces",
            &mut BTreeSet::new(),
            0,
            &mut sources,
        )?;
        let mut out = Vec::new();
        for (path, text) in sources {
            let mut current: Option<Stanza> = None;
            let mut active = Vec::new();
            for line in text
                .lines()
                .map(|l| l.split('#').next().unwrap_or("").trim())
                .filter(|l| !l.is_empty())
            {
                let parts = line.split_whitespace().collect::<Vec<_>>();
                match parts[0] {
                    "auto" | "allow-auto" | "allow-hotplug" => {
                        active.extend(parts[1..].iter().map(|s| (*s).to_owned()))
                    }
                    "iface" if parts.len() == 4 => {
                        if let Some(s) = current.take() {
                            out.push(s);
                        }
                        current = Some(Stanza {
                            path: path.clone(),
                            name: parts[1].into(),
                            family: parts[2].into(),
                            method: parts[3].into(),
                            values: BTreeMap::new(),
                            active: false,
                        });
                    }
                    "source" | "source-directory" => {
                        if let Some(s) = current.take() {
                            out.push(s);
                        }
                    }
                    _ => {
                        let s = current
                            .as_mut()
                            .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
                        if parts.len() < 2
                            || s.values
                                .insert(parts[0].into(), parts[1..].join(" "))
                                .is_some()
                        {
                            return Err(unsupported("unrepresentable_configuration"));
                        }
                    }
                }
            }
            if let Some(s) = current {
                out.push(s);
            }
            for s in out.iter_mut().filter(|s| s.path == path) {
                for pattern in &active {
                    let p = glob::Pattern::new(pattern)
                        .map_err(|_| unsupported("unsafe_configuration"))?;
                    if p.matches(&s.name) {
                        if pattern != &s.name {
                            return Err(unsupported("unrepresentable_configuration"));
                        }
                        s.active = true;
                    }
                }
            }
        }
        Ok(out)
    }
    pub(super) fn adopt_ifupdown(&self, interface: &str) -> Result<(NetworkProfile, Metadata)> {
        if !self.root.exists("/etc/network/interfaces")? {
            return Err(unsupported("unknown_backend"));
        }
        let stanzas = self.stanzas()?;
        let matching = stanzas
            .iter()
            .filter(|s| s.name == interface)
            .collect::<Vec<_>>();
        if matching.iter().any(|s| s.family == "inet6") {
            return Err(unsupported("ipv6_configuration"));
        }
        if matching.len() > 1 {
            return Err(unsupported("duplicate_stanza"));
        }
        let s = matching
            .first()
            .ok_or_else(|| unsupported("unknown_backend"))?;
        if s.path == "/etc/network/interfaces" {
            return Err(unsupported("ifupdown_main_stanza"));
        }
        if s.path != format!("/etc/network/interfaces.d/ndimon-{interface}") {
            return Err(located(unrepresentable("iface"), &s.path));
        }
        if !s.active || s.family != "inet" {
            return Err(located(unrepresentable("iface"), &s.path));
        }
        if let Some(key) = s.values.keys().find(|k| {
            ![
                "address",
                "gateway",
                "dns-nameservers",
                "dns-search",
                "metric",
                "privext",
            ]
            .contains(&k.as_str())
        }) {
            return Err(located(
                unsupported(&format!("custom_hooks: {key}")),
                &s.path,
            ));
        }
        // NE-3.9a (U17): `privext` is the RFC 4941 choice (0, 1 or 2), not address
        // intent. The rendered stanza omits it, and Revert restores the file.
        if s.values
            .get("privext")
            .is_some_and(|v| !["0", "1", "2"].contains(&v.as_str()))
        {
            return Err(located(unrepresentable("privext"), &s.path));
        }
        self.isc_dhclient()?;
        let mut n = NetworkProfile::dhcp("untagged", "Untagged");
        // NE-2.12a: dns-search belongs to the management row in either method.
        n.dns_search = search_list(
            s.values
                .get("dns-search")
                .map(|s| s.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            "dns-search",
        )
        .map_err(|e| located(e, &s.path))?;
        match s.method.as_str() {
            "dhcp"
                if s.values
                    .keys()
                    .any(|k| !["metric", "dns-search", "privext"].contains(&k.as_str())) =>
            {
                return Err(located(unsupported("mixed_dhcp_static"), &s.path));
            }
            "dhcp" => (),
            "static" => {
                let (a, prefix) = s
                    .values
                    .get("address")
                    .and_then(|s| s.split_once('/'))
                    .ok_or_else(|| located(unrepresentable("address"), &s.path))?;
                n.mode = AddressMode::Static;
                n.address = a.into();
                n.prefix = prefix
                    .parse()
                    .map_err(|_| located(unrepresentable("address"), &s.path))?;
                n.gateway = s.values.get("gateway").cloned().unwrap_or_default();
                n.dns = s
                    .values
                    .get("dns-nameservers")
                    .map(|s| s.split_whitespace().map(str::to_owned).collect())
                    .unwrap_or_default();
            }
            _ => return Err(located(unrepresentable("iface"), &s.path)),
        }
        let metric = s
            .values
            .get("metric")
            .map(|v| {
                v.parse::<u32>()
                    .map_err(|_| located(unrepresentable("metric"), &s.path))
            })
            .transpose()?
            .unwrap_or(0);
        Ok((n, {
            let mut metadata = Metadata::default();
            metadata.parent_source = s.path.clone();
            metadata.dhcp_metric = metric;
            metadata.static_metric = metric;
            metadata
        }))
    }
    fn isc_dhclient(&self) -> Result<()> {
        let args = vec!["--version".into()];
        let v = self.output(Tool::Dhclient, &args, 5000, None)?;
        if v.code != 0 || !format!("{}{}", v.stdout, v.stderr).contains("isc-dhclient-") {
            return Err(unsupported("unsupported_dhcp_client"));
        }
        // Alternative clients can change ifupdown's selection and hooks. Until
        // a backend-specific client proof exists, refuse their presence.
        for p in [
            "/sbin/udhcpc",
            "/usr/sbin/udhcpc",
            "/sbin/dhcpcd",
            "/usr/sbin/dhcpcd",
        ] {
            if self.root.path(p)?.exists() {
                return Err(unsupported("unsupported_dhcp_client"));
            }
        }
        Ok(())
    }
    pub fn ifupdown_drift(&self, p: &NetworkProfiles) -> Result<Vec<Foreign>> {
        let mut drift = Vec::new();
        let mut counts = BTreeMap::<String, usize>::new();
        for s in self.stanzas()? {
            if self.handoff && self.handoff_paths.contains(&s.path) {
                continue;
            }
            if names(p).contains(&s.name) {
                let count = counts.entry(s.name.clone()).or_default();
                *count += 1;
                let expected = if s.name == p.interface {
                    format!("/etc/network/interfaces.d/ndimon-{}", p.interface)
                } else {
                    "/etc/network/interfaces.d/ndimon-vlans".into()
                };
                if s.path != expected || *count > 1 || s.family != "inet" {
                    drift.push({
                        let mut drift = Foreign::new(ndimon_types::Domain::Network, "");
                        drift.path = s.path;
                        drift.artifact =
                            Some(format!("iface {} {} {}", s.name, s.family, s.method));
                        drift.selector = format!("activation={}", s.active);
                        drift.interface = s.name;
                        drift.neutralized = false;
                        drift.blocking = true;
                        drift.foreign = Some(drift.path.clone());
                        drift
                    });
                }
            }
        }
        Ok(drift)
    }
    pub(super) fn verify_ifupdown(&self, p: &NetworkProfiles) -> Result<()> {
        if self.ifupdown_drift(p)?.iter().any(|d| d.blocking) {
            return Err(failed("verify_backend", "foreign_configuration_selected"));
        }
        let stanzas = self.stanzas()?;
        for name in names(p) {
            if stanzas
                .iter()
                .filter(|s| s.name == name && s.family == "inet" && s.active)
                .count()
                != 1
            {
                return Err(failed("verify_backend", "foreign_configuration_selected"));
            }
            let state = self.output(Tool::Ifquery, &["--state".into(), name], 5000, None)?;
            if state.code != 0 {
                return Err(failed("verify_backend", "address_not_ready"));
            }
        }
        Ok(())
    }
    pub(super) fn render_ifupdown(
        &self,
        p: &NetworkProfiles,
        meta: &Metadata,
        files: &mut Vec<FileChange>,
    ) -> Result<()> {
        self.isc_dhclient()?;
        // Prove the configured include pattern admits both owned paths, even
        // before the VLAN file exists. Only direct includes are supported here.
        let main = self
            .root
            .text("/etc/network/interfaces")?
            .ok_or_else(|| unsupported("ifupdown_include_missing"))?;
        for name in [format!("ndimon-{}", p.interface), "ndimon-vlans".into()] {
            if !main.lines().any(|l| {
                let parts = l.split_whitespace().collect::<Vec<_>>();
                parts.first() == Some(&"source")
                    && parts[1..].iter().any(|s| {
                        s.strip_prefix("/etc/network/interfaces.d/")
                            .and_then(|s| glob::Pattern::new(s).ok())
                            .is_some_and(|p| p.matches(&name))
                    })
                    || parts.as_slice() == ["source-directory", "/etc/network/interfaces.d"]
            }) {
                return Err(unsupported("ifupdown_include_missing"));
            }
        }
        let mut parent = format!("auto {}\niface {} inet manual\n", p.interface, p.interface);
        let mut vlans = String::new();
        let mut guarded = Vec::new();
        let mut all_dhcp = Vec::new();
        for n in &p.networks {
            let name = ifname(p, n);
            let mut s = format!(
                "auto {name}\niface {name} inet {}\n",
                if n.mode == AddressMode::Dhcp {
                    "dhcp"
                } else {
                    "static"
                }
            );
            if n.vlan_id.is_some() {
                s.push_str(&format!("    vlan-raw-device {}\n", p.interface));
            }
            if n.mode == AddressMode::Static {
                s.push_str(&format!("    address {}/{}\n", n.address, n.prefix));
                if !n.gateway.is_empty() {
                    s.push_str(&format!("    gateway {}\n", n.gateway));
                }
                if !n.dns.is_empty() {
                    s.push_str(&format!("    dns-nameservers {}\n", n.dns.join(" ")));
                }
            } else {
                all_dhcp.push(name.clone());
                if n.id != p.roles.management {
                    guarded.push(name.clone());
                }
            }
            // NE-2.12a: management only, DHCP or static; omitted when empty.
            if n.id == p.roles.management && !n.dns_search.is_empty() {
                s.push_str(&format!("    dns-search {}\n", n.dns_search.join(" ")));
            }
            s.push_str(&format!("    metric {}\n", meta.metric(n)));
            if n.vlan_id.is_none() {
                parent = s;
            } else {
                vlans.push_str(&s);
                vlans.push('\n');
            }
        }
        add(
            &self.root,
            files,
            &format!("/etc/network/interfaces.d/ndimon-{}", p.interface),
            Some(&parent),
            0o644,
        )?;
        add(
            &self.root,
            files,
            "/etc/network/interfaces.d/ndimon-vlans",
            Some(&vlans),
            0o644,
        )?;
        let mut hook = String::from("# Managed by NDIMon network profiles\n");
        if !all_dhcp.is_empty() {
            hook.push_str(&format!(
                "case \"$interface\" in\n{})\nunset new_ntp_servers new_host_name\n;;\nesac\n",
                all_dhcp.join("|")
            ));
        }
        if !guarded.is_empty() {
            hook.push_str(&format!("case \"$interface\" in\n{})\nunset new_routers new_static_routes new_classless_static_routes new_rfc3442_classless_static_routes new_domain_name_servers new_domain_name new_domain_search\n;;\nesac\n", guarded.join("|")));
        }
        add(
            &self.root,
            files,
            "/etc/dhcp/dhclient-enter-hooks.d/ndimon-vlans",
            Some(&hook),
            0o644,
        )
    }
    pub(crate) fn deconfigure(&self, p: &NetworkProfiles, force: bool) -> Result<()> {
        let mut interfaces = names(p);
        interfaces.reverse();
        for name in interfaces {
            let args = if force {
                vec!["--force".into(), name.clone()]
            } else {
                vec![name.clone()]
            };
            let result = self.output(Tool::Ifdown, &args, 30_000, None)?;
            if result.code != 0 {
                let state =
                    self.output(Tool::Ifquery, &["--state".into(), name.clone()], 5000, None)?;
                if state.code != 1 {
                    return Err(failed("deconfigure", "ifdown_failed"));
                }
                let live = self.json(Tool::Ip, &["-j", "-4", "address", "show"])?;
                if rows(&live)?
                    .iter()
                    .filter(|l| l["ifname"] == name)
                    .any(|l| {
                        l["addr_info"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|a| a["scope"] == "global"))
                    })
                {
                    return Err(failed("deconfigure", "ifdown_failed"));
                }
            }
        }
        Ok(())
    }
    pub(super) fn activate_ifupdown(
        &self,
        p: &NetworkProfiles,
        _old: &NetworkProfiles,
        _restore: bool,
    ) -> Result<()> {
        for name in names(p) {
            self.run(Tool::Ifup, &[&name], 40)?;
        }
        Ok(())
    }
}
