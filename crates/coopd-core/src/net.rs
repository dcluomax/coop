//! Per-hen network policy: the manifest `network:` block plus a compiled,
//! allocation-light matcher (`ResolvedNetPolicy`) that the tools and the
//! sandbox consult to decide whether an egress is permitted.
//!
//! The policy is the *intent*. Enforcement lives in two places:
//! - the `http` tool (in-process, L7 host+port allowlist on top of the
//!   existing SSRF guard) — portable across every OS;
//! - the `bash`/tmux OS sandbox (Linux network namespace / macOS Seatbelt) —
//!   denies *direct* socket egress for `off`/`allowlist` hens.
//!
//! See `docs/net-isolation.md` for the full threat model and enforcement
//! matrix. This module is pure types + matching: it performs **no** DNS and
//! **no** I/O, so it stays in `coopd-core`.

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Network egress posture for a hen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetPolicy {
    /// No egress at all (neither `bash`/tmux nor the `http` tool).
    Off,
    /// Egress permitted **only** to hosts/ports in the allow list. Default-deny.
    Allowlist,
    /// Unrestricted egress. The `http` tool still applies SSRF protection
    /// (loopback / RFC1918 / link-local are always refused).
    #[default]
    Open,
}

impl NetPolicy {
    /// Stable identifier used in serialized YAML / JSON and log lines.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Allowlist => "allowlist",
            Self::Open => "open",
        }
    }

    /// Whether this policy is stricter than `open` and therefore requires an
    /// OS-level enforcement mechanism (network namespace / Seatbelt). When the
    /// host cannot enforce it, the hen must **refuse to hatch** (fail closed).
    #[must_use]
    pub fn requires_enforcement(self) -> bool {
        matches!(self, Self::Off | Self::Allowlist)
    }
}

/// A single allow-list entry: a host pattern plus the TCP ports it covers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetAllow {
    /// Host pattern. Either an exact host (`api.anthropic.com`), a single
    /// leading-label wildcard (`*.githubusercontent.com`), or an IP literal.
    pub host: String,
    /// Allowed TCP ports. Defaults to `[443]` when omitted.
    #[serde(default = "default_https_ports")]
    pub ports: Vec<u16>,
}

fn default_https_ports() -> Vec<u16> {
    vec![443]
}

/// The `network:` block of an `agent.yaml` manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSpec {
    /// Egress posture. Required when the `network:` block is present.
    pub policy: NetPolicy,
    /// Allow list. Only meaningful (and only permitted) when `policy: allowlist`.
    #[serde(default)]
    pub allow: Vec<NetAllow>,
}

impl NetworkSpec {
    /// Validate the spec, rejecting malformed hosts / illegal combinations.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidManifest`] when:
    /// - `allow` is non-empty for an `off`/`open` policy;
    /// - a host is empty, contains a scheme / path / port / whitespace, uses a
    ///   wildcard other than a single leading `*.`, or is non-ASCII (IDNs must
    ///   be supplied as punycode, e.g. `xn--…`);
    /// - a port is `0`.
    pub fn validate(&self) -> Result<()> {
        if self.policy != NetPolicy::Allowlist && !self.allow.is_empty() {
            return Err(CoreError::InvalidManifest(format!(
                "network.allow is only valid with policy: allowlist (got {})",
                self.policy.as_str()
            )));
        }
        for entry in &self.allow {
            validate_host_pattern(&entry.host)?;
            for &p in &entry.ports {
                if p == 0 {
                    return Err(CoreError::InvalidManifest(format!(
                        "network.allow host {:?} has an invalid port 0",
                        entry.host
                    )));
                }
            }
        }
        Ok(())
    }
}

fn validate_host_pattern(host: &str) -> Result<()> {
    let bad = |why: &str| {
        Err(CoreError::InvalidManifest(format!(
            "network.allow host {host:?} is invalid: {why}"
        )))
    };
    if host.is_empty() {
        return bad("empty");
    }
    if !host.is_ascii() {
        return bad("non-ASCII; supply IDNs as punycode (xn--…)");
    }
    if host.contains([' ', '\t', '/', ':', '@', '?', '#']) || host.contains("//") {
        return bad("must be a bare host (no scheme, path, port, or whitespace)");
    }
    // Wildcards: only a single leading "*." is permitted.
    if let Some(rest) = host.strip_prefix("*.") {
        if rest.is_empty() || rest.contains('*') {
            return bad("wildcard must be a single leading '*.' followed by a domain");
        }
    } else if host.contains('*') {
        return bad("'*' is only allowed as a leading '*.' label");
    }
    Ok(())
}

/// A compiled, lowercase host matcher.
#[derive(Debug, Clone)]
enum HostMatcher {
    /// Exact host (already lowercased).
    Exact(String),
    /// Suffix wildcard: stored without the leading `*`, e.g. `.example.com`.
    /// Matches `a.example.com` but **not** the apex `example.com`.
    Suffix(String),
}

impl HostMatcher {
    fn compile(pattern: &str) -> Self {
        let lower = pattern.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("*.") {
            Self::Suffix(format!(".{rest}"))
        } else {
            Self::Exact(lower)
        }
    }

    fn matches(&self, host_lower: &str) -> bool {
        match self {
            Self::Exact(h) => h == host_lower,
            Self::Suffix(suffix) => host_lower.ends_with(suffix.as_str()),
        }
    }
}

#[derive(Debug, Clone)]
struct AllowRule {
    matcher: HostMatcher,
    ports: Vec<u16>,
}

/// A manifest network policy compiled into a fast egress matcher.
///
/// Cheap to clone and store on `ToolCtx`. Construct with
/// [`ResolvedNetPolicy::from_spec`]. Defaults to [`NetPolicy::Open`] so a hen
/// with no `network:` block keeps working (backward compatible).
#[derive(Debug, Clone)]
pub struct ResolvedNetPolicy {
    policy: NetPolicy,
    rules: Vec<AllowRule>,
}

impl Default for ResolvedNetPolicy {
    fn default() -> Self {
        Self {
            policy: NetPolicy::Open,
            rules: Vec::new(),
        }
    }
}

impl ResolvedNetPolicy {
    /// Compile a manifest spec into a matcher. `None` (no `network:` block)
    /// resolves to [`NetPolicy::Open`] for backward compatibility.
    #[must_use]
    pub fn from_spec(spec: Option<&NetworkSpec>) -> Self {
        let Some(spec) = spec else {
            return Self::default();
        };
        let rules = spec
            .allow
            .iter()
            .map(|a| AllowRule {
                matcher: HostMatcher::compile(&a.host),
                ports: a.ports.clone(),
            })
            .collect();
        Self {
            policy: spec.policy,
            rules,
        }
    }

    /// The underlying policy.
    #[must_use]
    pub fn policy(&self) -> NetPolicy {
        self.policy
    }

    /// Whether **all** egress is denied: either `off`, or `allowlist` with an
    /// empty allow list (nothing can ever match).
    #[must_use]
    pub fn egress_denied_entirely(&self) -> bool {
        match self.policy {
            NetPolicy::Off => true,
            NetPolicy::Allowlist => self.rules.is_empty(),
            NetPolicy::Open => false,
        }
    }

    /// Whether `bash`/tmux must run with **no direct network** (empty netns /
    /// Seatbelt deny). True for every policy stricter than `open`; under
    /// `allowlist`, host-scoped egress is delivered via the `http` tool only
    /// (v1) — see `docs/net-isolation.md`.
    #[must_use]
    pub fn bash_egress_denied(&self) -> bool {
        self.policy.requires_enforcement()
    }

    /// Whether an L7 egress to `host:port` is permitted under this policy.
    ///
    /// - `off` → always `false`.
    /// - `open` → always `true` (the caller still applies the SSRF guard).
    /// - `allowlist` → `true` iff some entry matches the host **and** lists the
    ///   port. `host` is compared case-insensitively; IDNs must already be in
    ///   punycode (validated at manifest load).
    #[must_use]
    pub fn host_allowed(&self, host: &str, port: u16) -> bool {
        match self.policy {
            NetPolicy::Off => false,
            NetPolicy::Open => true,
            NetPolicy::Allowlist => {
                let host_lower = host.to_ascii_lowercase();
                self.rules
                    .iter()
                    .any(|r| r.matcher.matches(&host_lower) && r.ports.contains(&port))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(policy: NetPolicy, allow: &[(&str, &[u16])]) -> NetworkSpec {
        NetworkSpec {
            policy,
            allow: allow
                .iter()
                .map(|(h, ports)| NetAllow {
                    host: (*h).to_string(),
                    ports: ports.to_vec(),
                })
                .collect(),
        }
    }

    #[test]
    fn open_allows_everything() {
        let r = ResolvedNetPolicy::default();
        assert_eq!(r.policy(), NetPolicy::Open);
        assert!(r.host_allowed("evil.example.com", 443));
        assert!(!r.egress_denied_entirely());
        assert!(!r.bash_egress_denied());
    }

    #[test]
    fn off_denies_everything() {
        let s = spec(NetPolicy::Off, &[]);
        let r = ResolvedNetPolicy::from_spec(Some(&s));
        assert!(!r.host_allowed("api.anthropic.com", 443));
        assert!(r.egress_denied_entirely());
        assert!(r.bash_egress_denied());
    }

    #[test]
    fn allowlist_exact_host_and_port() {
        let s = spec(NetPolicy::Allowlist, &[("api.anthropic.com", &[443])]);
        let r = ResolvedNetPolicy::from_spec(Some(&s));
        assert!(r.host_allowed("api.anthropic.com", 443));
        assert!(r.host_allowed("API.Anthropic.COM", 443)); // case-insensitive
        assert!(!r.host_allowed("api.anthropic.com", 80)); // wrong port
        assert!(!r.host_allowed("evil.com", 443)); // wrong host
        assert!(r.bash_egress_denied()); // bash still gets no direct net
    }

    #[test]
    fn allowlist_suffix_wildcard() {
        let s = spec(NetPolicy::Allowlist, &[("*.githubusercontent.com", &[443])]);
        let r = ResolvedNetPolicy::from_spec(Some(&s));
        assert!(r.host_allowed("raw.githubusercontent.com", 443));
        assert!(r.host_allowed("a.b.githubusercontent.com", 443));
        // Apex is NOT matched by a wildcard (must be listed explicitly).
        assert!(!r.host_allowed("githubusercontent.com", 443));
        // Suffix must not be fooled by a lookalike registrable domain.
        assert!(!r.host_allowed("evilgithubusercontent.com", 443));
        assert!(!r.host_allowed("githubusercontent.com.evil.com", 443));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let s = spec(NetPolicy::Allowlist, &[]);
        let r = ResolvedNetPolicy::from_spec(Some(&s));
        assert!(r.egress_denied_entirely());
        assert!(!r.host_allowed("anything.com", 443));
    }

    #[test]
    fn validation_rejects_bad_hosts() {
        assert!(
            spec(NetPolicy::Allowlist, &[("https://x.com", &[443])])
                .validate()
                .is_err()
        );
        assert!(
            spec(NetPolicy::Allowlist, &[("x.com:443", &[443])])
                .validate()
                .is_err()
        );
        assert!(
            spec(NetPolicy::Allowlist, &[("a.*.com", &[443])])
                .validate()
                .is_err()
        );
        assert!(
            spec(NetPolicy::Allowlist, &[("*", &[443])])
                .validate()
                .is_err()
        );
        assert!(
            spec(NetPolicy::Allowlist, &[("ху.com", &[443])]) // non-ASCII
                .validate()
                .is_err()
        );
        assert!(
            spec(NetPolicy::Allowlist, &[("x.com", &[0])])
                .validate()
                .is_err()
        );
    }

    #[test]
    fn validation_rejects_allow_on_non_allowlist() {
        assert!(
            spec(NetPolicy::Off, &[("x.com", &[443])])
                .validate()
                .is_err()
        );
        assert!(
            spec(NetPolicy::Open, &[("x.com", &[443])])
                .validate()
                .is_err()
        );
    }

    #[test]
    fn validation_accepts_good_spec() {
        let s = spec(
            NetPolicy::Allowlist,
            &[("api.anthropic.com", &[443]), ("*.example.com", &[80, 443])],
        );
        assert!(s.validate().is_ok());
    }
}
