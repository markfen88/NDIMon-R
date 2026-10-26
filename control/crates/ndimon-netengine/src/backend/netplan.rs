use super::*;
const IP_KEYS: &[&str] = &[
    "dhcp4",
    "dhcp6",
    "addresses",
    "routes",
    "gateway4",
    "gateway6",
    "nameservers",
    "dhcp4-overrides",
    "dhcp6-overrides",
    "accept-ra",
    "link-local",
    // NE-3.9a: not address intent; cleared with the other IPv6 keys.
    "ipv6-privacy",
];
const OWNED: &[&str] = &[
    "/etc/netplan/90-ndimon.yaml",
    "/etc/netplan/91-ndimon-vlans.yaml",
];
impl Host {
    pub(crate) fn handoff_netplan_metadata(
        &self,
        p: &NetworkProfiles,
        adoption: &crate::handoff::Adoption,
        meta: &mut Metadata,
    ) -> Result<()> {
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        let (id, mut entry) = self.netplan_parent(&p.interface, &links)?;
        if id != adoption.parent_definition {
            return Err(unsupported("unrepresentable_configuration"));
        }
        let entry_map = entry
            .as_object_mut()
            .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
        for key in IP_KEYS {
            entry_map.remove(*key);
        }
        if entry_map.keys().any(|k| {
            ![
                "match",
                "set-name",
                "renderer",
                "mtu",
                "wakeonlan",
                "optional",
            ]
            .contains(&k.as_str())
        }) {
            return Err(unsupported("unrepresentable_configuration"));
        }
        meta.parent_definition = id;
        meta.parent_non_ip = entry;
        Ok(())
    }
    fn netplan_sources(&self) -> Result<Vec<String>> {
        let mut selected = BTreeMap::new();
        // Resolve only the documented usr-merge alias, never arbitrary linked
        // configuration ancestors. On non-merged systems use /lib directly.
        let lib = self.root.path("/lib")?;
        let vendor = match std::fs::symlink_metadata(&lib) {
            Ok(m) if m.file_type().is_symlink() => {
                let target =
                    std::fs::read_link(&lib).map_err(|_| unsupported("unsafe_configuration"))?;
                if target != std::path::Path::new("usr/lib")
                    && target != std::path::Path::new("/usr/lib")
                {
                    return Err(unsupported("unsafe_configuration"));
                }
                "/usr/lib/netplan"
            }
            Ok(_) => "/lib/netplan",
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "/usr/lib/netplan",
            Err(_) => return Err(unsupported("unsafe_configuration")),
        };
        for dir in [vendor, "/etc/netplan", "/run/netplan"] {
            for path in self.root.list(dir, None)? {
                if path.ends_with(".yaml") || path.ends_with(".yml") {
                    selected.insert(path.rsplit('/').next().unwrap_or("").to_owned(), path);
                }
            }
        }
        Ok(selected.into_values().collect())
    }
    /// Where a refused definition lives: its merged key path, which is what the
    /// key is relative to, and the YAML files that define it (`netplan get`
    /// merges them). Only the refusal path pays for this.
    fn netplan_locator(&self, id: &str) -> String {
        let files = self
            .netplan_sources()
            .unwrap_or_default()
            .into_iter()
            .filter(|path| {
                self.root
                    .text(path)
                    .ok()
                    .flatten()
                    .and_then(|text| crate::yaml::parse(&text).ok())
                    .is_some_and(|source| source["network"]["ethernets"].get(id).is_some())
            })
            .collect::<Vec<_>>();
        if files.is_empty() {
            format!("network.ethernets.{id}")
        } else {
            format!("network.ethernets.{id} ({})", files.join(", "))
        }
    }
    fn netplan_parent(&self, interface: &str, links: &Value) -> Result<(String, Value)> {
        let map =
            crate::yaml::parse(&self.run(Tool::Netplan, &["get", "network.ethernets"], 20)?)?;
        let map = map
            .as_object()
            .ok_or_else(|| unsupported("missing_parent_definition"))?;
        let mac = rows(links)?
            .iter()
            .find(|l| l["ifname"] == interface)
            .and_then(|l| l["address"].as_str())
            .unwrap_or("");
        let mut found = Vec::new();
        for (id, entry) in map {
            let selectors = entry.get("match").and_then(Value::as_object);
            if selectors.is_some_and(|m| {
                m.keys()
                    .any(|k| !["name", "macaddress"].contains(&k.as_str()))
            }) {
                return Err(unsupported("unrepresentable_configuration"));
            }
            let renamed = entry["set-name"] == interface;
            let name_match = if let Some(pattern) = entry["match"]["name"].as_str() {
                glob::Pattern::new(pattern)
                    .map_err(|_| unsupported("unsafe_configuration"))?
                    .matches(interface)
                    || renamed
            } else {
                true
            };
            let mac_match = entry["match"]["macaddress"]
                .as_str()
                .is_none_or(|m| m.eq_ignore_ascii_case(mac));
            let identifies = selectors.is_some_and(|m| !m.is_empty()) || renamed || id == interface;
            if identifies && name_match && mac_match {
                found.push((id.clone(), entry.clone()));
            }
        }
        if found.len() != 1 {
            return Err(unsupported(if found.is_empty() {
                "missing_parent_definition"
            } else {
                "ambiguous_parent_definition"
            }));
        }
        found
            .pop()
            .ok_or_else(|| unsupported("missing_parent_definition"))
    }
    pub(super) fn adopt_netplan(
        &self,
        interface: &str,
        links: &Value,
    ) -> Result<(NetworkProfile, Metadata)> {
        let (id, entry) = self.netplan_parent(interface, links)?;
        let renderer = if let Some(renderer) = entry["renderer"].as_str() {
            renderer.to_owned()
        } else {
            let v =
                crate::yaml::parse(&self.run(Tool::Netplan, &["get", "network.renderer"], 20)?)?;
            v.as_str().unwrap_or("networkd").to_owned()
        };
        if renderer != "networkd" {
            return Err(unsupported(if renderer == "NetworkManager" {
                "backend_not_managing_port"
            } else {
                "unsupported_renderer"
            }));
        }
        let (n, dhcp_metric, static_metric) =
            parse(&entry).map_err(|e| located(e, &self.netplan_locator(&id)))?;
        let mut non_ip = entry.clone();
        let obj = non_ip
            .as_object_mut()
            .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
        for key in IP_KEYS {
            obj.remove(*key);
        }
        let sources = self.netplan_sources()?;
        Ok((n, {
            let mut metadata = Metadata::default();
            metadata.parent_definition = id;
            metadata.parent_non_ip = non_ip;
            metadata.sources = sources;
            metadata.dhcp_metric = dhcp_metric;
            metadata.static_metric = static_metric;
            metadata
        }))
    }
    pub(crate) fn render_netplan(
        &self,
        old: &NetworkProfiles,
        p: &NetworkProfiles,
        meta: &Metadata,
        files: &mut Vec<FileChange>,
    ) -> Result<()> {
        self.render_networkd(old, p, meta, false, files)?;
        let links = self.json(Tool::Ip, &["-j", "-d", "link", "show"])?;
        let (id, effective) = self.netplan_parent(&p.interface, &links)?;
        if id != meta.parent_definition {
            return Err(unsupported("ambiguous_parent_definition"));
        }
        if effective["renderer"]
            .as_str()
            .is_some_and(|r| r != "networkd")
        {
            return Err(unsupported("unsupported_renderer"));
        }
        let mut sources_found = false;
        for path in self.netplan_sources()? {
            if self.handoff && self.handoff_paths.contains(&path) {
                sources_found = true;
                continue;
            }
            if OWNED.contains(&path.as_str()) {
                continue;
            }
            let text = self
                .root
                .text(&path)?
                .ok_or_else(|| unsupported("missing_active_configuration"))?;
            let mut source = crate::yaml::parse(&text)?;
            // Whole-file before-images must never retain Wi-Fi credentials.
            if source["network"].get("wifis").is_some() {
                return Err(unsupported("unsafe_configuration"));
            }
            if source["network"]["vlans"]
                .as_object()
                .is_some_and(|m| m.values().any(|v| v["link"] == id))
            {
                return Err(unsupported("foreign_vlan"));
            }
            if let Some(entry) = source["network"]["ethernets"]
                .get_mut(&id)
                .and_then(Value::as_object_mut)
            {
                sources_found = true;
                let mut changed = false;
                for key in IP_KEYS {
                    changed |= entry.remove(*key).is_some();
                }
                if changed {
                    add(
                        &self.root,
                        files,
                        &path,
                        Some(&crate::yaml::emit(&source)?),
                        0o600,
                    )?;
                }
            }
        }
        if !sources_found && !self.root.exists(OWNED[0])? {
            return Err(unsupported("missing_parent_definition"));
        }
        let mut parent = meta.parent_non_ip.clone();
        let obj = parent
            .as_object_mut()
            .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
        obj.extend(
            entry(p.networks.iter().find(|n| n.vlan_id.is_none()), p, meta)
                .as_object()
                .ok_or_else(|| unsupported("unrepresentable_configuration"))?
                .clone(),
        );
        let ethernet = json!({"network":{"version":2,"ethernets":{id.clone():parent}}});
        let mut vlans = serde_json::Map::new();
        for n in p.networks.iter().filter(|n| n.vlan_id.is_some()) {
            let mut v = entry(Some(n), p, meta);
            v["id"] = json!(n.vlan_id);
            v["link"] = json!(id);
            vlans.insert(ifname(p, n), v);
        }
        add(
            &self.root,
            files,
            OWNED[0],
            Some(&crate::yaml::emit(&ethernet)?),
            0o600,
        )?;
        add(
            &self.root,
            files,
            OWNED[1],
            Some(&crate::yaml::emit(
                &json!({"network":{"version":2,"vlans":vlans}}),
            )?),
            0o600,
        )
    }
}
fn entry(n: Option<&NetworkProfile>, p: &NetworkProfiles, meta: &Metadata) -> Value {
    let mut v = json!({"dhcp4":false,"dhcp6":false,"addresses":[],"routes":[],"nameservers":{"addresses":[]},"optional":true,"accept-ra":false,"link-local":[]});
    let Some(n) = n else {
        return v;
    };
    let management = n.id == p.roles.management;
    v["dhcp4-overrides"] = json!({"use-routes":management,"use-dns":management,"use-ntp":false,"use-hostname":false,"route-metric":meta.dhcp_metric});
    if n.mode == AddressMode::Dhcp {
        v["dhcp4"] = json!(true);
    } else {
        v["addresses"] = json!([format!("{}/{}", n.address, n.prefix)]);
        v["nameservers"]["addresses"] = json!(n.dns);
        if management && !n.gateway.is_empty() {
            v["routes"] = json!([{"to":"default","via":n.gateway,"metric":meta.static_metric}]);
        }
    }
    // NE-2.12a: management only, DHCP or static; the key is omitted when empty.
    if management && !n.dns_search.is_empty() {
        v["nameservers"]["search"] = json!(n.dns_search);
    }
    v
}
/// NE-3.9: preserve a single representable default route and its metric.
pub fn parse(v: &Value) -> Result<(NetworkProfile, u32, u32)> {
    let obj = v
        .as_object()
        .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
    let allowed = [
        "dhcp4",
        "dhcp6",
        "addresses",
        "routes",
        "gateway4",
        "gateway6",
        "nameservers",
        "dhcp4-overrides",
        "dhcp6-overrides",
        "accept-ra",
        "link-local",
        "ipv6-privacy",
        "optional",
        "match",
        "set-name",
        "renderer",
        "mtu",
        "wakeonlan",
    ];
    if let Some(key) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(unrepresentable(key));
    }
    // NE-3.9a (U17): the privacy-extension switch is a boolean, never address intent.
    if v.get("ipv6-privacy").is_some_and(|p| !p.is_boolean()) {
        return Err(unrepresentable("ipv6-privacy"));
    }
    // A bare `dhcp6: true` is the distribution's implicit IPv6 default (stock
    // Armbian sets it with `ipv6-privacy` on every Ethernet block, HW-1 H3), not
    // operator address intent: managed output turns it off and Revert restores
    // it. Static IPv6 addresses, DNS, `gateway6`, `dhcp6-overrides` and an
    // explicit `accept-ra: true` still refuse below.
    if !v["gateway6"].is_null() || v.get("dhcp6-overrides").is_some() || v["accept-ra"] == true {
        return Err(unsupported("ipv6_configuration"));
    }
    let mut n = NetworkProfile::dhcp("untagged", "Untagged");
    let addresses = list(v, "addresses")?;
    let routes = list(v, "routes")?;
    if routes.len() > 1 || !routes.is_empty() && v.get("gateway4").is_some() {
        return Err(unsupported("multiple_gateways"));
    }
    // NE-2.12a: `search` is the management row's search list (DHCP or static);
    // any other nameservers key stays unrepresentable.
    let (dns, search) = if v["nameservers"].is_null() {
        (Vec::new(), Vec::new())
    } else {
        let nameservers = v["nameservers"]
            .as_object()
            .ok_or_else(|| unrepresentable("nameservers"))?;
        if let Some(key) = nameservers
            .keys()
            .find(|k| !["addresses", "search"].contains(&k.as_str()))
        {
            return Err(unrepresentable(format!("nameservers.{key}")));
        }
        let search = list(&v["nameservers"], "search")
            .map_err(|_| unrepresentable("nameservers.search"))?
            .into_iter()
            .map(|d| {
                d.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| unrepresentable("nameservers.search"))
            })
            .collect::<Result<Vec<_>>>()?;
        (
            list(&v["nameservers"], "addresses")
                .map_err(|_| unrepresentable("nameservers.addresses"))?,
            search_list(search, "nameservers.search")?,
        )
    };
    let overrides = v.get("dhcp4-overrides").and_then(Value::as_object);
    if let Some(key) = overrides.and_then(|m| {
        m.keys().find(|k| {
            ![
                "route-metric",
                "use-routes",
                "use-dns",
                "use-ntp",
                "use-hostname",
                "use-domains",
            ]
            .contains(&k.as_str())
        })
    }) {
        return Err(unrepresentable(format!("dhcp4-overrides.{key}")));
    }
    // NE-2.12a: lease domains are not desired state; only an explicit false is
    // representable (`true` and `route` both use them).
    if v["dhcp4-overrides"]
        .get("use-domains")
        .is_some_and(|d| *d != false)
    {
        return Err(unrepresentable("dhcp4-overrides.use-domains"));
    }
    let metric = v["dhcp4-overrides"]
        .get("route-metric")
        .map(|v| {
            v.as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| unrepresentable("dhcp4-overrides.route-metric"))
        })
        .transpose()?
        .unwrap_or(100);
    let mut static_metric = 0;
    let mut gateways = BTreeSet::new();
    if let Some(g) = v["gateway4"].as_str() {
        gateways.insert(g.to_owned());
    }
    for r in routes {
        if r["to"] != "default" && r["to"] != "0.0.0.0/0" {
            return Err(unsupported("custom_routes"));
        }
        if r.as_object().is_none_or(|m| {
            m.keys()
                .any(|k| !["to", "via", "metric"].contains(&k.as_str()))
        }) {
            return Err(unsupported("custom_routes"));
        }
        gateways.insert(
            r["via"]
                .as_str()
                .ok_or_else(|| unsupported("custom_routes"))?
                .into(),
        );
        static_metric = r
            .get("metric")
            .map(|v| {
                v.as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| unrepresentable("routes.metric"))
            })
            .transpose()?
            .unwrap_or(0);
    }
    if gateways.len() > 1 {
        return Err(unsupported("multiple_gateways"));
    }
    if addresses
        .iter()
        .chain(dns.iter())
        .any(|v| v.as_str().is_some_and(|s| s.contains(':')))
        || gateways.iter().any(|s| s.contains(':'))
    {
        return Err(unsupported("ipv6_configuration"));
    }
    if v["dhcp4"] == true {
        if !addresses.is_empty() || !gateways.is_empty() || !dns.is_empty() {
            return Err(unsupported("mixed_dhcp_static"));
        }
    } else {
        if addresses.len() != 1 {
            return Err(unsupported("multiple_addresses"));
        }
        let (address, prefix) = addresses[0]
            .as_str()
            .and_then(|v| v.split_once('/'))
            .ok_or_else(|| unrepresentable("addresses"))?;
        n.mode = AddressMode::Static;
        n.address = address.into();
        n.prefix = prefix.parse().map_err(|_| unrepresentable("addresses"))?;
        n.gateway = gateways.into_iter().next().unwrap_or_default();
        n.dns = dns
            .iter()
            .map(|d| {
                d.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| unsupported("ipv6_configuration"))
            })
            .collect::<Result<Vec<_>>>()?;
    }
    n.dns_search = search;
    Ok((n, metric, static_metric))
}
fn list<'a>(v: &'a Value, key: &str) -> Result<Vec<&'a Value>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => Ok(a.iter().collect()),
        _ => Err(unrepresentable(key)),
    }
}

impl Host {
    pub fn netplan_drift(&self, p: &NetworkProfiles) -> Result<Vec<Foreign>> {
        self.netplan_drift_sources(p, self.netplan_sources()?)
    }
    pub(crate) fn netplan_drift_sources(
        &self,
        p: &NetworkProfiles,
        sources: Vec<String>,
    ) -> Result<Vec<Foreign>> {
        let mut drift = Vec::new();
        for path in sources {
            if OWNED.contains(&path.as_str()) {
                continue;
            }
            // Apply/recovery can remove R's YAML after this observer lists it.
            // An absent competing source is no longer drift; strict plan and
            // adoption reads still require their explicitly selected sources.
            let Some(text) = self.root.text(&path)? else {
                continue;
            };
            let source = crate::yaml::parse(&text)?;
            if source["network"].get("wifis").is_some() {
                return Err(unsupported("unsafe_configuration"));
            }
            if let Some(entries) = source["network"]["ethernets"].as_object() {
                for (id, entry) in entries {
                    for name in names(p) {
                        let pattern = entry["match"]["name"].as_str();
                        let name_match = pattern
                            .map(|s| glob::Pattern::new(s).map(|p| p.matches(&name)))
                            .transpose()
                            .map_err(|_| unsupported("unsafe_configuration"))?
                            .unwrap_or(id == &name || entry["set-name"] == name);
                        let mac = self.link_mac(&name)?;
                        let mac_match = entry["match"]["macaddress"]
                            .as_str()
                            .is_none_or(|m| m.eq_ignore_ascii_case(&mac));
                        if name_match && mac_match {
                            drift.push({
                                let mut drift = Foreign::new(ndimon_types::Domain::Network, "");
                                drift.path = path.clone();
                                drift.artifact = Some(id.clone());
                                drift.selector = format!(
                                    "name={};mac={}",
                                    pattern.unwrap_or(id),
                                    entry["match"]["macaddress"].as_str().unwrap_or("")
                                );
                                drift.interface = name;
                                drift.neutralized = true;
                                drift.blocking = false;
                                drift.foreign = Some(drift.path.clone());
                                drift
                            });
                        }
                    }
                }
            }
        }
        Ok(drift)
    }
}
