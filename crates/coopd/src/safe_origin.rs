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
//! Disable with `COOP_PUBLIC=1` if you're deliberately exposing coopd to
//! a non-loopback interface (you still need `COOP_API_TOKEN` for that to
//! be safe; see `SECURITY.md`).

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
        return true;
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

/// Axum middleware: rejects requests whose Host or Origin is not loopback.
pub async fn require_safe_origin(req: Request, next: Next) -> Response {
    if std::env::var("COOP_PUBLIC").ok().as_deref() == Some("1") {
        return next.run(req).await;
    }
    let path = req.uri().path();
    // Exempt healthchecks so external probes still work even on bound-public
    // deployments that forgot to set COOP_PUBLIC=1.
    if path == "/api/v1/healthz" || path == "/api/v1/readyz" {
        return next.run(req).await;
    }
    let headers = req.headers();
    if let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
    {
        if !host_is_loopback(host) {
            return forbid("host not loopback (set COOP_PUBLIC=1 to allow)");
        }
    } else {
        return forbid("missing Host header");
    }
    if let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|h| h.to_str().ok())
    {
        if !origin_is_loopback(origin) {
            return forbid("origin not loopback");
        }
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
        assert!(origin_is_loopback("null"));
    }

    #[test]
    fn origin_deny() {
        assert!(!origin_is_loopback("http://evil.example.com"));
        assert!(!origin_is_loopback("file:///etc/passwd"));
        assert!(!origin_is_loopback("http://localhost.evil.com"));
        assert!(!origin_is_loopback("ftp://localhost"));
    }
}
