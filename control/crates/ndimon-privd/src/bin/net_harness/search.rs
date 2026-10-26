//! NE-2.12a on the real backends. xtask writes a management configuration that
//! carries search domains (the stock cloud-init shape on Netplan); these cases
//! adopt it, apply and keep a changed list, adopt the result back from the
//! operating system, and revert. The lab's DHCP server also offers the domain
//! `unwanted.test`, which must never be written into the configuration.
use super::*;

/// What xtask's fixture configures, in its order.
pub const FIXTURE: [&str; 2] = ["lab.example", "corp.example"];
const CHANGED: [&str; 2] = ["lab.example", "ops.example"];
const LEASE: &str = "unwanted.test";

fn list(domains: &[&str]) -> Vec<String> {
    domains.iter().map(|d| (*d).to_owned()).collect()
}
/// A fresh host view over the container's real tools, for first-adoption reads.
fn host() -> ndimon_netengine::Host {
    let mut host = ndimon_netengine::Host::default();
    host.runner = Arc::new(ContainerProcesses);
    host
}
fn same_row(a: &NetworkProfile, b: &NetworkProfile) -> bool {
    a.mode == b.mode
        && a.address == b.address
        && a.prefix == b.prefix
        && a.gateway == b.gateway
        && a.dns == b.dns
        && a.dns_search == b.dns_search
}
fn dhcp(p: &mut NetworkProfiles) {
    let n = &mut p.networks[0];
    n.mode = AddressMode::Dhcp;
    n.address.clear();
    n.prefix = 0;
    n.gateway.clear();
    n.dns.clear();
}
fn fixture_row(p: &NetworkProfiles) -> Result<()> {
    let n = p.networks.first().ok_or_else(|| error("no_network"))?;
    ensure(
        n.mode == AddressMode::Static
            && n.address == "192.0.2.10"
            && n.prefix == 24
            && n.gateway == "192.0.2.1"
            && n.dns == ["192.0.2.1"],
        "fixture_not_adopted",
    )?;
    ensure(n.dns_search == FIXTURE, "search_not_adopted")
}
/// Stage, wait for the applied phase, and Keep; the staged files are returned.
fn kept(e: &Engine, p: &NetworkProfiles) -> Result<Vec<FileChange>> {
    let revision = e.authority.load()?.revision;
    let (token, _, files) = stage(e, p)?;
    wait(e, true)?;
    serialized(|| e.confirm(token.expose(), None))?;
    ensure(
        e.authority.load()?.revision == revision + 1,
        "revision_increment",
    )?;
    Ok(files)
}
/// The operating system carries exactly these domains, as its own tools report.
fn configured(backend: NetworkBackend, present: &[&str], absent: &[&str]) -> Result<()> {
    let text = match backend {
        NetworkBackend::Networkd | NetworkBackend::Netplan => {
            run(Tool::Networkctl, &["status", "--no-pager", "--", "end0"])?
        }
        NetworkBackend::Networkmanager => {
            let uuid = run(
                Tool::Nmcli,
                &["-g", "GENERAL.CON-UUID", "device", "show", "end0"],
            )?;
            run(
                Tool::Nmcli,
                &[
                    "-g",
                    "ipv4.dns-search",
                    "connection",
                    "show",
                    "uuid",
                    uuid.trim(),
                ],
            )?
        }
        NetworkBackend::Ifupdown => Root::production()
            .text("/etc/network/interfaces.d/ndimon-end0")?
            .ok_or_else(|| error("ifupdown_file_missing"))?,
        _ => return Err(error("backend")),
    };
    for domain in present {
        ensure(
            text.contains(domain),
            &format!("domain_missing:{domain}:{}", redact(&text)),
        )?;
    }
    for domain in absent.iter().chain(&[LEASE]) {
        ensure(!text.contains(domain), &format!("domain_present:{domain}"))?;
    }
    Ok(())
}
/// Adopt again from the files and tools the engine just configured.
fn adopted(backend: NetworkBackend, expected: &NetworkProfile) -> Result<()> {
    let (profiles, _) = host().adopt(Some("end0"))?;
    ensure(
        profiles.backend.is_some() && same_row(&profiles.networks[0], expected),
        &format!(
            "round_trip_adoption:{backend:?}:{:?} != {:?}",
            profiles.networks[0], expected
        ),
    )
}

pub fn scenario(e: &Engine, backend: NetworkBackend, name: &str) -> Result<()> {
    match name {
        // Adopt the fixture, stage a change on top of it, then revert: the
        // original files and domains must be back, byte for byte.
        "search-first-revert" => {
            let base = e.authority.load()?;
            ensure(base.profiles.backend.is_none(), "requires_fresh_adoption")?;
            let status = e.status()?;
            ensure(status.refusal.is_none(), "adoption_refused")?;
            fixture_row(&status.profiles)?;
            configured(backend, &FIXTURE, &[])?;
            let mut p = status.profiles.clone();
            p.networks[0].dns_search = list(&CHANGED);
            let (token, _, files) = stage(e, &p)?;
            wait(e, true)?;
            configured(backend, &CHANGED, &["corp.example"])?;
            serialized(|| e.revert(token.expose()))?;
            ensure(e.authority.load()? == base, "adoption_revert_committed")?;
            restored(&files)?;
            configured(backend, &FIXTURE, &["ops.example"])?;
            ensure(e.status()?.profiles == status.profiles, "readopted_differs")?;
        }
        // Adopt, apply, Keep; adopt back from the OS; then DHCP management with
        // the same list (a lease's own domain is never taken); then none.
        "search-adopt-keep" => {
            let base = e.authority.load()?;
            ensure(base.profiles.backend.is_none(), "requires_fresh_adoption")?;
            let status = e.status()?;
            fixture_row(&status.profiles)?;
            let mut p = status.profiles.clone();
            p.networks[0].dns_search = list(&CHANGED);
            kept(e, &p)?;
            configured(backend, &CHANGED, &["corp.example"])?;
            ensure(
                e.authority.load()?.profiles.networks[0].dns_search == CHANGED,
                "committed_list",
            )?;
            adopted(backend, &p.networks[0])?;
            // DHCP keeps the list; the lease offers `unwanted.test` meanwhile.
            dhcp(&mut p);
            let files = kept(e, &p)?;
            for file in &files {
                if let Some(image) = &file.after {
                    ensure(
                        !String::from_utf8_lossy(&image.bytes).contains(LEASE),
                        &format!("lease_domain_written:{}", file.path),
                    )?;
                }
            }
            configured(backend, &CHANGED, &[])?;
            adopted(backend, &p.networks[0])?;
            // Back to no domains at all.
            p.networks[0].dns_search.clear();
            kept(e, &p)?;
            configured(
                backend,
                &[],
                &["lab.example", "ops.example", "corp.example"],
            )?;
            adopted(backend, &p.networks[0])?;
            ensure(
                e.authority.load()?.profiles.networks[0]
                    .dns_search
                    .is_empty(),
                "committed_empty",
            )?;
        }
        // A committed device: a changed list that is reverted leaves every file
        // and the OS as they were.
        "search-revert" => {
            let base = e.authority.load()?;
            ensure(base.profiles.backend.is_some(), "requires_committed_state")?;
            let mut p = base.profiles.clone();
            p.networks[0].dns_search = list(&CHANGED);
            let (token, _, files) = stage(e, &p)?;
            wait(e, true)?;
            configured(backend, &CHANGED, &[])?;
            serialized(|| e.revert(token.expose()))?;
            ensure(e.authority.load()? == base, "revert_changed_commit")?;
            restored(&files)?;
            configured(
                backend,
                &[],
                &["lab.example", "ops.example", "corp.example"],
            )?;
            adopted(backend, &base.profiles.networks[0])?;
        }
        _ => return Err(error("unknown_scenario")),
    }
    Ok(())
}
