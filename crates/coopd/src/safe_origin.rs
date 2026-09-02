//! Origin/Host allowlist middleware — fixes C3 (WebSocket cross-origin
//! hijack) and C4 (browser-initiated CSRF against the JSON API).
//!
//! Browsers do **not** enforce the Same-Origin Policy on WebSocket
//! handshakes; any page on the open internet could otherwise open
//! `ws://127.0.0.1:9700/api/v1/hens/.../shell` from a visited page and run
//! commands in the user's PTY. Cookies on a `Bearer` cookie are likewise
//! sent on `<form action=…>` POSTs. The fix is the same in both cases:
//! refuse any request whose `Host` header isn't a loopback name, and
//! refuse any request with an `Origin` header that isn't a loopback URL.
//!
//! `COOP_PUBLIC=1` permits non-loopback hosts but still requires browser
//! origins to match the request host. Public mode also requires
//! `COOP_API_TOKEN`; see `SECURITY.md`.

use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Hosts (sans port) we accept on the `Host` header. IPv6 literals must
/// arrive wrapped in `[]` per RFC 7230.
const HOST_ALLOW: &[&str] = &["127.0.0.1", "localhost", "[::1]"];

/// Origins (scheme + host, sans port) we accept on the `Origin` header.
const ORIGIN_ALLOW: &[&str] = &[
    "http://127.0.0.1",
    "https://127.0.0.1",
    "http://localhost",
    "https://localhost",
    "http://[::1]",
    "https://[::1]",
];

fn host_is_loopback(host: &str) -> bool {
    // Strip optional `:<port>` suffix. IPv6 literals carry their own `[]`
    // and the port (if any) appears after the `]`.
    let bare = if host.starts_with('[') {
        match host.find(']') {
            Some(end) => &host[..=end],
            None => host,
        }
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    HOST_ALLOW.contains(&bare)
}

fn origin_is_loopback(origin: &str) -> bool {
    if origin == "null" {
        return false;
    }
    let scheme_end = match origin.find("://") {
        Some(i) => i + 3,
        None => return false,
    };
    let after_scheme = &origin[scheme_end..];
    let host_part = if after_scheme.starts_with('[') {
        match after_scheme.find(']') {
            Some(end) => &after_scheme[..=end],
            None => return false,
        }
    } else {
        after_scheme
            .split_once([':', '/'])
            .map_or(after_scheme, |(h, _)| h)
    };
    let prefix = &origin[..scheme_end];
    let candidate = format!("{prefix}{host_part}");
    ORIGIN_ALLOW.iter().any(|a| candidate == *a)
}

fn normalize_public_origin(raw: &str) -> Result<String, String> {
    let uri = raw
        .trim()
        .parse::<axum::http::Uri>()
        .map_err(|_| "COOP_PUBLIC_ORIGIN must be a valid http(s) origin".to_string())?;
    let scheme = uri
        .scheme_str()
        .filter(|scheme| matches!(*scheme, "http" | "https"))
        .ok_or_else(|| "COOP_PUBLIC_ORIGIN must use http or https".to_string())?;
    let authority = uri
        .authority()
        .map(|value| value.as_str())
        .filter(|value| !value.contains('@'))
        .ok_or_else(|| "COOP_PUBLIC_ORIGIN must include a host without userinfo".to_string())?;
    if uri.query().is_some() || !matches!(uri.path(), "" | "/") {
        return Err("COOP_PUBLIC_ORIGIN must not include a path or query".into());
    }
    Ok(format!("{scheme}://{authority}"))
}

pub fn configured_public_origin() -> Result<String, String> {
    let raw = std::env::var("COOP_PUBLIC_ORIGIN")
        .map_err(|_| "COOP_PUBLIC=1 requires COOP_PUBLIC_ORIGIN".to_string())?;
    normalize_public_origin(&raw)
}

fn public_request_matches(origin: &str, host: &str, configured: &str) -> bool {
    let Some((_, configured_authority)) = configured.split_once("://") else {
        return false;
    };
    origin.eq_ignore_ascii_case(configured) && host.eq_ignore_ascii_case(configured_authority)
}

/// Axum middleware: rejects requests whose Host or Origin is not loopback.
pub async fn require_safe_origin(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    // Exempt healthchecks so external probes still work even on bound-public
    // deployments that forgot to set COOP_PUBLIC=1.
    if path == "/api/v1/healthz" || path == "/api/v1/readyz" {
        return next.run(req).await;
    }
    let headers = req.headers();
    let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
    else {
        return forbid("missing Host header");
    };
    if std::env::var("COOP_PUBLIC").ok().as_deref() == Some("1") {
        let Ok(configured) = configured_public_origin() else {
            return forbid("public origin is not configured");
        };
        if let Some(origin) = headers
            .get(axum::http::header::ORIGIN)
            .and_then(|h| h.to_str().ok())
            && !public_request_matches(origin, host, &configured)
        {
            return forbid("origin or host does not match COOP_PUBLIC_ORIGIN");
        }
        return next.run(req).await;
    }
    if !host_is_loopback(host) {
        return forbid("host not loopback (set COOP_PUBLIC=1 to allow)");
    }
    if let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        && !origin_is_loopback(origin)
    {
        return forbid("origin not loopback");
    }
    next.run(req).await
}

fn forbid(msg: &'static str) -> Response {
    (
        StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "text/plain")],
        Body::from(msg),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allow_basic() {
        assert!(host_is_loopback("127.0.0.1"));
        assert!(host_is_loopback("127.0.0.1:9700"));
        assert!(host_is_loopback("localhost"));
        assert!(host_is_loopback("localhost:9700"));
        assert!(host_is_loopback("[::1]"));
        assert!(host_is_loopback("[::1]:9700"));
    }

    #[test]
    fn host_deny() {
        assert!(!host_is_loopback("evil.example.com"));
        assert!(!host_is_loopback("192.168.1.1"));
        assert!(!host_is_loopback("127.0.0.1.evil.com"));
    }

    #[test]
    fn origin_allow_basic() {
        assert!(origin_is_loopback("http://127.0.0.1"));
        assert!(origin_is_loopback("http://127.0.0.1:9700"));
        assert!(origin_is_loopback("http://localhost:5173"));
        assert!(origin_is_loopback("https://[::1]:443"));
        assert!(!origin_is_loopback("null"));
    }

    #[test]
    fn origin_deny() {
        assert!(!origin_is_loopback("http://evil.example.com"));
        assert!(!origin_is_loopback("file:///etc/passwd"));
        assert!(!origin_is_loopback("http://localhost.evil.com"));
        assert!(!origin_is_loopback("ftp://localhost"));
    }

    #[test]
    fn public_origin_must_match_configured_scheme_host_and_port() {
        let configured = normalize_public_origin("https://farm.example.com:9700/").unwrap();
        assert_eq!(configured, "https://farm.example.com:9700");
        assert!(public_request_matches(
            "https://farm.example.com:443",
            "farm.example.com:443",
            "https://farm.example.com:443"
        ));
        assert!(!public_request_matches(
            "http://farm.example.com:9700",
            "farm.example.com:9700",
            &configured
        ));
        assert!(!public_request_matches(
            "https://evil.example.com",
            "farm.example.com:9700",
            &configured
        ));
        assert!(!public_request_matches(
            "https://farm.example.com:9700",
            "farm.example.com",
            &configured
        ));
        assert!(normalize_public_origin("file:///tmp/farm").is_err());
        assert!(normalize_public_origin("https://farm.example/path").is_err());
    }
}
