use super::*;
use crate::store::random_id;
// Exactly the bounded takeover properties; never request connection secrets.
const FIELDS: &[&str] = &[
    "connection.interface-name",
    "connection.autoconnect",
    "connection.wait-device-timeout",
    "ipv4.method",
    "ipv4.addresses",
    "ipv4.gateway",
    "ipv4.dns",
    "ipv4.dns-search",
    "ipv4.never-default",
    "ipv4.ignore-auto-dns",
    "ipv4.ignore-auto-routes",
    "ipv4.route-metric",
    "ipv4.may-fail",
    "ipv4.dhcp-send-hostname",
    "ipv6.method",
    // NE-3.9a: recorded so Revert restores it; inert while `ipv6.method` is
    // `disabled`, and a Netplan write-back may rewrite it (HW-2 D8).
    "ipv6.ip6-privacy",
];
/// What is read back after a write: the properties that decide addressing and
/// routing, in a form `nmcli` prints the way it was given. A profile that a
/// plugin stores without keeping one of them must not be activated as if it had.
const VERIFIED: &[&str] = &[
    "ipv4.method",
    "ipv4.addresses",
    "ipv4.gateway",
    "ipv4.dns",
    "ipv4.dns-search",
    "ipv4.never-default",
    "ipv4.ignore-auto-dns",
    "ipv4.ignore-auto-routes",
    "ipv4.route-metric",
    "ipv6.method",
];
/// NetworkManager's own default metric for an Ethernet device, the value
/// `ipv4.route-metric = -1` stands for (nm-settings-nmcli(5)).
const NM_ETHERNET_METRIC: u32 = 100;
/// Where NetworkManager keeps profile files; a Netplan-generated one starts in
/// `/run` and moves to `/etc` (or to Netplan YAML) when it is modified.
const NM_DIRS: &[&str] = &[
    "/etc/NetworkManager/system-connections",
    "/run/NetworkManager/system-connections",
];
/// Where Netplan, NetworkManager's Netplan plugin and NetworkManager itself keep
/// a profile; every file here that names the parent's UUID is part of it
/// (network-engine-spec NE-4.9).
const PROFILE_DIRS: &[&str] = &[
    "/etc/netplan",
    "/run/netplan",
    "/etc/NetworkManager/system-connections",
    "/run/NetworkManager/system-connections",
];
/// How long NetworkManager gets to settle (after `connection up`, after a
/// `reload` that makes its Netplan writer re-save a file) before what it
/// reports is judged. Bounded further by the host's own budget.
const SETTLE_MS: u64 = 15_000;
const SETTLE_STEP_MS: u64 = 250;
impl Host {
    /// Run `check` until it finds nothing wrong or `SETTLE_MS` have passed; the
    /// last finding is returned. NetworkManager answers a read right after
    /// `nmcli connection up` with the state it is leaving (NI-17 D15), so a
    /// mismatch is only one once it has persisted.
    fn nm_settled(
        &self,
        mut check: impl FnMut() -> Result<Option<String>>,
    ) -> Result<Option<String>> {
        let end = self
            .clock
            .now_ms()?
            .saturating_add(SETTLE_MS)
            .min(self.deadline.unwrap_or(u64::MAX));
        loop {
            let wrong = check()?;
            if wrong.is_none() || self.clock.now_ms()? >= end {
                return Ok(wrong);
            }
            self.clock.sleep_ms(SETTLE_STEP_MS);
        }
    }
    /// NI-17 D14. NetworkManager's Netplan write-back (Raspberry Pi OS, Ubuntu)
    /// re-saves a YAML it did not write itself within a second of a `connection
    /// reload` (it adds `ipv6.ip6-privacy: "-1"`, say), and the YAML left by an
    /// earlier `nmcli modify` of ours is such a file. A before-image taken as it
    /// lies would never be what NetworkManager keeps after a restore. So the
    /// before-images of a Netplan-owned profile are taken after one reload, when
    /// NetworkManager has said its piece; the restore then puts back a file it
    /// leaves alone. True when an image changed.
    pub(crate) fn settle_nm_files(&self, changes: &mut [NmChange]) -> Result<bool> {
        let written_back = changes.iter().any(|c| {
            c.files
                .iter()
                .any(|f| f.path.starts_with("/etc/netplan/") && f.before.is_some())
        });
        if !written_back {
            return Ok(false);
        }
        self.run(Tool::Nmcli, &["connection", "reload"], 20)?;
        let start = self.clock.now_ms()?;
        let end = start
            .saturating_add(SETTLE_MS)
            .min(self.deadline.unwrap_or(u64::MAX));
        let mut last: Option<Vec<Option<crate::store::Image>>> = None;
        let images = loop {
            self.clock.sleep_ms(SETTLE_STEP_MS);
            let mut now = Vec::new();
            for change in changes.iter() {
                for file in &change.files {
                    now.push(self.root.snapshot(&file.path)?);
                }
            }
            let at = self.clock.now_ms()?;
            // At least a second after the reload, and two equal looks in a row.
            if (at.saturating_sub(start) >= 1000 && last.as_ref() == Some(&now)) || at >= end {
                break now;
            }
            last = Some(now);
        };
        let mut changed = false;
        let mut images = images.into_iter();
        for change in changes.iter_mut() {
            for file in &mut change.files {
                let image = images.next().flatten();
                if image != file.before {
                    if image
                        .as_ref()
                        .is_some_and(|image| holds_credentials(&image.bytes))
                    {
                        return Err(unsupported(&format!(
                            "unsafe_configuration: {} holds credentials",
                            file.path
                        )));
                    }
                    file.before = image;
                    changed = true;
                }
            }
        }
        Ok(changed)
    }
    /// Delete the connections a change created, and the files NetworkManager
    /// made for them. The way out of a failed recovery (NI-17) uses it; the
    /// recovery itself does the same one connection at a time.
    pub(super) fn discard_created_nm(&self, changes: &[NmChange]) -> Result<()> {
        let present = self.nm_uuids()?;
        let mut reload = false;
        for change in changes.iter().filter(|c| c.before.is_none()) {
            if present.contains(&change.uuid) {
                self.run(
                    Tool::Nmcli,
                    &["connection", "delete", "uuid", &change.uuid],
                    20,
                )?;
            }
            reload |= self.restore_nm_files(&change.files)?;
        }
        if reload {
            self.run(Tool::Nmcli, &["connection", "reload"], 20)?;
        }
        Ok(())
    }
    /// After a restore and a reload: the profile's files are the before-images
    /// again (`profile_file_restore_mismatch`). NetworkManager saves a profile
    /// whenever it activates it and writes its own bookkeeping each time (the
    /// activation timestamp, the default privacy value), so a restored file is
    /// the same file when only those lines differ (HW-3 D14); anything else is
    /// not, after the bounded wait for NetworkManager to finish saving.
    fn verify_files(&self, change: &NmChange) -> Result<()> {
        if change.files.is_empty() {
            return Ok(());
        }
        let wrong = self.nm_settled(|| {
            for file in &change.files {
                let now = self.root.snapshot(&file.path)?;
                if !same_profile(now.as_ref(), file.before.as_ref()) {
                    return Ok(Some(file.path.clone()));
                }
            }
            Ok(None)
        })?;
        match wrong {
            None => Ok(()),
            Some(path) => {
                let id = self
                    .nm_field(&change.uuid, "connection.id")
                    .unwrap_or_default();
                Err(failed(
                    "verify_backend",
                    &format!(
                        "profile_file_restore_mismatch: {path} (connection {id} ({}))",
                        change.uuid
                    ),
                ))
            }
        }
    }
    /// After a restore: the connection reads as it did before the change. The
    /// addressing properties are compared as `nmcli` prints them (the files are
    /// restored byte for byte, but NetworkManager may save them again, so the
    /// bytes are not what is judged); the failure names the connection and the
    /// property.
    fn verify_restored(&self, change: &NmChange) -> Result<()> {
        let Some(before) = &change.before else {
            return Ok(());
        };
        let wrong = self.nm_settled(|| {
            let mut differing = Vec::new();
            for field in VERIFIED {
                let Some(want) = before.get(*field) else {
                    continue;
                };
                let got = self.nm_field(&change.uuid, field)?;
                if !same_value(field, want, &got) {
                    differing.push(format!("{field} reads '{got}', expected '{want}'"));
                }
            }
            Ok((!differing.is_empty()).then(|| differing.join("; ")))
        })?;
        match wrong {
            None => Ok(()),
            Some(what) => {
                let id = self
                    .nm_field(&change.uuid, "connection.id")
                    .unwrap_or_default();
                Err(failed(
                    "verify_backend",
                    &format!(
                        "connection_restore_mismatch: connection {id} ({}) {what}",
                        change.uuid
                    ),
                ))
            }
        }
    }
    pub(super) fn import_nm(&self, p: &NetworkProfiles) -> Result<Metadata> {
        let mut meta = Metadata::default();
        for name in names(p) {
            let uuid = self
                .run(
                    Tool::Nmcli,
                    &["-g", "GENERAL.CON-UUID", "device", "show", &name],
                    20,
                )?
                .trim()
                .to_owned();
            if !valid_uuid(&uuid) {
                return Err(unsupported("missing_parent_connection"));
            }
            if name == p.interface {
                meta.nm_parent_uuid = Some(uuid.clone());
            } else {
                meta.nm_owned.insert(name.clone(), uuid.clone());
            }
            if name == ifname(p, assigned(p, &p.roles.management)?) {
                let raw = self.nm_field(&uuid, "ipv4.route-metric")?;
                let metric = self.nm_metric(&uuid, &name, &raw)?;
                meta.dhcp_metric = metric;
                meta.static_metric = metric;
            }
        }
        Ok(meta)
    }
    /// `ipv4.route-metric`. A number is the profile's own choice. `-1` leaves it
    /// to NetworkManager, which uses its device-type default: 100 for Ethernet,
    /// the value every stock Raspberry Pi OS and Netplan-generated profile has
    /// (NE-4.4: the metric the backend would have installed). When the profile is
    /// active on `device` and has a default route, the metric NetworkManager
    /// really installed must agree, so a connection default set in `conf.d`
    /// is refused instead of guessed.
    fn nm_metric(&self, uuid: &str, device: &str, raw: &str) -> Result<u32> {
        if let Ok(metric) = raw.parse::<u32>() {
            return Ok(metric);
        }
        if raw != "-1" {
            return Err(unrepresentable("ipv4.route-metric"));
        }
        let active = self.run(
            Tool::Nmcli,
            &["-g", "GENERAL.CON-UUID", "device", "show", device],
            20,
        )?;
        if active.trim() == uuid {
            let routes = self.run(
                Tool::Nmcli,
                &["-g", "IP4.ROUTE", "device", "show", device],
                20,
            )?;
            // `dst = 0.0.0.0/0, nh = 192.0.2.1, mt = 100 | dst = ...`
            for route in routes.split(['|', '\n']) {
                if !route.contains("dst = 0.0.0.0/0") {
                    continue;
                }
                let live = route
                    .split(',')
                    .find_map(|part| part.trim().strip_prefix("mt = "))
                    .and_then(|mt| mt.trim().parse::<u32>().ok());
                if live != Some(NM_ETHERNET_METRIC) {
                    return Err(unrepresentable("ipv4.route-metric"));
                }
            }
        }
        Ok(NM_ETHERNET_METRIC)
    }
    /// The file NetworkManager loaded this connection from, or empty.
    fn nm_filename(&self, uuid: &str) -> Result<String> {
        // An nmcli too old to list FILENAME only loses the Netplan detection by
        // name; the YAML check in `nm_netplan_owned` stays, and every other
        // property read still fails loudly if NetworkManager is unusable.
        let text = self
            .run(
                Tool::Nmcli,
                &[
                    "--escape",
                    "no",
                    "-g",
                    "UUID,FILENAME",
                    "connection",
                    "show",
                ],
                20,
            )
            .unwrap_or_default();
        // The UUID leads and has a fixed length: the path may hold any character.
        Ok(text
            .lines()
            .find_map(|l| l.strip_prefix(uuid).and_then(|r| r.strip_prefix(':')))
            .unwrap_or("")
            .to_owned())
    }
    /// Does Netplan own this profile? Its generator names the keyfile
    /// `netplan-<id>`, and NetworkManager's Netplan integration keeps a modified
    /// one in `/etc/netplan/90-NM-<uuid>.yaml`. Returns that and the file name.
    fn nm_netplan_owned(&self, uuid: &str) -> Result<(bool, String)> {
        let filename = self.nm_filename(uuid)?;
        let generated = filename
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with("netplan-"));
        let owned = generated
            || self
                .root
                .exists(&format!("/etc/netplan/90-NM-{uuid}.yaml"))?;
        Ok((owned, filename))
    }
    /// Before-images of every file this connection may live in. The YAML, the
    /// keyfile it came from and both keyfile names NetworkManager may move it to
    /// (absent files are recorded as absent, so Revert removes what appeared).
    /// Never a file with credentials: a whole-file image would keep them.
    fn nm_persisted(&self, uuid: &str, id: &str, filename: &str) -> Result<Vec<NmFile>> {
        let mut paths = vec![format!("/etc/netplan/90-NM-{uuid}.yaml")];
        if NM_DIRS.iter().any(|dir| {
            filename
                .strip_prefix(dir)
                .is_some_and(|r| r.starts_with('/'))
        }) && !filename.contains("/../")
        {
            paths.push(filename.to_owned());
        }
        if !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" ._-".contains(&b))
        {
            paths.extend(NM_DIRS.iter().map(|dir| format!("{dir}/{id}.nmconnection")));
        }
        // Whatever else names this profile's UUID, wherever the plugin and the
        // generator keep it. A file that cannot be read safely is not ours to
        // restore; the names above stay strict.
        for directory in PROFILE_DIRS {
            for path in self.root.list(directory, None).unwrap_or_default() {
                if let Ok(Some(image)) = self.root.snapshot(&path) {
                    if String::from_utf8_lossy(&image.bytes).contains(uuid) {
                        paths.push(path);
                    }
                }
            }
        }
        paths.sort();
        paths.dedup();
        let mut files = Vec::new();
        for path in paths {
            let before = self.root.snapshot(&path)?;
            if before
                .as_ref()
                .is_some_and(|image| holds_credentials(&image.bytes))
            {
                return Err(unsupported(&format!(
                    "unsafe_configuration: {path} holds credentials"
                )));
            }
            files.push(NmFile { path, before });
        }
        Ok(files)
    }
    /// Put the persisted files back byte for byte; true when any differed.
    fn restore_nm_files(&self, files: &[NmFile]) -> Result<bool> {
        let mut changed = false;
        for file in files {
            if self.root.snapshot(&file.path)? != file.before {
                self.root.replace(&file.path, file.before.as_ref())?;
                changed = true;
            }
        }
        Ok(changed)
    }
    /// Read back what was just written. A Netplan-owned profile keeps what its
    /// YAML can express; the binding (`connection.interface-name`) is not in
    /// `VERIFIED`, and is not written for such a parent at all.
    fn verify_written(&self, change: &NmChange) -> Result<()> {
        for field in VERIFIED {
            let Some(want) = change.after.get(*field) else {
                continue;
            };
            let got = self.nm_field(&change.uuid, field)?;
            if !same_value(field, want, &got) {
                let id = self
                    .nm_field(&change.uuid, "connection.id")
                    .unwrap_or_default();
                return Err(failed(
                    "verify_backend",
                    &format!(
                        "profile_property_not_applied: connection {id} ({}) {field} reads '{got}', expected '{want}'",
                        change.uuid
                    ),
                ));
            }
        }
        Ok(())
    }
    fn nm_field(&self, uuid: &str, field: &str) -> Result<String> {
        if !valid_uuid(uuid) {
            return Err(unsupported("missing_parent_connection"));
        }
        Ok(self
            .run(
                Tool::Nmcli,
                &[
                    "--escape",
                    "no",
                    "-g",
                    field,
                    "connection",
                    "show",
                    "uuid",
                    uuid,
                ],
                20,
            )?
            .trim()
            .to_owned())
    }
    fn nm_snapshot(&self, uuid: &str) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        for field in FIELDS {
            out.insert((*field).into(), self.nm_field(uuid, field)?);
        }
        Ok(out)
    }
    fn nm_uuids(&self) -> Result<Vec<String>> {
        let text = self.run(Tool::Nmcli, &["-g", "UUID", "connection", "show"], 20)?;
        let mut ids = Vec::new();
        for id in text.lines().filter(|s| !s.is_empty()) {
            if !valid_uuid(id) {
                return Err(unsupported("foreign_connection"));
            }
            ids.push(id.to_owned());
        }
        Ok(ids)
    }
    pub(super) fn adopt_nm(
        &self,
        interface: &str,
        uuid: &str,
    ) -> Result<(NetworkProfile, Metadata)> {
        let kind = self.nm_field(uuid, "connection.type")?;
        if kind != "802-3-ethernet" {
            return Err(unsupported("wired_port_required"));
        }
        let binding = self.nm_field(uuid, "connection.interface-name")?;
        if !binding.is_empty() && binding != interface {
            return Err(unsupported("foreign_connection"));
        }
        // The refusal names the property; `adopt_known` names the connection.
        for field in ["ipv4.routes", "ipv4.routing-rules"] {
            if !empty(&self.nm_field(uuid, field)?) {
                return Err(unsupported(&format!(
                    "{}: {field}",
                    if field == "ipv4.routes" {
                        "custom_routes"
                    } else {
                        "policy_routing"
                    }
                )));
            }
        }
        // NE-3.9a: `ipv6.ip6-privacy` is not among these. It is a privacy choice,
        // not address intent; it is recorded in the snapshot and left alone.
        for field in [
            "ipv6.addresses",
            "ipv6.gateway",
            "ipv6.dns",
            "ipv6.routes",
            "ipv6.routing-rules",
        ] {
            if !empty(&self.nm_field(uuid, field)?) {
                return Err(unsupported(&format!("ipv6_configuration: {field}")));
            }
        }
        let ipv6 = self.nm_field(uuid, "ipv6.method")?;
        if !["auto", "disabled", "ignore"].contains(&ipv6.as_str()) {
            return Err(unsupported(&format!(
                "ipv6_configuration: ipv6.method is {ipv6}"
            )));
        }
        let properties = self.nm_snapshot(uuid)?;
        let mut n = NetworkProfile::dhcp("untagged", "Untagged");
        let addresses = split(
            properties
                .get("ipv4.addresses")
                .map(String::as_str)
                .unwrap_or(""),
        );
        let gateway = properties
            .get("ipv4.gateway")
            .filter(|s| !empty(s))
            .cloned()
            .unwrap_or_default();
        let dns = split(properties.get("ipv4.dns").map(String::as_str).unwrap_or(""));
        // NE-2.12a: the connection's configured property, never lease data
        // (`IP4.DOMAIN`), so a DHCP server's domains are not adopted as intent.
        n.dns_search = search_list(
            split(
                properties
                    .get("ipv4.dns-search")
                    .map(String::as_str)
                    .unwrap_or(""),
            ),
            "ipv4.dns-search",
        )?;
        match properties.get("ipv4.method").map(String::as_str) {
            Some("auto") => {
                if !addresses.is_empty() || !gateway.is_empty() || !dns.is_empty() {
                    return Err(unsupported("mixed_dhcp_static"));
                }
                for field in [
                    "ipv4.dhcp-hostname",
                    "ipv4.dhcp-client-id",
                    "ipv4.dhcp-fqdn",
                ] {
                    if !empty(&self.nm_field(uuid, field)?) {
                        return Err(unrepresentable(field));
                    }
                }
                for field in [
                    "ipv4.never-default",
                    "ipv4.ignore-auto-dns",
                    "ipv4.ignore-auto-routes",
                ] {
                    if properties.get(field).is_some_and(|s| s == "yes") {
                        return Err(unrepresentable(field));
                    }
                }
            }
            Some("manual") => {
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
                n.gateway = gateway;
                n.dns = dns;
            }
            _ => return Err(unsupported("unrepresentable_configuration")),
        }
        let metric = properties
            .get("ipv4.route-metric")
            .ok_or_else(|| unrepresentable("ipv4.route-metric"))?;
        // `-1` is NetworkManager's own device default (see `nm_metric`).
        let metric = self.nm_metric(uuid, interface, metric)?;
        let meta = {
            let mut metadata = Metadata::default();
            metadata.nm_parent_uuid = Some(uuid.into());
            metadata.dhcp_metric = metric;
            metadata.static_metric = metric;
            metadata
        };
        let mut probe = NetworkProfiles::default();
        probe.interface = interface.into();
        probe.backend = Some(NetworkBackend::Networkmanager);
        if !self.nm_drift(&probe, &meta)?.is_empty() {
            return Err(unsupported("foreign_connection"));
        }
        Ok((n, meta))
    }
    pub fn nm_drift(&self, p: &NetworkProfiles, meta: &Metadata) -> Result<Vec<Foreign>> {
        let mut drift = Vec::new();
        let base_mac = self.link_mac(&p.interface)?;
        for uuid in self.nm_uuids()? {
            if meta.nm_parent_uuid.as_ref() == Some(&uuid)
                || meta.nm_owned.values().any(|id| id == &uuid)
            {
                continue;
            }
            let kind = self.nm_field(&uuid, "connection.type")?;
            if kind != "802-3-ethernet" && kind != "vlan" {
                continue;
            }
            let binding = self.nm_field(&uuid, "connection.interface-name")?;
            let id = self.nm_field(&uuid, "connection.id")?;
            let autoconnect = self.nm_field(&uuid, "connection.autoconnect")?;
            let mac = if kind == "802-3-ethernet" {
                self.nm_field(&uuid, "802-3-ethernet.mac-address")?
            } else {
                String::new()
            };
            let parent = if kind == "vlan" {
                self.nm_field(&uuid, "vlan.parent")?
            } else {
                String::new()
            };
            let competing = names(p).contains(&binding)
                || binding.is_empty() && (mac.is_empty() || mac.eq_ignore_ascii_case(&base_mac))
                || parent == p.interface
                || meta.nm_parent_uuid.as_ref().is_some_and(|s| s == &parent)
                || p.networks
                    .iter()
                    .any(|n| id == format!("ndimon-vlan-{}", ifname(p, n)));
            if competing {
                drift.push({ let mut drift = Foreign::new(ndimon_types::Domain::Network, ""); drift.path = uuid.clone(); drift.artifact = Some(id); drift.selector = format!("type={kind};ifname={binding};parent={parent};autoconnect={autoconnect};mac={mac}"); drift.interface = if binding.is_empty() { p.interface.clone() } else { binding }; drift.neutralized = false; drift.blocking = true; drift.foreign = Some("/etc/NetworkManager/system-connections".into()); drift });
            }
        }
        Ok(drift)
    }
    pub(super) fn plan_nm(
        &self,
        old: &NetworkProfiles,
        p: &NetworkProfiles,
        meta: &Metadata,
    ) -> Result<Vec<NmChange>> {
        let uuid = meta
            .nm_parent_uuid
            .as_ref()
            .ok_or_else(|| unsupported("missing_parent_connection"))?;
        let mut changes = Vec::new();
        let live = self.run(
            Tool::Nmcli,
            &["-g", "UUID", "connection", "show", "--active"],
            20,
        )?;
        // A Netplan-owned parent cannot keep `connection.interface-name`: its
        // profile has `match: {}` and the plugin drops the binding (HW-2 D2).
        // It is not written there, and its files are restored byte for byte.
        let (netplan_owned, parent_file) = self.nm_netplan_owned(uuid)?;
        let mut add_change = |uuid: String,
                              interface: String,
                              tag: Option<u16>,
                              n: Option<&NetworkProfile>,
                              existed: bool,
                              removed: bool|
         -> Result<()> {
            let before = if existed {
                Some(self.nm_snapshot(&uuid)?)
            } else {
                None
            };
            let active = live.lines().any(|s| s == uuid);
            let after = if removed {
                BTreeMap::from([("connection.autoconnect".into(), "no".into())])
            } else {
                settings(n, p, meta, &interface, tag.is_some() || !netplan_owned)
            };
            let files = if netplan_owned {
                let (id, filename) = match (tag, existed) {
                    (None, _) => (self.nm_field(&uuid, "connection.id")?, parent_file.clone()),
                    (Some(_), true) => (
                        self.nm_field(&uuid, "connection.id")?,
                        self.nm_filename(&uuid)?,
                    ),
                    (Some(_), false) => (format!("ndimon-vlan-{interface}"), String::new()),
                };
                self.nm_persisted(&uuid, &id, &filename)?
            } else {
                Vec::new()
            };
            changes.push(NmChange {
                uuid,
                interface,
                tag,
                before,
                active,
                after,
                remove_after_commit: removed,
                files,
            });
            Ok(())
        };
        add_change(
            uuid.clone(),
            p.interface.clone(),
            None,
            p.networks.iter().find(|n| n.vlan_id.is_none()),
            true,
            false,
        )?;
        for n in p.networks.iter().filter(|n| n.vlan_id.is_some()) {
            let interface = ifname(p, n);
            let (uuid, existed) = if let Some(uuid) = meta.nm_owned.get(&interface) {
                (uuid.clone(), true)
            } else {
                (new_uuid()?, false)
            };
            add_change(uuid, interface, n.vlan_id, Some(n), existed, false)?;
        }
        for n in old
            .networks
            .iter()
            .filter(|n| n.vlan_id.is_some() && !p.networks.iter().any(|m| m.vlan_id == n.vlan_id))
        {
            let interface = ifname(old, n);
            add_change(
                meta.nm_owned
                    .get(&interface)
                    .ok_or_else(|| unsupported("foreign_connection"))?
                    .clone(),
                interface,
                n.vlan_id,
                None,
                true,
                true,
            )?;
        }
        Ok(changes)
    }
    pub(super) fn activate_nm(&self, changes: &[NmChange], restore: bool) -> Result<()> {
        let present = self.nm_uuids()?;
        for change in changes {
            if restore && change.before.is_none() {
                if present.contains(&change.uuid) {
                    self.run(
                        Tool::Nmcli,
                        &["connection", "delete", "uuid", &change.uuid],
                        20,
                    )?;
                }
                // The Netplan plugin writes a YAML for a new connection; if
                // deleting it left one, it is removed (it was absent before).
                if self.restore_nm_files(&change.files)? {
                    self.run(Tool::Nmcli, &["connection", "reload"], 20)?;
                }
                self.verify_files(change)?;
                continue;
            }
            if !restore && change.before.is_none() && !present.contains(&change.uuid) {
                let tag = change
                    .tag
                    .ok_or_else(|| unsupported("foreign_connection"))?
                    .to_string();
                let parent = change
                    .interface
                    .rsplit_once('.')
                    .ok_or_else(|| unsupported("foreign_connection"))?
                    .0;
                self.run(
                    Tool::Nmcli,
                    &[
                        "connection",
                        "add",
                        "type",
                        "vlan",
                        "con-name",
                        &format!("ndimon-vlan-{}", change.interface),
                        "ifname",
                        &change.interface,
                        "dev",
                        parent,
                        "id",
                        &tag,
                        "connection.uuid",
                        &change.uuid,
                    ],
                    20,
                )?;
            }
            let values = if restore {
                change
                    .before
                    .as_ref()
                    .ok_or_else(|| unsupported("foreign_connection"))?
            } else {
                &change.after
            };
            let mut args = vec![
                "connection".into(),
                "modify".into(),
                "uuid".into(),
                change.uuid.clone(),
            ];
            for (k, v) in values {
                args.push(k.clone());
                args.push(v.clone());
            }
            self.run_owned(Tool::Nmcli, &args, 20, None)?;
            if restore {
                // `modify` made NetworkManager (and Netplan) rewrite or move the
                // profile. Its files go back exactly, then it is read again.
                if self.restore_nm_files(&change.files)? {
                    self.run(Tool::Nmcli, &["connection", "reload"], 20)?;
                }
            } else {
                self.verify_written(change)?;
            }
            if (!restore && !change.remove_after_commit) || (restore && change.active) {
                self.run(Tool::Nmcli, &["connection", "up", "uuid", &change.uuid], 40)?;
            } else {
                let active = self.run(
                    Tool::Nmcli,
                    &["-g", "UUID", "connection", "show", "--active"],
                    20,
                )?;
                if active.lines().any(|s| s == change.uuid) {
                    self.run(
                        Tool::Nmcli,
                        &["connection", "down", "uuid", &change.uuid],
                        20,
                    )?;
                }
            }
            if restore {
                self.verify_restored(change)?;
                self.verify_files(change)?;
            }
        }
        Ok(())
    }
    pub(super) fn check_nm_snapshots(&self, changes: &[NmChange]) -> Result<()> {
        let present = self.nm_uuids()?;
        for change in changes {
            if let Some(before) = &change.before {
                let now = self.nm_snapshot(&change.uuid)?;
                if now != *before {
                    let field = FIELDS
                        .iter()
                        .find(|f| before.get(**f) != now.get(**f))
                        .copied()
                        .unwrap_or("");
                    let was = before.get(field).map_or("", String::as_str);
                    let is = now.get(field).map_or("", String::as_str);
                    return Err(failed(
                        "preflight",
                        &format!(
                            "configuration_changed: connection {} property {field} was '{was}', now '{is}'",
                            change.uuid
                        ),
                    ));
                }
            } else if present.contains(&change.uuid) {
                return Err(unsupported("foreign_connection"));
            }
        }
        Ok(())
    }
    pub(crate) fn prune_nm(&self, plan: &Plan) -> Result<()> {
        if plan.nm.is_empty() {
            return Ok(());
        }
        let present = self.nm_uuids()?;
        for change in plan.nm.iter().filter(|c| c.remove_after_commit) {
            if present.contains(&change.uuid) {
                self.run(
                    Tool::Nmcli,
                    &["connection", "delete", "uuid", &change.uuid],
                    20,
                )?;
            }
        }
        Ok(())
    }
    pub(super) fn verify_nm(&self, p: &NetworkProfiles, meta: &Metadata) -> Result<()> {
        for name in names(p) {
            let expected = if name == p.interface {
                meta.nm_parent_uuid.as_ref()
            } else {
                meta.nm_owned.get(&name)
            }
            .ok_or_else(|| unsupported("missing_parent_connection"))?;
            // The device names the connection it runs only once NetworkManager
            // has finished activating it (NI-17 D15); a read straight after
            // `connection up` may still show the one it is leaving.
            let mut shown = String::new();
            let wrong = self.nm_settled(|| {
                shown = self.run(
                    Tool::Nmcli,
                    &["-g", "GENERAL.CON-UUID", "device", "show", &name],
                    20,
                )?;
                Ok((shown.trim() != expected).then(|| shown.trim().to_owned()))
            })?;
            let actual = shown;
            if wrong.is_some() {
                // Name both connections, so an operator sees which profile won.
                let label = |uuid: &str| {
                    let id = self.nm_field(uuid, "connection.id").unwrap_or_default();
                    if uuid.is_empty() {
                        "no connection".to_owned()
                    } else {
                        format!("{id} ({uuid})")
                    }
                };
                return Err(failed(
                    "verify_backend",
                    &format!(
                        "foreign_configuration_selected: device {name} runs {} instead of {}",
                        label(actual.trim()),
                        label(expected)
                    ),
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn handoff_verify_nm(&self, p: &NetworkProfiles, meta: &Metadata) -> Result<()> {
        self.verify_nm(p, meta)
    }
}
/// `bind`: write the interface binding. Not for a Netplan-owned parent.
fn settings(
    n: Option<&NetworkProfile>,
    p: &NetworkProfiles,
    meta: &Metadata,
    name: &str,
    bind: bool,
) -> BTreeMap<String, String> {
    let management = n.is_some_and(|n| n.id == p.roles.management);
    let static_row = n.filter(|n| n.mode == AddressMode::Static);
    let mut values = BTreeMap::from([
        ("connection.interface-name".into(), name.into()),
        ("connection.autoconnect".into(), "yes".into()),
        ("connection.wait-device-timeout".into(), "0".into()),
        (
            "ipv4.method".into(),
            n.map_or("disabled", |n| {
                if n.mode == AddressMode::Dhcp {
                    "auto"
                } else {
                    "manual"
                }
            })
            .into(),
        ),
        (
            "ipv4.addresses".into(),
            static_row.map_or(String::new(), |n| format!("{}/{}", n.address, n.prefix)),
        ),
        (
            "ipv4.gateway".into(),
            n.map_or(String::new(), |n| n.gateway.clone()),
        ),
        (
            "ipv4.dns".into(),
            n.map_or(String::new(), |n| n.dns.join(" ")),
        ),
        // NE-2.12a: management only, DHCP or static; empty clears the property.
        (
            "ipv4.dns-search".into(),
            n.filter(|_| management)
                .map_or(String::new(), |n| n.dns_search.join(" ")),
        ),
        (
            "ipv4.never-default".into(),
            if management { "no" } else { "yes" }.into(),
        ),
        (
            "ipv4.ignore-auto-dns".into(),
            if management { "no" } else { "yes" }.into(),
        ),
        (
            "ipv4.ignore-auto-routes".into(),
            if management { "no" } else { "yes" }.into(),
        ),
        (
            "ipv4.route-metric".into(),
            n.map_or(meta.dhcp_metric, |n| meta.metric(n)).to_string(),
        ),
        ("ipv4.may-fail".into(), "yes".into()),
        ("ipv4.dhcp-send-hostname".into(), "no".into()),
        ("ipv6.method".into(), "disabled".into()),
    ]);
    if !bind {
        values.remove("connection.interface-name");
    }
    values
}
/// Do two `nmcli` values of `field` mean the same? Lists compare by element
/// (commas or spaces) and keep their order; `yes`/`no` accept their aliases.
fn same_value(field: &str, want: &str, got: &str) -> bool {
    let boolean = |s: &str| match s.to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => "yes".to_owned(),
        "no" | "false" | "off" | "0" => "no".to_owned(),
        other => other.to_owned(),
    };
    if field.ends_with("never-default") || field.contains("ignore-auto") {
        return boolean(want) == boolean(got);
    }
    split(want) == split(got)
}
/// NetworkManager's own bookkeeping in a profile file (YAML passthrough or
/// keyfile): the activation timestamp and the default privacy value.
fn nm_bookkeeping(line: &str) -> bool {
    let line = line.trim().replace('\'', "\"");
    line.starts_with("connection.timestamp:")
        || line.starts_with("timestamp=")
        || line == "ipv6.ip6-privacy: \"-1\""
        || line == "ip6-privacy=-1"
}
/// Is this file the before-image, as far as NetworkManager's own saving goes?
fn same_profile(now: Option<&crate::store::Image>, before: Option<&crate::store::Image>) -> bool {
    match (now, before) {
        (None, None) => true,
        (Some(now), Some(before)) => {
            let text = |image: &crate::store::Image| {
                String::from_utf8_lossy(&image.bytes)
                    .lines()
                    .filter(|line| !nm_bookkeeping(line))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            now.bytes == before.bytes || text(now) == text(before)
        }
        _ => false,
    }
}
/// A whole-file image of these would keep a secret in the journal.
fn holds_credentials(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    [
        "[802-1x]",
        "[wifi-security]",
        "[802-11-wireless-security]",
        "[vpn]",
        "[wireguard]",
        "[pppoe]",
        "password",
        "psk",
        "private-key",
        "auth:",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}
fn empty(s: &str) -> bool {
    s.is_empty() || s == "--"
}
fn split(s: &str) -> Vec<String> {
    if empty(s) {
        Vec::new()
    } else {
        s.split([',', ' '])
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
}
fn valid_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
fn new_uuid() -> Result<String> {
    let s = random_id()?;
    Ok(format!(
        "{}-{}-4{}-8{}-{}",
        &s[..8],
        &s[8..12],
        &s[13..16],
        &s[17..20],
        &s[20..32]
    ))
}
