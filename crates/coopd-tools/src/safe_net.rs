//! Shared helper: SSRF guard for the `http` tool.
//!
//! Rejects URLs whose host (after async DNS resolution) maps to any IP that
//! is loopback, link-local, multicast, unspecified, broadcast, RFC1918
//! private space, CGNAT (100.64/10), documentation, or IPv6 ULA/link-local.
//!
//! This is enforced for the initial URL *and* every redirect target — to
//! defeat both DNS rebinding (resolve happens at request time) and
//! open-redirect-into-localhost pivots.

use coopd_core::{CoreError, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Maximum redirect hops we will follow.
pub const MAX_REDIRECTS: usize = 3;

/// Returns true if `ip` belongs to any address space we must refuse for
/// SSRF protection.
#[must_use]
pub fn is_disallowed_ip(ip: IpAddr) -> bool {
    if ip.is_loopback() || ip.is_multicast() || ip.is_unspecified() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || is_cgnat(v4)
                || is_v4_reserved(v4)
        }
        IpAddr::V6(v6) => is_v6_unique_local(v6) || is_v6_link_local(v6) || is_v6_documentation(v6),
    }
}

fn is_cgnat(v4: Ipv4Addr) -> bool {
    // 100.64.0.0/10
    let o = v4.octets();
    o[0] == 100 && (o[1] & 0b1100_0000) == 64
}

fn is_v4_reserved(v4: Ipv4Addr) -> bool {
    // 0.0.0.0/8 (already covered by is_unspecified for 0.0.0.0 itself, but
    // the full /8 is "this network" and should not be reachable).
    let o = v4.octets();
    o[0] == 0
}

fn is_v6_unique_local(v6: Ipv6Addr) -> bool {
    // fc00::/7
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

fn is_v6_link_local(v6: Ipv6Addr) -> bool {
    // fe80::/10
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

fn is_v6_documentation(v6: Ipv6Addr) -> bool {
    // 2001:db8::/32
    let s = v6.segments();
    s[0] == 0x2001 && s[1] == 0x0db8
}

/// Validate that `url`'s host resolves only to public, routable IPs.
///
/// # Errors
///
/// Returns [`CoreError::Other`] if the URL scheme is not http/https, the
/// host cannot be parsed/resolved, or any resolved IP is in a disallowed
/// range (loopback, RFC1918, link-local, ULA, etc.).
pub async fn validate_url(url: &str) -> Result<()> {
    let parsed =
        reqwest::Url::parse(url).map_err(|e| CoreError::Other(format!("invalid url: {e}")))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(CoreError::Other(format!("scheme not allowed: {scheme}")));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| CoreError::Other("url has no host".into()))?;
    let port = parsed.port_or_known_default().unwrap_or(0);

    // Resolve. If the host is an IP literal `lookup_host` handles it.
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| CoreError::Other(format!("dns lookup {host}: {e}")))?;

    let mut any = false;
    for sa in addrs {
        any = true;
        if is_disallowed_ip(sa.ip()) {
            return Err(CoreError::Other(format!(
                "ssrf: refusing connection to disallowed address {sa}"
            )));
        }
    }
    if !any {
        return Err(CoreError::Other(format!("dns: no addresses for {host}")));
    }
    Ok(())
}

/// Enforce a per-hen [`ResolvedNetPolicy`](coopd_core::ResolvedNetPolicy)
/// against a URL (L7 host+port allowlist). This is the in-process counterpart
/// to the OS network sandbox: it gates the `http` tool, which runs with the
/// daemon's privileges.
///
/// - `open` → permitted (the SSRF guard in [`validate_url`] still applies).
/// - `off` → always refused.
/// - `allowlist` → refused unless the URL's host+port matches an allow entry.
///
/// # Errors
///
/// Returns [`CoreError::Other`] when the URL cannot be parsed or the policy
/// denies the egress.
pub fn enforce_policy(policy: &coopd_core::ResolvedNetPolicy, url: &str) -> Result<()> {
    use coopd_core::NetPolicy;
    if policy.policy() == NetPolicy::Open {
        return Ok(());
    }
    let parsed =
        reqwest::Url::parse(url).map_err(|e| CoreError::Other(format!("invalid url: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| CoreError::Other("url has no host".into()))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| CoreError::Other("url has no port".into()))?;
    if policy.host_allowed(host, port) {
        Ok(())
    } else {
        Err(CoreError::Other(format!(
            "network policy ({}) denies egress to {host}:{port}",
            policy.policy().as_str()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_loopback() {
        assert!(is_disallowed_ip("127.0.0.1".parse().unwrap()));
        assert!(is_disallowed_ip("::1".parse().unwrap()));
    }

    #[test]
    fn blocks_rfc1918() {
        for s in ["10.0.0.1", "172.16.0.1", "192.168.1.1"] {
            assert!(is_disallowed_ip(s.parse().unwrap()), "{s}");
        }
    }

    #[test]
    fn blocks_link_local() {
        assert!(is_disallowed_ip("169.254.169.254".parse().unwrap()));
        assert!(is_disallowed_ip("fe80::1".parse().unwrap()));
    }

    #[test]
    fn blocks_cgnat() {
        assert!(is_disallowed_ip("100.64.0.1".parse().unwrap()));
        assert!(is_disallowed_ip("100.127.255.254".parse().unwrap()));
        // Just outside CGNAT range:
        assert!(!is_disallowed_ip("100.128.0.1".parse().unwrap()));
    }

    #[test]
    fn blocks_unique_local_v6() {
        assert!(is_disallowed_ip("fc00::1".parse().unwrap()));
        assert!(is_disallowed_ip("fd00::1".parse().unwrap()));
    }

    #[test]
    fn allows_public() {
        assert!(!is_disallowed_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_disallowed_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_disallowed_ip("2606:4700::1111".parse().unwrap()));
    }

    #[tokio::test]
    async fn rejects_file_scheme() {
        let e = validate_url("file:///etc/passwd").await.unwrap_err();
        assert!(format!("{e}").contains("scheme"));
    }

    #[tokio::test]
    async fn rejects_localhost() {
        let e = validate_url("http://127.0.0.1:9700/api").await.unwrap_err();
        assert!(format!("{e}").contains("ssrf") || format!("{e}").contains("disallowed"));
    }

    #[tokio::test]
    async fn rejects_metadata_ip() {
        let e = validate_url("http://169.254.169.254/latest")
            .await
            .unwrap_err();
        assert!(format!("{e}").contains("ssrf") || format!("{e}").contains("disallowed"));
    }
}
