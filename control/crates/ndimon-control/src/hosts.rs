//! Validate HTTP authority against local identity/configuration, never DNS answers.
use axum::http::{HeaderMap, header, uri::Authority};
use std::net::IpAddr;

pub fn allowed(headers: &HeaderMap) -> bool {
    // Network Stage/Keep and hostname changes take effect immediately. Reading
    // getifaddrs/gethostname per request avoids an obsolete management allowlist.
    let hostname = nix::unistd::gethostname().ok();
    let addresses = nix::ifaddrs::getifaddrs()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|interface| {
            let address = interface.address?;
            address
                .as_sockaddr_in()
                .map(|a| IpAddr::V4(a.ip()))
                .or_else(|| address.as_sockaddr_in6().map(|a| IpAddr::V6(a.ip())))
        });
    let hostname = hostname.as_ref().and_then(|h| h.to_str());
    matches_device(headers, hostname, addresses)
        || matches_search_domains(
            headers,
            hostname,
            &search_domains(std::path::Path::new("/")),
        )
}

fn normalized(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        ip => ip,
    }
}

fn request_host(headers: &HeaderMap) -> Option<&str> {
    let mut hosts = headers.get_all(header::HOST).iter();
    let value = hosts.next().and_then(|h| h.to_str().ok())?;
    if hosts.next().is_some() || value.contains('@') {
        return None;
    }
    let authority = value.parse::<Authority>().ok()?;
    let host = authority.host();
    let suffix = &value[host.len()..];
    if !suffix.is_empty()
        && !suffix.strip_prefix(':').is_some_and(|port| {
            !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
                && port.parse::<u16>().is_ok()
        })
    {
        return None;
    }
    Some(&value[..host.len()])
}

fn matches_device(
    headers: &HeaderMap,
    hostname: Option<&str>,
    addresses: impl IntoIterator<Item = IpAddr>,
) -> bool {
    let Some(host) = request_host(headers) else {
        return false;
    };
    let literal = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = literal.parse::<IpAddr>() {
        // IPv6 literals must use HTTP's bracketed spelling.
        if ip.is_ipv6() && !host.starts_with('[') {
            return false;
        }
        let ip = normalized(ip);
        return !ip.is_unspecified()
            && (ip.is_loopback() || addresses.into_iter().any(|a| normalized(a) == ip));
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || hostname.is_some_and(|name| {
            let name = name.trim_end_matches('.');
            !name.is_empty()
                && (host.eq_ignore_ascii_case(name)
                    || host.eq_ignore_ascii_case(&format!("{name}.local")))
        })
}

/// Additional names are exact matches to accepted device intent.
pub fn configured_allowed(headers: &HeaderMap, names: &[String]) -> bool {
    request_host(headers).is_some_and(|host| {
        let host = host.strip_suffix('.').unwrap_or(host);
        names
            .iter()
            .any(|name| ndimon_types::valid_web_hostname(name) && host.eq_ignore_ascii_case(name))
    })
}

fn matches_search_domains(headers: &HeaderMap, hostname: Option<&str>, domains: &[String]) -> bool {
    let Some(hostname) = hostname.filter(|name| !name.is_empty()) else {
        return false;
    };
    configured_allowed(
        headers,
        &domains
            .iter()
            .map(|domain| format!("{}.{}", hostname.trim_end_matches('.'), domain))
            .collect::<Vec<_>>(),
    )
}

/// resolv.conf uses the last search/domain directive. resolved's generated
/// upstream file contains the effective global AND link search domains, omitting
/// route-only domains. Read it only when the device actually uses resolved.
fn resolver_domains(raw: &str) -> Vec<String> {
    let mut domains = Vec::new();
    for line in raw.lines() {
        let mut words = line
            .split(['#', ';'])
            .next()
            .unwrap_or("")
            .split_whitespace();
        let Some(directive) = words.next() else {
            continue;
        };
        if matches!(directive, "search" | "domain") {
            domains = words
                .take(if directive == "domain" { 1 } else { usize::MAX })
                .map(|name| name.trim_end_matches('.').to_ascii_lowercase())
                .filter(|name| ndimon_types::valid_web_hostname(name))
                .collect();
        }
    }
    domains
}

fn search_domains(root: &std::path::Path) -> Vec<String> {
    let path = root.join("etc/resolv.conf");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let resolved_dir = root.join("run/systemd/resolve");
    let uses_resolved = std::fs::canonicalize(&path)
        .is_ok_and(|path| path.starts_with(&resolved_dir))
        || raw.lines().any(|line| {
            let mut words = line
                .split(['#', ';'])
                .next()
                .unwrap_or("")
                .split_whitespace();
            words.next() == Some("nameserver")
                && matches!(words.next(), Some("127.0.0.53" | "127.0.0.54"))
        });
    if uses_resolved && let Ok(upstream) = std::fs::read_to_string(resolved_dir.join("resolv.conf"))
    {
        return resolver_domains(&upstream);
    }
    resolver_domains(&raw)
}

/// Host is untrusted. Never reflect control characters or an unbounded value.
pub fn display_host(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .map(|value| {
            String::from_utf8_lossy(value.as_bytes())
                .chars()
                .filter(|ch| !ch.is_control())
                .take(253)
                .collect()
        })
        .unwrap_or_else(|| "(missing Host)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn headers(host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse().unwrap());
        headers
    }

    #[test]
    fn search_domains_and_aliases_are_exact_names_with_case_and_port_variants() {
        let domains = resolver_domains("search lan home.arpa corp.example.com\n");
        for host in [
            "monitor.lan",
            "MONITOR.HOME.ARPA:8080",
            "monitor.corp.example.com.:80",
        ] {
            assert!(
                matches_search_domains(&headers(host), Some("monitor"), &domains),
                "{host}"
            );
        }
        for host in [
            "monitor.evil.example",
            "monitor.lan.evil",
            "monitor2.lan",
            "sub.monitor.lan",
            "monitor.lan:65536",
        ] {
            assert!(
                !matches_search_domains(&headers(host), Some("monitor"), &domains),
                "{host}"
            );
        }
        let aliases = vec!["studio.corp.example.com".into()];
        for host in [
            "studio.corp.example.com",
            "STUDIO.CORP.EXAMPLE.COM:8080",
            "studio.corp.example.com.:80",
        ] {
            assert!(configured_allowed(&headers(host), &aliases), "{host}");
        }
        for host in [
            "studio.corp.example.com.evil",
            "sub.studio.corp.example.com",
            "other.corp.example.com",
            "studio.corp.example.com:bad",
        ] {
            assert!(!configured_allowed(&headers(host), &aliases), "{host}");
        }
        assert!(!configured_allowed(
            &headers("studio.corp.example.com"),
            &[]
        ));
        assert!(!configured_allowed(
            &headers("192.0.2.1"),
            &["192.0.2.1".into()]
        ));
        assert!(!configured_allowed(
            &headers("evil.example"),
            &["*.example".into()]
        ));
    }

    #[test]
    fn active_resolver_search_and_domain_refresh_without_dns_resolution() {
        let root = tempfile::tempdir().unwrap();
        let etc = root.path().join("etc");
        let resolved = root.path().join("run/systemd/resolve");
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::create_dir_all(&resolved).unwrap();
        let path = etc.join("resolv.conf");
        std::fs::write(&path, "search LAN home.arpa. # comment\n").unwrap();
        assert_eq!(search_domains(root.path()), ["lan", "home.arpa"]);
        std::fs::write(
            &path,
            "search old.example\ndomain corp.example.com ; comment\n",
        )
        .unwrap();
        assert_eq!(search_domains(root.path()), ["corp.example.com"]);
        std::fs::write(
            resolved.join("resolv.conf"),
            "search global.example link.example ~route.only .\n",
        )
        .unwrap();
        // A stale resolved file must never expand a foreign resolver's policy.
        assert_eq!(search_domains(root.path()), ["corp.example.com"]);
        std::fs::write(&path, "nameserver 127.0.0.53\nsearch global.example\n").unwrap();
        assert_eq!(
            search_domains(root.path()),
            ["global.example", "link.example"]
        );
        std::fs::write(resolved.join("resolv.conf"), "search changed.example\n").unwrap();
        assert_eq!(search_domains(root.path()), ["changed.example"]);
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(resolved.join("resolv.conf"), &path).unwrap();
        assert_eq!(search_domains(root.path()), ["changed.example"]);
        std::fs::remove_file(&path).unwrap();
        assert!(search_domains(root.path()).is_empty());
        assert_eq!(
            resolver_domains("search *.evil bad_name http://evil evil:80 good.example\n"),
            ["good.example"]
        );
    }

    #[test]
    fn reflected_host_strips_controls_and_caps_length() {
        assert_eq!(display_host(&headers("evil\t.example")), "evil.example");
        assert_eq!(display_host(&headers(&"x".repeat(1024))).len(), 253);
        assert_eq!(display_host(&HeaderMap::new()), "(missing Host)");
    }

    #[test]
    fn literal_hostname_localhost_and_port_table() {
        let addresses = [
            "192.0.2.10".parse().unwrap(),
            "2001:db8::10".parse().unwrap(),
        ];
        for host in [
            "192.0.2.10",
            "192.0.2.10:80",
            "192.0.2.10:8080",
            "[2001:db8::10]",
            "[2001:db8::10]:80",
            "[2001:db8::10]:8080",
            "monitor",
            "MONITOR:8080",
            "monitor.local",
            "monitor.local:80",
            "monitor.local.",
            "localhost",
            "localhost:8080",
            "127.0.0.1",
            "127.0.0.2:80",
            "[::1]",
            "[::1]:8080",
            "[::ffff:127.0.0.1]:80",
        ] {
            assert!(
                matches_device(&headers(host), Some("monitor"), addresses),
                "{host}"
            );
        }
        for host in [
            "evil.example",
            "monitor.evil.example",
            "monitor.local.evil",
            "192.0.2.11",
            "[2001:db8::11]:80",
            "2001:db8::10",
            "0.0.0.0",
            "[::]",
            "monitor:65536",
            "user@monitor",
            "monitor/path",
            "monitor:bad",
            "monitor,evil",
        ] {
            assert!(
                !matches_device(&headers(host), Some("monitor"), addresses),
                "{host}"
            );
        }
        let mut duplicate = headers("monitor");
        duplicate.append(header::HOST, "evil.example".parse().unwrap());
        assert!(!matches_device(&duplicate, Some("monitor"), addresses));
        assert!(!matches_device(
            &HeaderMap::new(),
            Some("monitor"),
            addresses
        ));
    }

    #[test]
    fn network_move_uses_new_inventory_without_waiting_for_keep_or_cache_expiry() {
        let old: IpAddr = "192.0.2.10".parse().unwrap();
        let new: IpAddr = "198.51.100.20".parse().unwrap();
        let request = headers("198.51.100.20:8080");
        assert!(!matches_device(&request, Some("monitor"), [old]));
        assert!(matches_device(&request, Some("monitor"), [old, new]));
        assert!(matches_device(&request, Some("monitor"), [new]));
        assert!(!matches_device(
            &headers("192.0.2.10"),
            Some("monitor"),
            [new]
        ));
    }

    #[test]
    fn live_kernel_inventory_accepts_all_current_ip_addresses_and_hostname() {
        let interfaces = nix::ifaddrs::getifaddrs().unwrap();
        for interface in interfaces {
            let Some(address) = interface.address else {
                continue;
            };
            let host = address
                .as_sockaddr_in()
                .map(|a| a.ip().to_string())
                .or_else(|| address.as_sockaddr_in6().map(|a| format!("[{}]", a.ip())));
            if let Some(host) = host {
                assert!(allowed(&headers(&host)), "{host}");
            }
        }
        let hostname = nix::unistd::gethostname().unwrap();
        assert!(allowed(&headers(hostname.to_str().unwrap())));
    }
}
