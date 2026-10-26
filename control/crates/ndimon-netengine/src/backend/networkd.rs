use super::*;
use std::path::Path;

pub(crate) fn selected(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Network File:").map(str::trim))
        .filter(|s| s.ends_with(".network"))
}
/// Same-basename resolution first, then lexical order (NE-3.4/5.2).
impl Host {
    pub(super) fn import_networkd(&self, p: &NetworkProfiles, source: &str) -> Result<Metadata> {
        let text = self.effective_networkd(source)?;
        let mut section = "";
        let mut filtered = String::new();
        for line in text.lines() {
            let clean = line.trim();
            if clean.starts_with('[') {
                section = clean;
            }
            if section == "[Network]" {
                if let Some(value) = clean.strip_prefix("VLAN=") {
                    if !value.is_empty() && !names(p).iter().any(|n| n == value) {
                        return Err(unsupported("foreign_vlan"));
                    }
                    continue;
                }
            }
            filtered.push_str(line);
            filtered.push('\n');
        }
        let (_, dhcp_metric, static_metric) = parse(&filtered).map_err(|e| located(e, source))?;
        Ok({
            let mut metadata = Metadata::default();
            metadata.parent_source = source.into();
            metadata.dhcp_metric = dhcp_metric;
            metadata.static_metric = static_metric;
            metadata
        })
    }
    pub(crate) fn effective_dropins(&self, basename: &str) -> Result<Vec<String>> {
        let mut chosen = BTreeMap::new();
        for root in ROOTS {
            for p in self
                .root
                .list(&format!("{root}/{basename}.d"), Some(".conf"))?
            {
                chosen.insert(p.rsplit('/').next().unwrap_or("").to_owned(), p);
            }
        }
        Ok(chosen.into_values().collect())
    }
    fn effective_networkd(&self, path: &str) -> Result<String> {
        self.effective_networkd_if_present(path)?
            .ok_or_else(|| unsupported("missing_active_configuration"))
    }
    fn effective_networkd_if_present(&self, path: &str) -> Result<Option<String>> {
        if !ROOTS
            .iter()
            .any(|root| Path::new(path).parent() == Some(Path::new(root)))
        {
            return Err(unsupported("unexpected_configuration_path"));
        }
        let Some(mut text) = self.root.text(path)? else {
            return Ok(None);
        };
        let base = path
            .rsplit('/')
            .next()
            .ok_or_else(|| unsupported("unexpected_configuration_path"))?;
        for p in self.effective_dropins(base)? {
            text.push('\n');
            if let Some(dropin) = self.root.text(&p)? {
                text.push_str(&dropin);
            }
        }
        Ok(Some(text))
    }
    pub(super) fn adopt_networkd(
        &self,
        _interface: &str,
        path: &str,
    ) -> Result<(NetworkProfile, Metadata)> {
        let text = self.effective_networkd(path)?;
        let (row, dhcp_metric, static_metric) = parse(&text).map_err(|e| located(e, path))?;
        Ok((row, {
            let mut metadata = Metadata::default();
            metadata.parent_source = path.into();
            metadata.dhcp_metric = dhcp_metric;
            metadata.static_metric = static_metric;
            metadata
        }))
    }
    pub(crate) fn render_networkd(
        &self,
        previous: &NetworkProfiles,
        p: &NetworkProfiles,
        meta: &Metadata,
        netdevs: bool,
        files: &mut Vec<FileChange>,
    ) -> Result<()> {
        let parent = p.networks.iter().find(|n| n.vlan_id.is_none());
        let mut parent_text = format!(
            "[Match]\nName={}\n\n[Link]\nRequiredForOnline=no\n\n[Network]\nLinkLocalAddressing=no\nIPv6AcceptRA=no\n",
            p.interface
        );
        for n in p.networks.iter().filter(|n| n.vlan_id.is_some()) {
            parent_text.push_str(&format!("VLAN={}\n", ifname(p, n)));
        }
        parent_text.push_str(&address(
            parent,
            parent.is_some_and(|n| n.id == p.roles.management),
            meta,
        ));
        add(
            &self.root,
            files,
            &format!("/etc/systemd/network/00-ndimon-{}.network", p.interface),
            Some(&parent_text),
            0o644,
        )?;
        for n in p.networks.iter().filter(|n| n.vlan_id.is_some()) {
            let name = ifname(p, n);
            let text = format!(
                "[Match]\nName={name}\n\n[Link]\nRequiredForOnline=no\n\n[Network]\nLinkLocalAddressing=no\nIPv6AcceptRA=no\n{}",
                address(Some(n), n.id == p.roles.management, meta)
            );
            add(
                &self.root,
                files,
                &format!("/etc/systemd/network/00-ndimon-{name}.network"),
                Some(&text),
                0o644,
            )?;
            if netdevs {
                let tag = n.vlan_id.ok_or_else(|| unsupported("invalid_vlan_id"))?;
                let text = format!("[NetDev]\nName={name}\nKind=vlan\n\n[VLAN]\nId={tag}\n");
                add(
                    &self.root,
                    files,
                    &format!("/etc/systemd/network/00-ndimon-{name}.netdev"),
                    Some(&text),
                    0o644,
                )?;
            }
        }
        for n in previous
            .networks
            .iter()
            .filter(|n| n.vlan_id.is_some() && !p.networks.iter().any(|m| m.vlan_id == n.vlan_id))
        {
            for suffix in if netdevs {
                &["network", "netdev"][..]
            } else {
                &["network"][..]
            } {
                add(
                    &self.root,
                    files,
                    &format!(
                        "/etc/systemd/network/00-ndimon-{}.{suffix}",
                        ifname(previous, n)
                    ),
                    None,
                    0o644,
                )?;
            }
        }
        let old = format!("/etc/systemd/network/05-ndimon-{}.network", p.interface);
        // The adopted source is an explicit ownership exception. Never remove a
        // similarly named file on another port or an unrecorded product prefix.
        if meta.parent_source == old {
            add(&self.root, files, &old, None, 0o644)?;
        }
        for root in ROOTS {
            for file in self.root.list(root, Some(".network.d"))? {
                for path in self.root.list(&file, Some(".conf"))? {
                    if let Some(text) = self.root.text(&path)? {
                        if text.starts_with(&format!(
                            "# Managed by ndimon-net VLANs for {}\n",
                            p.interface
                        )) {
                            add(&self.root, files, &path, None, 0o644)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub fn networkd_drift(&self, p: &NetworkProfiles) -> Result<Vec<Foreign>> {
        let mut chosen = BTreeMap::new();
        for root in ROOTS {
            for file in self.root.list(root, Some(".network"))? {
                chosen.insert(file.rsplit('/').next().unwrap_or("").to_owned(), file);
            }
        }
        let mut drift = Vec::new();
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        for (base, path) in chosen {
            if self.handoff && self.handoff_paths.contains(&path) {
                continue;
            }
            // Netplan replaces generated files during apply. A directory entry
            // that disappeared is no longer a competing match; adoption of an
            // explicitly selected source above still requires it to exist.
            let Some(text) = self.effective_networkd_if_present(&path)? else {
                continue;
            };
            let entries = ini(&text)?;
            let mut match_names = Vec::new();
            let mut match_macs = Vec::new();
            let mut unknown_selector = false;
            for (s, k, v) in entries {
                if s == "Match" {
                    match k.as_str() {
                        "Name" => reset_list(&mut match_names, &v),
                        "MACAddress" => reset_list(&mut match_macs, &v),
                        _ if !v.is_empty() => unknown_selector = true,
                        _ => (),
                    }
                }
            }
            for name in names(p) {
                let expected = format!("00-ndimon-{name}.network");
                let mac = rows(&links)?
                    .iter()
                    .find(|l| l["ifname"] == name)
                    .and_then(|l| l["address"].as_str())
                    .unwrap_or("");
                let matches = pattern_list(&match_names, &name)?
                    && (match_macs.is_empty()
                        || match_macs.iter().any(|m| m.eq_ignore_ascii_case(mac)));
                // Match keys are conjunctive. A known Name/MAC mismatch rules
                // out the file even when it also has a Kind/Type selector.
                // Additional selectors cannot outrank an earlier exact pin.
                if matches {
                    let pin =
                        base == expected && path == format!("/etc/systemd/network/{expected}");
                    let dropins = self.effective_dropins(&base)?;
                    if pin && dropins.is_empty() && !unknown_selector {
                        continue;
                    }
                    let blocking =
                        base < expected || pin && (!dropins.is_empty() || unknown_selector);
                    drift.push({
                        let mut drift = Foreign::new(ndimon_types::Domain::Network, "");
                        drift.path = path.clone();
                        drift.artifact = Some(base.clone());
                        drift.selector = format!(
                            "Name={} MACAddress={}",
                            match_names.join(" "),
                            match_macs.join(" ")
                        );
                        drift.interface = name;
                        drift.neutralized = !blocking;
                        drift.blocking = blocking;
                        drift.foreign = Some(drift.path.clone());
                        drift
                    });
                }
            }
        }
        Ok(drift)
    }
    pub(super) fn link_mac(&self, name: &str) -> Result<String> {
        let v = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        Ok(rows(&v)?
            .iter()
            .find(|l| l["ifname"] == name)
            .and_then(|l| l["address"].as_str())
            .unwrap_or("")
            .to_owned())
    }
}
fn reset_list(list: &mut Vec<String>, value: &str) {
    if value.is_empty() {
        list.clear();
    } else {
        list.extend(
            value
                .split_whitespace()
                .map(|v| v.trim_matches('"').to_owned()),
        );
    }
}
fn pattern_list(patterns: &[String], name: &str) -> Result<bool> {
    if patterns.is_empty() {
        return Ok(true);
    }
    let mut positive = false;
    let mut any_positive = false;
    for p in patterns {
        let (negative, p) = p
            .strip_prefix('!')
            .map_or((false, p.as_str()), |p| (true, p));
        let matches = glob::Pattern::new(p)
            .map_err(|_| unsupported("unsafe_configuration"))?
            .matches(name);
        if negative && matches {
            return Ok(false);
        }
        if !negative {
            any_positive = true;
            positive |= matches;
        }
    }
    Ok(!any_positive || positive)
}
fn address(n: Option<&NetworkProfile>, management: bool, meta: &Metadata) -> String {
    let Some(n) = n else {
        return "DHCP=no\n".into();
    };
    // NE-2.12a: `Domains=` is a [Network] key, so it precedes the [DHCPv4] section.
    let domains = if management && !n.dns_search.is_empty() {
        format!("Domains={}\n", n.dns_search.join(" "))
    } else {
        String::new()
    };
    if n.mode == AddressMode::Dhcp {
        return format!(
            "DHCP=ipv4\n{domains}\n[DHCPv4]\nUseDNS={}\nUseRoutes={}\nUseNTP=no\nUseHostname=no\nRouteMetric={}\n",
            if management { "yes" } else { "no" },
            if management { "yes" } else { "no" },
            meta.dhcp_metric
        );
    }
    let mut s = format!("DHCP=no\nAddress={}/{}\n", n.address, n.prefix);
    if management {
        for dns in &n.dns {
            s.push_str(&format!("DNS={dns}\n"));
        }
        s.push_str(&domains);
        if !n.gateway.is_empty() {
            s.push_str(&format!(
                "\n[Route]\nDestination=0.0.0.0/0\nGateway={}\nMetric={}\n",
                n.gateway, meta.static_metric
            ));
        }
    }
    s
}
/// `IPv6PrivacyExtensions=`: a systemd boolean, `prefer-public` or `kernel`.
fn privacy_extensions(value: &str) -> bool {
    [
        "1",
        "0",
        "yes",
        "no",
        "y",
        "n",
        "true",
        "false",
        "t",
        "f",
        "on",
        "off",
        "prefer-public",
        "kernel",
    ]
    .contains(&value.to_ascii_lowercase().as_str())
}
/// Conservative INI parser. Unsupported syntax is never silently discarded.
pub(super) fn ini(text: &str) -> Result<Vec<(String, String, String)>> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(['#', ';']))
    {
        if line.ends_with('\\') {
            return Err(unsupported("unrepresentable_configuration"));
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].into();
            out.push((section.clone(), String::new(), String::new()));
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
        out.push((section.clone(), k.trim().into(), v.trim().into()));
    }
    Ok(out)
}
pub fn parse(text: &str) -> Result<(NetworkProfile, u32, u32)> {
    let entries = ini(text)?;
    let mut n = NetworkProfile::dhcp("untagged", "Untagged");
    let mut addresses = Vec::new();
    let mut gateways = Vec::new();
    let mut dns = Vec::new();
    let mut domains = Vec::new();
    let mut dhcp = false;
    let mut dhcp_metric = 1024;
    let mut static_metric = 0;
    let mut route = BTreeMap::<String, String>::new();
    let finish = |route: &mut BTreeMap<String, String>,
                  gateways: &mut Vec<String>,
                  metric: &mut u32|
     -> Result<()> {
        if route.is_empty() {
            return Ok(());
        }
        if route
            .keys()
            .any(|k| !["Destination", "Gateway", "Metric"].contains(&k.as_str()))
        {
            return Err(unsupported("custom_routes"));
        }
        if route
            .get("Destination")
            .is_some_and(|d| d != "0.0.0.0/0" && d != "default")
        {
            return Err(unsupported("custom_routes"));
        }
        gateways.push(
            route
                .get("Gateway")
                .ok_or_else(|| unsupported("custom_routes"))?
                .clone(),
        );
        if let Some(m) = route.get("Metric") {
            *metric = m
                .parse()
                .map_err(|_| unsupported("unrepresentable_configuration"))?;
        }
        route.clear();
        Ok(())
    };
    let mut last = String::new();
    for (section, key, value) in entries {
        if key.is_empty() {
            if last == "Route" {
                finish(&mut route, &mut gateways, &mut static_metric)?;
            }
            last = section;
            continue;
        }
        if value.is_empty() && section != "Network" && section != "Address" {
            continue;
        }
        match (section.as_str(), key.as_str()) {
            ("Match", _) | ("Link", "RequiredForOnline") => (),
            ("Network", "DHCP") => match value.as_str() {
                "ipv4" => dhcp = true,
                "no" | "false" => dhcp = false,
                "yes" | "true" | "ipv6" => return Err(unsupported("ipv6_configuration")),
                _ => return Err(unsupported("unrepresentable_configuration")),
            },
            ("Network" | "Address", "Address") => reset_list(&mut addresses, &value),
            ("Network", "Gateway") => reset_list(&mut gateways, &value),
            ("Network", "DNS") => reset_list(&mut dns, &value),
            ("Network", "Domains") => reset_list(&mut domains, &value),
            ("Network", "IPv6AcceptRA") if value == "no" || value == "false" => (),
            // NE-3.9a (U17): a privacy-extension choice is not address intent. The
            // managed pin switches IPv6 addressing off and does not carry it.
            ("Network", "IPv6PrivacyExtensions") if privacy_extensions(&value) => (),
            ("Network", "LinkLocalAddressing")
                if ["no", "false", "ipv4"].contains(&value.as_str()) => {}
            ("Network", "VLAN") if value.is_empty() => (),
            ("DHCPv4" | "DHCP", "RouteMetric") => {
                dhcp_metric = value
                    .parse()
                    .map_err(|_| unsupported("unrepresentable_configuration"))?
            }
            ("DHCPv4" | "DHCP", "UseDNS" | "UseRoutes" | "UseNTP" | "UseHostname")
                if ["yes", "no", "true", "false"].contains(&value.as_str()) => {}
            // NE-2.12a: lease domains are not desired state. Only "do not use them"
            // is representable; `yes`, `true` or `route` fall through to a refusal.
            ("DHCPv4" | "DHCP", "UseDomains") if ["no", "false"].contains(&value.as_str()) => {}
            ("Route", _) => {
                route.insert(key, value);
            }
            ("RoutingPolicyRule", _) => return Err(unsupported("policy_routing")),
            ("Network", "IPv6AcceptRA" | "DHCPv6" | "IPv6Token")
            | ("IPv6AcceptRA" | "DHCPv6", _) => return Err(unsupported("ipv6_configuration")),
            _ if value.is_empty() => (),
            _ => return Err(unrepresentable(format!("[{section}] {key}"))),
        }
    }
    finish(&mut route, &mut gateways, &mut static_metric)?;
    // A routing-only `~domain` is not a search domain: refuse it, don't drop it.
    n.dns_search = search_list(domains, "[Network] Domains")?;
    if addresses
        .iter()
        .chain(gateways.iter())
        .chain(dns.iter())
        .any(|s| s.contains(':'))
    {
        return Err(unsupported("ipv6_configuration"));
    }
    if dhcp && (!addresses.is_empty() || !gateways.is_empty() || !dns.is_empty()) {
        return Err(unsupported("mixed_dhcp_static"));
    }
    if !dhcp {
        if addresses.len() != 1 {
            return Err(unsupported("multiple_addresses"));
        }
        let (a, prefix) = addresses[0]
            .split_once('/')
            .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
        n.mode = AddressMode::Static;
        n.address = a.into();
        n.prefix = prefix
            .parse()
            .map_err(|_| unsupported("unrepresentable_configuration"))?;
        if gateways.len() > 1 {
            return Err(unsupported("multiple_gateways"));
        }
        n.gateway = gateways.into_iter().next().unwrap_or_default();
        n.dns = dns;
    }
    Ok((n, dhcp_metric, static_metric))
}
