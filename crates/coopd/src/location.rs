//! Farm location info — what URLs should other devices use to reach this farm?
//!
//! Other devices on the LAN (Pi flocks, your laptop visiting your home farm,
//! the mobile Alpha-Signal app, etc.) need a stable URL pointing at this
//! daemon. We compute it from the bound listen address plus enumerated
//! non-loopback IPv4 interfaces, so the user can copy a working URL out of
//! the Farm UI without `ifconfig`-ing themselves.

use serde::Serialize;
use std::net::SocketAddr;

#[derive(Debug, Clone, Serialize)]
pub struct FarmLocation {
    /// `gethostname()`, e.g. `my-host.local`.
    pub hostname: String,
    /// Listen address as reported by the daemon (e.g. `127.0.0.1:9700`
    /// or `0.0.0.0:9700`).
    pub bound_addr: String,
    /// Whether `bound_addr` is loopback-only (i.e. NOT reachable from
    /// other devices). The UI shows a hint when true.
    pub loopback_only: bool,
    /// Suggested URLs to share. Always includes `http://127.0.0.1:<port>`;
    /// when bound to `0.0.0.0`, also includes one entry per non-loopback
    /// IPv4 interface and a `http://<hostname>:<port>` entry.
    pub urls: Vec<UrlEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UrlEntry {
    pub label: String,
    pub url: String,
    /// Hint about reachability: `local` (this device only) or `lan`
    /// (likely reachable from other devices on the same network).
    pub scope: &'static str,
}

/// Build the farm location info from the daemon's bound address.
#[must_use]
pub fn compute(bound: &str) -> FarmLocation {
    let port = bound
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(9700);

    let hostname = hostname_lossy();
    let parsed: Option<SocketAddr> = bound.parse().ok();
    let loopback_only = parsed.is_some_and(|s| s.ip().is_loopback());

    let mut urls = vec![UrlEntry {
        label: "Loopback".into(),
        url: format!("http://127.0.0.1:{port}/"),
        scope: "local",
    }];

    // Only advertise LAN URLs when the daemon is actually bound to
    // something other devices can reach.
    if !loopback_only {
        if !hostname.is_empty() && hostname != "localhost" {
            urls.push(UrlEntry {
                label: format!("Hostname ({hostname})"),
                url: format!("http://{hostname}:{port}/"),
                scope: "lan",
            });
        }
        for (name, ip) in lan_ipv4s() {
            urls.push(UrlEntry {
                label: format!("{name} ({ip})"),
                url: format!("http://{ip}:{port}/"),
                scope: "lan",
            });
        }
    }

    FarmLocation {
        hostname,
        bound_addr: bound.to_string(),
        loopback_only,
        urls,
    }
}

fn hostname_lossy() -> String {
    // Use libc indirectly via std on unix; fall back to env.
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

fn lan_ipv4s() -> Vec<(String, std::net::Ipv4Addr)> {
    let Ok(ifs) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ifc in ifs {
        if ifc.is_loopback() {
            continue;
        }
        if let std::net::IpAddr::V4(v4) = ifc.ip() {
            if v4.is_link_local() {
                continue;
            }
            out.push((ifc.name.clone(), v4));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_only_when_bound_to_127() {
        let loc = compute("127.0.0.1:9700");
        assert!(loc.loopback_only);
        assert_eq!(loc.urls.len(), 1);
        assert_eq!(loc.urls[0].scope, "local");
    }

    #[test]
    fn lan_urls_when_bound_to_all() {
        let loc = compute("0.0.0.0:9700");
        assert!(!loc.loopback_only);
        assert!(loc.urls.iter().any(|u| u.scope == "local"));
        // Can't guarantee a LAN interface in CI, but the loopback entry
        // must always be first.
        assert_eq!(loc.urls[0].url, "http://127.0.0.1:9700/");
    }

    #[test]
    fn parses_port_from_addr() {
        let loc = compute("0.0.0.0:8080");
        assert!(loc.urls[0].url.contains(":8080"));
    }
}
