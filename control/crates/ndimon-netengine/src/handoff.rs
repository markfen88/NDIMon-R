//! NE-9.5–9.10. The exchange contains intent and hashes, never peer journals.
//! Receive-side effects live exclusively in the ordinary NE-6 pending journal.
use crate::backend::{self, Metadata, Plan};
use crate::engine::{Commit, Engine};
use crate::host::{Host, Tool};
use crate::migration::PrivateDirectory;
use crate::store::{FileChange, Image, Root, Store};
use crate::{Result, failed, invalid, locked, unsupported};
use ndimon_types::{
    AddressMode, ApiError, Command, ConfirmPolicy, Domain, Impact, ImpactScope, Interruption,
    NetworkBackend, NetworkHandoff, NetworkHandoffReceipt, NetworkProduct, NetworkProfile,
    NetworkProfiles, Service,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const QUIESCE: &str = "/var/lib/ndimon-network/quiesce.json";
pub const EXTERNAL: &str = "/var/lib/ndimon-network/external.json";

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|_| failed("handoff", "serialize"))
}
fn image(value: &impl Serialize) -> Result<Image> {
    Ok(Root::image(&encode(value)?, 0o600))
}
fn path(source: &NetworkHandoff, suffix: &str) -> Result<String> {
    if !valid_id(&source.id) {
        return Err(invalid("id", "invalid_handoff_id"));
    }
    Ok(format!(
        "{}/handoff/{}.{}",
        source.product.state_directory(),
        source.id,
        suffix
    ))
}
fn marker(source: &NetworkHandoff, name: &str) -> String {
    format!("{}/{name}.json", source.product.state_directory())
}
fn private_read<T: serde::de::DeserializeOwned>(root: &Root, path: &str) -> Result<Option<T>> {
    let parent = std::path::Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .ok_or_else(|| unsupported("unsafe_configuration"))?;
    let name = path
        .rsplit('/')
        .next()
        .ok_or_else(|| unsupported("unsafe_configuration"))?;
    let Some(directory) = PrivateDirectory::open(root, parent)? else {
        return Ok(None);
    };
    directory
        .read(name)?
        .map(|i| {
            serde_json::from_slice(&i.bytes).map_err(|_| invalid("handoff", "invalid_document"))
        })
        .transpose()
}

// Nullable schema-1 fields are required even when their value is null.
fn required_nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
fn present_ndi<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Ipv4 {
    Dhcp {},
    Static {
        address: String,
        prefix: u8,
        #[serde(deserialize_with = "required_nullable")]
        gateway: Option<String>,
        dns: Vec<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub id: String,
    pub name: String,
    #[serde(deserialize_with = "required_nullable")]
    pub vlan_id: Option<u16>,
    pub ipv4: Ipv4,
    /// NE-2.12a: beside `ipv4`; omitted when empty so schema-1 files for older
    /// counterparts stay byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns_search: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policies {
    pub confirm: ConfirmPolicy,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub version: u32,
    pub revision: u64,
    pub interface: String,
    pub backend: NetworkBackend,
    pub networks: Vec<Network>,
    pub management: String,
    pub audio: String,
    #[serde(
        default,
        deserialize_with = "present_ndi",
        skip_serializing_if = "Option::is_none"
    )]
    pub ndi: Option<String>,
    pub policies: Policies,
}
impl Intent {
    pub fn from_profiles(p: &NetworkProfiles, revision: u64) -> Result<Self> {
        Ok(Self {
            version: 1,
            revision,
            interface: p.interface.clone(),
            backend: p
                .backend
                .ok_or_else(|| unsupported("unrepresentable_configuration"))?,
            networks: p
                .networks
                .iter()
                .map(|n| Network {
                    id: n.id.clone(),
                    name: n.name.clone(),
                    vlan_id: n.vlan_id,
                    ipv4: if n.mode == AddressMode::Dhcp {
                        Ipv4::Dhcp {}
                    } else {
                        Ipv4::Static {
                            address: n.address.clone(),
                            prefix: n.prefix,
                            gateway: if n.gateway.is_empty() {
                                None
                            } else {
                                Some(n.gateway.clone())
                            },
                            dns: n.dns.clone(),
                        }
                    },
                    dns_search: n.dns_search.clone(),
                })
                .collect(),
            management: p.roles.management.clone(),
            audio: p.roles.audio.clone(),
            ndi: Some(p.roles.ndi.clone()),
            policies: Policies {
                confirm: p.confirm_policy,
            },
        })
    }
    pub fn profiles(&self, product: NetworkProduct) -> Result<NetworkProfiles> {
        if self.version != 1
            || (product == NetworkProduct::Ndimon && self.ndi.is_none())
            || (product == NetworkProduct::Tessera && self.ndi.is_some())
        {
            return Err(unsupported("unrepresentable_configuration"));
        }
        let mut p = NetworkProfiles::default();
        p.interface = self.interface.clone();
        p.backend = Some(self.backend);
        p.confirm_policy = self.policies.confirm;
        p.roles.management = self.management.clone();
        p.roles.audio = self.audio.clone();
        // Tessera has no NDI role. Hosting adds NDI on its existing media path.
        p.roles.ndi = self.ndi.clone().unwrap_or_else(|| self.audio.clone());
        for n in &self.networks {
            let mut row = NetworkProfile::dhcp(&n.id, &n.name);
            row.vlan_id = n.vlan_id;
            row.dns_search = n.dns_search.clone();
            if let Ipv4::Static {
                address,
                prefix,
                gateway,
                dns,
            } = &n.ipv4
            {
                row.mode = AddressMode::Static;
                row.address = address.clone();
                row.prefix = *prefix;
                row.gateway = gateway.clone().unwrap_or_default();
                row.dns = dns.clone();
            }
            p.networks.push(row);
        }
        let normalized = crate::validation::validate(&p, &p)
            .map_err(|_| unsupported("unrepresentable_configuration"))?;
        if normalized != p {
            return Err(unsupported("unrepresentable_configuration"));
        }
        Ok(p)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: String,
    pub sha256: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adoption {
    pub interface: String,
    #[serde(deserialize_with = "required_nullable")]
    pub nm_parent_uuid: Option<String>,
    pub nm_owned: BTreeMap<String, String>,
    pub parent_definition: String,
    pub dhcp_metric: u32,
    pub static_metric: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub schema: u32,
    pub id: String,
    pub product: NetworkProduct,
    pub version: String,
    pub network: Intent,
    pub backend: NetworkBackend,
    pub adoption: Adoption,
    pub artifacts: Vec<Artifact>,
    pub nftables_table: String,
    pub bindings: BTreeMap<String, String>,
    pub created_at: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quiesce {
    pub id: String,
}
/// Provisional reservation: R remains owner until the receiver's authoritative
/// transaction decision exists. R cannot cancel or mutate during this phase.
/// The same identity is copied into .done only after that decision is durable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ownership {
    pub id: String,
    pub receiver: NetworkProduct,
    pub transaction: String,
    pub document_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transfer {
    pub source: NetworkHandoff,
    pub document: Document,
    pub document_sha256: String,
    pub source_table: Option<String>,
    pub source_quiesce: Image,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub source: NetworkHandoff,
    pub profiles: NetworkProfiles,
    pub write_files: Vec<String>,
    pub remove_files: Vec<String>,
    pub replace_table: String,
    pub install_table: String,
    pub interruptions: Vec<String>,
    pub impact: Impact,
    pub impact_context: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseStatus {
    pub externally_managed: bool,
    pub quiesced: bool,
    pub stale_done: Vec<String>,
}
fn table(product: NetworkProduct) -> &'static str {
    match product {
        NetworkProduct::Ndimon => "ndimon_management",
        NetworkProduct::Tessera => "tessera_network",
    }
}
fn bindings(p: &NetworkProfiles, product: NetworkProduct) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for (role, id) in [
        ("management", &p.roles.management),
        ("audio", &p.roles.audio),
    ] {
        bindings.insert(
            role.into(),
            crate::validation::ifname(p, crate::validation::assigned(p, id)?),
        );
    }
    if product == NetworkProduct::Ndimon {
        bindings.insert(
            "ndi".into(),
            crate::validation::ifname(p, crate::validation::assigned(p, &p.roles.ndi)?),
        );
    }
    Ok(bindings)
}
fn artifact_paths(p: &NetworkProfiles, product: NetworkProduct) -> Vec<String> {
    let prefix = if product == NetworkProduct::Ndimon {
        "ndimon"
    } else {
        "tessera"
    };
    let mut paths = vec![format!("/etc/modules-load.d/{prefix}-8021q.conf")];
    match p.backend {
        Some(NetworkBackend::Networkd | NetworkBackend::Netplan) => {
            for name in backend::names(p) {
                paths.push(format!("/etc/systemd/network/00-{prefix}-{name}.network"));
                if p.backend == Some(NetworkBackend::Networkd) && name != p.interface {
                    paths.push(format!("/etc/systemd/network/00-{prefix}-{name}.netdev"));
                }
            }
            if p.backend == Some(NetworkBackend::Netplan) {
                if product == NetworkProduct::Ndimon {
                    paths.extend([
                        "/etc/netplan/90-ndimon.yaml".into(),
                        "/etc/netplan/91-ndimon-vlans.yaml".into(),
                    ]);
                } else {
                    paths.push("/etc/netplan/90-tessera-network.yaml".into());
                }
            }
        }
        Some(NetworkBackend::Ifupdown) => paths.extend([
            format!("/etc/network/interfaces.d/{prefix}-{}", p.interface),
            format!("/etc/network/interfaces.d/{prefix}-vlans"),
            format!("/etc/dhcp/dhclient-enter-hooks.d/{prefix}-vlans"),
        ]),
        _ => (),
    }
    if product == NetworkProduct::Ndimon {
        paths.extend(
            [
                crate::BINDING,
                crate::TESSERA_BINDING,
                "/etc/systemd/system/ndimonndi.slice",
                "/etc/systemd/system/ndimontessera.slice",
                "/etc/systemd/system/ndimon-r.service.d/70-ndimon-network.conf",
                "/etc/systemd/system/ndimon-finder.service.d/70-ndimon-network.conf",
                "/etc/systemd/system/tesserad.service.d/70-ndimon-network.conf",
            ]
            .map(str::to_owned),
        );
    } else {
        paths.push("/etc/tessera/config.d/59-standalone-network.toml".into());
    }
    if p.backend == Some(NetworkBackend::Networkmanager) {
        for name in backend::names(p).into_iter().filter(|n| n != &p.interface) {
            paths.push(format!(
                "/etc/NetworkManager/system-connections/{prefix}-vlan-{name}.nmconnection"
            ));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
fn assert_idle(host: &Host) -> Result<()> {
    for name in ["pending", "upgrade", "handoff"] {
        if host.root.exists(&format!(
            "{}/{name}.json",
            NetworkProduct::Ndimon.state_directory()
        ))? {
            return Err(locked("pending_transaction"));
        }
    }
    Ok(())
}
fn peer(host: &Host, source: &NetworkHandoff) -> Result<PrivateDirectory> {
    PrivateDirectory::open(&host.root, source.product.state_directory())?
        .ok_or_else(|| unsupported("handoff_peer_missing"))
}
fn load(host: &Host, source: &NetworkHandoff) -> Result<Document> {
    private_read(&host.root, &path(source, "json")?)?.ok_or_else(|| ApiError::NotFound {
        what: "network_handoff".into(),
    })
}
pub(crate) fn validate(
    host: &Host,
    source: &NetworkHandoff,
    document: &Document,
) -> Result<NetworkProfiles> {
    let date = chrono::DateTime::parse_from_rfc3339(&document.created_at)
        .map_err(|_| invalid("created_at", "invalid_timestamp"))?;
    if !valid_id(&source.id)
        || document.schema != 1
        || document.id != source.id
        || document.product != source.product
        || document.version.is_empty()
        || document.version.len() > 128
        || document.backend != document.network.backend
        || document.adoption.interface != document.network.interface
        || document.nftables_table != table(source.product)
        || date.offset().local_minus_utc() != 0
        || !(document.created_at.ends_with('Z') || document.created_at.ends_with("+00:00"))
    {
        return Err(invalid("handoff", "invalid_document"));
    }
    let p = document.network.profiles(source.product)?;
    if document.bindings != bindings(&p, source.product)? {
        return Err(unsupported("unrepresentable_configuration"));
    }
    let allowed = artifact_paths(&p, source.product);
    let mut listed = BTreeSet::new();
    for artifact in &document.artifacts {
        if !allowed.contains(&artifact.path)
            || !listed.insert(artifact.path.clone())
            || !crate::store::valid_id(&artifact.sha256)
        {
            return Err(unsupported("unsafe_handoff_artifact"));
        }
    }
    // Refuse omissions, including the source's bindings and inactive pins.
    for owned in allowed {
        if host.root.exists(&owned)? && !listed.contains(&owned) {
            return Err(unsupported("incomplete_handoff_offer"));
        }
    }
    let q: Quiesce = private_read(&host.root, &marker(source, "quiesce"))?
        .ok_or_else(|| locked("handoff_not_prepared"))?;
    if q.id != source.id {
        return Err(locked("handoff"));
    }
    Ok(p)
}
fn snapshots(host: &Host, document: &Document) -> Result<Vec<FileChange>> {
    let mut changes = Vec::new();
    for artifact in &document.artifacts {
        let before = host.root.snapshot(&artifact.path)?;
        if before
            .as_ref()
            .is_none_or(|i| sha256(&i.bytes) != artifact.sha256)
        {
            return Err(ApiError::Drift {
                path: artifact.path.clone(),
            });
        }
        changes.push(FileChange {
            path: artifact.path.clone(),
            before,
            after: None,
        });
    }
    Ok(changes)
}
impl Engine {
    pub fn reconcile_handoff(&self) -> Result<()> {
        let status = release_status(self)?;
        if status.externally_managed && status.quiesced {
            let store = Store {
                root: self.host.root.clone(),
            };
            let _lock = store.lock()?;
            complete_release(self)?;
        }
        Ok(())
    }
    pub fn prepare_handoff(&self) -> Result<Document> {
        let store = Store {
            root: self.host.root.clone(),
        };
        let _lock = store.lock()?;
        let status = complete_release(self)?;
        if status.externally_managed {
            return Err(locked("external_owner"));
        }
        assert_idle(&self.host)?;
        if crate::migration::needed(&self.host)? {
            return Err(locked("pending_transaction"));
        }
        if let Some(q) = store.read::<Quiesce>("quiesce")? {
            return load(
                &self.host,
                &NetworkHandoff::new(NetworkProduct::Ndimon, q.id),
            );
        }
        self.host.ownership()?;
        let committed = self.authority.load()?;
        let p = &committed.profiles;
        crate::validation::validate(p, p)?;
        let id = crate::store::random_id()?[..32].to_owned();
        let source = NetworkHandoff::new(NetworkProduct::Ndimon, &id);
        let mut artifacts = Vec::new();
        for path in artifact_paths(p, NetworkProduct::Ndimon) {
            if let Some(i) = self.host.root.snapshot(&path)? {
                artifacts.push(Artifact {
                    path,
                    sha256: sha256(&i.bytes),
                });
            }
        }
        let d = Document {
            schema: 1,
            id: id.clone(),
            product: NetworkProduct::Ndimon,
            version: env!("CARGO_PKG_VERSION").into(),
            network: Intent::from_profiles(
                p,
                committed
                    .network_commit_revision
                    .unwrap_or(committed.revision),
            )?,
            backend: p
                .backend
                .ok_or_else(|| unsupported("unrepresentable_configuration"))?,
            adoption: Adoption {
                interface: p.interface.clone(),
                nm_parent_uuid: committed.metadata.nm_parent_uuid.clone(),
                nm_owned: committed.metadata.nm_owned.clone(),
                parent_definition: committed.metadata.parent_definition.clone(),
                dhcp_metric: committed.metadata.dhcp_metric,
                static_metric: committed.metadata.static_metric,
            },
            artifacts,
            nftables_table: table(NetworkProduct::Ndimon).into(),
            bindings: bindings(p, NetworkProduct::Ndimon)?,
            created_at: chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                .to_rfc3339(),
        };
        store.private_directory("/var/lib/ndimon-network/handoff")?;
        // Publish the document first. A crash before the marker leaves an inert
        // orphan; no receiver may import it without its matching marker.
        self.host
            .root
            .replace(&path(&source, "json")?, Some(&image(&d)?))?;
        store.write("quiesce", &Quiesce { id })?;
        Ok(d)
    }
    pub fn cancel_handoff(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            return Err(invalid("id", "invalid_handoff_id"));
        }
        let store = Store {
            root: self.host.root.clone(),
        };
        let _lock = store.lock()?;
        if store.read::<Ownership>("external")?.is_some() {
            return Err(locked("handoff_commit"));
        }
        let Some(q) = store.read::<Quiesce>("quiesce")? else {
            return Ok(());
        };
        if q.id != id {
            return Err(locked("handoff"));
        }
        // Removing the marker last makes interrupted cancellation retryable.
        self.host.root.replace(
            &path(&NetworkHandoff::new(NetworkProduct::Ndimon, id), "json")?,
            None,
        )?;
        store.remove("quiesce")
    }
    pub fn import_handoff(&self, source: &NetworkHandoff) -> Result<Review> {
        let _source_lock = source_lock(&self.host, source)?;
        Ok(self.handoff_plan(source)?.0)
    }
    pub(crate) fn handoff_plan(
        &self,
        source: &NetworkHandoff,
    ) -> Result<(Review, Commit, Plan, Transfer)> {
        // Same-prefix engines have one state root and cannot receive themselves.
        if source.product == NetworkProduct::Ndimon {
            return Err(unsupported("handoff_same_owner"));
        }
        assert_idle(&self.host)?;
        if crate::migration::needed(&self.host)? {
            return Err(locked("pending_transaction"));
        }
        let release = release_status(self)?;
        if release.quiesced || release.externally_managed {
            return Err(locked("handoff"));
        }
        if self.host.root.exists(&marker(source, "external"))? {
            return Err(locked("handoff_commit"));
        }
        let document = load(&self.host, source)?;
        let p = validate(&self.host, source, &document)?;
        if !p
            .backend
            .is_some_and(|b| self.config.certified_backends.contains(&b))
        {
            return Err(unsupported("backend_not_certified"));
        }
        let base = self.authority.load()?;
        if base.profiles.backend.is_some() || self.host.table("ndimon_management")?.is_some() {
            return Err(locked("ndimon"));
        }
        let source_files = snapshots(&self.host, &document)?;
        let mut host = self.host.clone();
        host.handoff = true;
        host.handoff_paths = source_files.iter().map(|f| f.path.clone()).collect();
        let metadata = host.handoff_metadata(&p, &document.adoption)?;
        let mut plan = host.plan(p.clone(), p.clone(), metadata.clone(), 80)?;
        if p.backend == Some(NetworkBackend::Networkmanager) {
            // Preserve VLAN UUIDs (and therefore DHCP client identity), while
            // moving their persistent keyfiles into A's explicit ownership.
            for n in p.networks.iter().filter(|n| n.vlan_id.is_some()) {
                let name = crate::validation::ifname(&p, n);
                let from = format!(
                    "/etc/NetworkManager/system-connections/tessera-vlan-{name}.nmconnection"
                );
                let to = format!(
                    "/etc/NetworkManager/system-connections/ndimon-vlan-{name}.nmconnection"
                );
                let before = source_files
                    .iter()
                    .find(|f| f.path == from)
                    .and_then(|f| f.before.as_ref())
                    .ok_or_else(|| unsupported("incomplete_handoff_offer"))?;
                if host.root.exists(&to)? {
                    return Err(locked("ndimon"));
                }
                let text = std::str::from_utf8(&before.bytes)
                    .map_err(|_| unsupported("unrepresentable_configuration"))?;
                let uuid = metadata
                    .nm_owned
                    .get(&name)
                    .ok_or_else(|| unsupported("missing_parent_connection"))?;
                let rendered = nm_keyfile(text, &name, uuid)?;
                plan.files.push(FileChange {
                    path: to,
                    before: None,
                    after: Some(Root::image(&rendered, 0o600)),
                });
            }
        }
        plan.previous_metadata = metadata;
        plan.affected_services = backend::MEDIA.iter().map(|s| (*s).into()).collect();
        // The complete before-set includes both products; paths may not overlap.
        for change in source_files {
            if plan.files.iter().any(|c| c.path == change.path) {
                return Err(unsupported("unsafe_handoff_artifact"));
            }
            plan.files.push(change);
        }
        let document_image = self
            .host
            .root
            .snapshot(&path(source, "json")?)?
            .ok_or_else(|| invalid("handoff", "invalid_document"))?;
        let transfer = Transfer {
            source: source.clone(),
            document,
            document_sha256: sha256(&document_image.bytes),
            source_table: host.table(table(source.product))?,
            source_quiesce: self
                .host
                .root
                .snapshot(&marker(source, "quiesce"))?
                .ok_or_else(|| locked("handoff_not_prepared"))?,
        };
        if transfer.source_table.is_none() {
            return Err(unsupported("handoff_guard_missing"));
        }
        plan.metadata.handoff = Some(NetworkHandoffReceipt::new(
            source.clone(),
            transfer.document_sha256.clone(),
        ));
        let mut impact = Impact::new(ImpactScope::Network);
        impact.restarts = vec![Service::Tessera, Service::Finder, Service::Decoder];
        impact.interruptions = backend::MEDIA
            .iter()
            .map(|s| Interruption::new(Domain::Network, *s, Some(crate::APPLY_MS)))
            .collect();
        impact.lines = vec!["Transfer network ownership to NDIMon-R without changing addresses, VLANs or existing roles.".into(),
            "Restart network consumers; Keep within 60 seconds after readiness or restore the releasing engine.".into()];
        let impact_context = vec![sha256(
            encode(&(&base, &plan.files, &plan.nm, &transfer.document_sha256))?.as_bytes(),
        )];
        impact = impact
            .sealed(
                &Command::CommitNetworkHandoff {
                    source: source.clone(),
                },
                &impact_context,
            )
            .map_err(|_| failed("handoff", "serialize"))?;
        let review = Review {
            source: source.clone(),
            profiles: p,
            write_files: plan
                .files
                .iter()
                .filter(|c| c.after.is_some())
                .map(|c| c.path.clone())
                .collect(),
            remove_files: transfer
                .document
                .artifacts
                .iter()
                .map(|a| a.path.clone())
                .collect(),
            replace_table: transfer.document.nftables_table.clone(),
            install_table: "ndimon_management".into(),
            interruptions: plan.affected_services.clone(),
            impact,
            impact_context,
        };
        Ok((review, base, plan, transfer))
    }
}
impl Host {
    fn handoff_metadata(&self, p: &NetworkProfiles, adoption: &Adoption) -> Result<Metadata> {
        let mut m = Metadata::default();
        m.dhcp_metric = adoption.dhcp_metric;
        m.static_metric = adoption.static_metric;
        m.nm_parent_uuid = adoption.nm_parent_uuid.clone();
        m.nm_owned = adoption.nm_owned.clone();
        if p.backend == Some(NetworkBackend::Networkmanager) {
            if m.nm_parent_uuid.as_ref().is_none_or(|s| !uuid(s))
                || m.nm_owned.iter().any(|(name, id)| {
                    !backend::names(p).contains(name) || name == &p.interface || !uuid(id)
                })
                || p.networks
                    .iter()
                    .filter(|n| n.vlan_id.is_some())
                    .any(|n| !m.nm_owned.contains_key(&crate::validation::ifname(p, n)))
            {
                return Err(unsupported("unrepresentable_configuration"));
            }
            self.handoff_verify_nm(p, &m)
                .map_err(|_| unsupported("unrepresentable_configuration"))?;
        } else if m.nm_parent_uuid.is_some() || !m.nm_owned.is_empty() {
            return Err(unsupported("unrepresentable_configuration"));
        }
        if p.backend == Some(NetworkBackend::Netplan) {
            self.handoff_netplan_metadata(p, adoption, &mut m)?;
        }
        if p.backend == Some(NetworkBackend::Ifupdown) {
            m.parent_source = format!("/etc/network/interfaces.d/ndimon-{}", p.interface);
        }
        Ok(m)
    }
}
fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
/// Called under A's mutation lock. Locks are always A, then R; R never acquires A.
pub(crate) fn source_lock(
    host: &Host,
    source: &NetworkHandoff,
) -> Result<nix::fcntl::Flock<std::fs::File>> {
    if source.product == NetworkProduct::Ndimon {
        return Err(unsupported("handoff_same_owner"));
    }
    if !valid_id(&source.id) {
        return Err(invalid("id", "invalid_handoff_id"));
    }
    peer(host, source)?.lock().map_err(|error| match error {
        ApiError::Locked { .. } => locked("handoff"),
        error => error,
    })
}
pub(crate) fn ownership(t: &Transfer, job: &str) -> Ownership {
    Ownership {
        id: t.source.id.clone(),
        receiver: NetworkProduct::Ndimon,
        transaction: job.into(),
        document_sha256: t.document_sha256.clone(),
    }
}
pub(crate) fn reserve(host: &Host, t: &Transfer, job: &str) -> Result<()> {
    host.root.replace(
        &marker(&t.source, "external"),
        Some(&image(&ownership(t, job))?),
    )
}
pub(crate) fn check(host: &Host, t: &Transfer, job: &str) -> Result<()> {
    let document = host.root.snapshot(&path(&t.source, "json")?)?;
    if document.is_none_or(|i| sha256(&i.bytes) != t.document_sha256) {
        return Err(ApiError::Drift {
            path: path(&t.source, "json")?,
        });
    }
    snapshots(host, &t.document)?;
    if host
        .table(&t.document.nftables_table)?
        .as_deref()
        .map(table_configuration)
        != t.source_table.as_deref().map(table_configuration)
    {
        return Err(ApiError::Drift {
            path: t.document.nftables_table.clone(),
        });
    }
    if host.root.snapshot(&marker(&t.source, "quiesce"))? != Some(t.source_quiesce.clone()) {
        return Err(locked("handoff"));
    }
    let external: Option<Ownership> = private_read(&host.root, &marker(&t.source, "external"))?;
    if external != Some(ownership(t, job)) {
        return Err(locked("handoff_commit"));
    }
    Ok(())
}
pub(crate) fn replace_tables(
    host: &Host,
    source: &str,
    target: Option<&str>,
    previous: Option<&str>,
) -> Result<()> {
    let mut rules = String::new();
    for name in ["ndimon_management", source] {
        if host.table(name)?.is_some() {
            rules.push_str(&format!("delete table inet {name}\n"));
        }
    }
    for text in [target, previous].into_iter().flatten() {
        rules.push_str(text);
        rules.push('\n');
    }
    if !rules.is_empty() {
        host.run_owned(Tool::Nft, &["-f".into(), "-".into()], 20, Some(&rules))?;
    }
    Ok(())
}
pub(crate) fn restored(host: &Host, t: &Transfer) -> Result<()> {
    // Idempotent recovery: only release quiescence after files/backend/table have
    // been restored and verified. A retained failed journal keeps both blocked.
    let external: Option<Ownership> = private_read(&host.root, &marker(&t.source, "external"))?;
    if external.as_ref().is_some_and(|e| e.id == t.source.id) {
        host.root.replace(&marker(&t.source, "external"), None)?;
    }
    let q: Option<Quiesce> = private_read(&host.root, &marker(&t.source, "quiesce"))?;
    if q.as_ref().is_some_and(|q| q.id == t.source.id) {
        host.root.replace(&marker(&t.source, "quiesce"), None)?;
    }
    Ok(())
}
pub(crate) fn kept(host: &Host, t: &Transfer, job: &str) -> Result<()> {
    let expected = ownership(t, job);
    let external: Option<Ownership> = private_read(&host.root, &marker(&t.source, "external"))?;
    if external != Some(expected.clone()) {
        return Err(locked("handoff_commit"));
    }
    host.root
        .replace(&path(&t.source, "done")?, Some(&image(&expected)?))
}
pub(crate) fn source_readiness(host: &Host, t: &Transfer, plan: &Plan) -> Result<()> {
    host.readiness(&plan.previous, &plan.previous_metadata, true, false)?;
    match plan.previous.backend {
        Some(NetworkBackend::Networkd | NetworkBackend::Netplan) => {
            let prefix = if t.source.product == NetworkProduct::Tessera {
                "tessera"
            } else {
                "ndimon"
            };
            for name in backend::names(&plan.previous) {
                let text = host.run(Tool::Networkctl, &["status", "--no-pager", "--", &name], 8)?;
                if backend::networkd::selected(&text)
                    != Some(format!("/etc/systemd/network/00-{prefix}-{name}.network").as_str())
                {
                    return Err(failed("handoff_recovery", "foreign_configuration_selected"));
                }
            }
        }
        Some(NetworkBackend::Networkmanager) => {
            host.handoff_verify_nm(&plan.previous, &plan.previous_metadata)?
        }
        Some(NetworkBackend::Ifupdown) => {
            for name in backend::names(&plan.previous) {
                host.run(Tool::Ifquery, &["--state", &name], 5)?;
            }
        }
        _ => return Err(unsupported("unknown_backend")),
    }
    Ok(())
}
pub(crate) fn release_status(engine: &Engine) -> Result<ReleaseStatus> {
    let store = Store {
        root: engine.host.root.clone(),
    };
    let q = store.read::<Quiesce>("quiesce")?;
    let external = store.read::<Ownership>("external")?;
    let mut status = ReleaseStatus {
        externally_managed: false,
        quiesced: q.is_some(),
        stale_done: Vec::new(),
    };
    for file in engine
        .host
        .root
        .list("/var/lib/ndimon-network/handoff", Some(".done"))?
    {
        let id = file
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_end_matches(".done");
        if !valid_id(id) {
            status.stale_done.push(file);
            continue;
        }
        let done: Option<Ownership> = match private_read(&engine.host.root, &file) {
            Ok(done) => done,
            Err(ApiError::Invalid { .. }) => {
                status.stale_done.push(file);
                continue;
            }
            Err(error) => return Err(error),
        };
        let matches = done.as_ref().zip(external.as_ref()).is_some_and(|(d, e)| {
            d == e
                && d.id == id
                && d.receiver == NetworkProduct::Tessera
                && crate::store::valid_id(&d.transaction)
                && crate::store::valid_id(&d.document_sha256)
        }) && q.as_ref().is_none_or(|q| q.id == id);
        let document_matches = if matches {
            engine
                .host
                .root
                .snapshot(&path(
                    &NetworkHandoff::new(NetworkProduct::Ndimon, id),
                    "json",
                )?)?
                .is_some_and(|i| {
                    done.as_ref()
                        .is_some_and(|d| sha256(&i.bytes) == d.document_sha256)
                })
        } else {
            matches
        };
        if matches && document_matches {
            status.externally_managed = true;
        } else {
            status.stale_done.push(file);
        }
    }
    Ok(status)
}
pub(crate) fn complete_release(engine: &Engine) -> Result<ReleaseStatus> {
    let mut status = release_status(engine)?;
    if !status.externally_managed || !status.quiesced {
        return Ok(status);
    }
    let store = Store {
        root: engine.host.root.clone(),
    };
    let q = store
        .read::<Quiesce>("quiesce")?
        .ok_or_else(|| locked("handoff"))?;
    let directory = format!("/var/lib/ndimon-network/archive/{}", q.id);
    store.private_directory("/var/lib/ndimon-network/archive")?;
    store.private_directory(&directory)?;
    // Preserve the authoritative network decision without moving state.json:
    // that envelope also owns every other native domain. External ownership
    // fences its archived network section from every future network mutation.
    let decision = format!("{directory}/decision.json");
    if !engine.host.root.exists(&decision)? {
        engine
            .host
            .root
            .replace(&decision, Some(&image(&engine.authority.load()?)?))?;
    }
    for name in ["pending", "last", "committed", "upgrade", "handoff"] {
        let from = format!("/var/lib/ndimon-network/{name}.json");
        if let Some(before) = engine.host.root.snapshot(&from)? {
            engine
                .host
                .root
                .replace(&format!("{directory}/{name}.json"), Some(&before))?;
            engine.host.root.replace(&from, None)?;
        }
    }
    store.remove("quiesce")?;
    status.quiesced = false;
    Ok(status)
}
pub(crate) fn writable(engine: &Engine) -> Result<()> {
    let status = release_status(engine)?;
    if status.quiesced {
        return Err(locked("handoff"));
    }
    if status.externally_managed {
        return Err(locked("external_owner"));
    }
    Ok(())
}

pub(crate) fn received(host: &Host) -> Result<bool> {
    let external: Option<Ownership> =
        private_read(&host.root, "/var/lib/tessera-network/external.json")?;
    let Some(external) = external else {
        return Ok(false);
    };
    if external.receiver != NetworkProduct::Ndimon
        || !valid_id(&external.id)
        || !crate::store::valid_id(&external.transaction)
        || !crate::store::valid_id(&external.document_sha256)
    {
        return Ok(false);
    }
    let source = NetworkHandoff::new(NetworkProduct::Tessera, &external.id);
    let done: Option<Ownership> = private_read(&host.root, &path(&source, "done")?)?;
    let document = host.root.snapshot(&path(&source, "json")?)?;
    Ok(done == Some(external.clone())
        && document.is_some_and(|i| sha256(&i.bytes) == external.document_sha256))
}

fn table_configuration(text: &str) -> Vec<String> {
    let mut previous = "";
    text.split_whitespace()
        .filter_map(|word| {
            let counter =
                matches!(previous, "packets" | "bytes") && word.bytes().all(|b| b.is_ascii_digit());
            previous = word;
            if counter { None } else { Some(word.to_owned()) }
        })
        .collect()
}

fn nm_keyfile(text: &str, name: &str, uuid: &str) -> Result<String> {
    let mut section = "";
    let mut identity = false;
    let mut matched_uuid = false;
    let mut rendered = String::new();
    for line in text.lines() {
        let clean = line.trim();
        if clean.starts_with('[') {
            section = clean;
            if ![
                "[connection]",
                "[ethernet]",
                "[vlan]",
                "[ipv4]",
                "[ipv6]",
                "[proxy]",
            ]
            .contains(&section)
            {
                return Err(unsupported("unrepresentable_configuration"));
            }
        }
        if section == "[connection]" && clean.starts_with("id=") {
            if identity {
                return Err(unsupported("unrepresentable_configuration"));
            }
            identity = true;
            rendered.push_str(&format!("id=ndimon-vlan-{name}\n"));
        } else {
            if section == "[connection]" && clean.starts_with("uuid=") {
                matched_uuid = clean == format!("uuid={uuid}");
            }
            rendered.push_str(line);
            rendered.push('\n');
        }
    }
    if !identity || !matched_uuid {
        return Err(unsupported("unrepresentable_configuration"));
    }
    Ok(rendered)
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn ne_9_10_exact_artifact_inventory_for_both_products_and_every_backend() {
        let d: Document = serde_json::from_str(include_str!(
            "../tests/fixtures/tessera-ea70828/tessera-handoff.json"
        ))
        .unwrap();
        for product in [NetworkProduct::Ndimon, NetworkProduct::Tessera] {
            let prefix = if product == NetworkProduct::Ndimon {
                "ndimon"
            } else {
                "tessera"
            };
            for backend in [
                NetworkBackend::Networkd,
                NetworkBackend::Netplan,
                NetworkBackend::Networkmanager,
                NetworkBackend::Ifupdown,
            ] {
                let mut p = d.network.profiles(NetworkProduct::Tessera).unwrap();
                p.backend = Some(backend);
                let mut expected = vec![format!("/etc/modules-load.d/{prefix}-8021q.conf")];
                match backend {
                    NetworkBackend::Networkd | NetworkBackend::Netplan => {
                        expected.extend([
                            format!("/etc/systemd/network/00-{prefix}-end0.network"),
                            format!("/etc/systemd/network/00-{prefix}-end0.20.network"),
                        ]);
                        if backend == NetworkBackend::Networkd {
                            expected
                                .push(format!("/etc/systemd/network/00-{prefix}-end0.20.netdev"));
                        } else if product == NetworkProduct::Ndimon {
                            expected.extend([
                                "/etc/netplan/90-ndimon.yaml".into(),
                                "/etc/netplan/91-ndimon-vlans.yaml".into(),
                            ]);
                        } else {
                            expected.push("/etc/netplan/90-tessera-network.yaml".into());
                        }
                    }
                    NetworkBackend::Networkmanager => expected.push(format!(
                        "/etc/NetworkManager/system-connections/{prefix}-vlan-end0.20.nmconnection"
                    )),
                    NetworkBackend::Ifupdown => expected.extend([
                        format!("/etc/network/interfaces.d/{prefix}-end0"),
                        format!("/etc/network/interfaces.d/{prefix}-vlans"),
                        format!("/etc/dhcp/dhclient-enter-hooks.d/{prefix}-vlans"),
                    ]),
                    _ => unreachable!(),
                }
                if product == NetworkProduct::Tessera {
                    expected.push("/etc/tessera/config.d/59-standalone-network.toml".into());
                } else {
                    expected.extend(
                        [
                            "/etc/ndimon-network-interfaces.json",
                            "/etc/tessera/config.d/62-ndimon-network.toml",
                            "/etc/systemd/system/ndimonndi.slice",
                            "/etc/systemd/system/ndimontessera.slice",
                            "/etc/systemd/system/ndimon-r.service.d/70-ndimon-network.conf",
                            "/etc/systemd/system/ndimon-finder.service.d/70-ndimon-network.conf",
                            "/etc/systemd/system/tesserad.service.d/70-ndimon-network.conf",
                        ]
                        .map(str::to_owned),
                    );
                }
                expected.sort();
                assert_eq!(
                    artifact_paths(&p, product),
                    expected,
                    "{product:?}/{backend:?}"
                );
            }
        }
    }
}
