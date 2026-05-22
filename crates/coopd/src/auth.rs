//! Optional bearer-token authentication for the public coopd endpoint.
//!
//! Activation is opt-in: when the `COOP_API_TOKEN` env var is unset (or empty)
//! the middleware is a no-op, preserving the local-dev loopback workflow.
//! When set, every request to `/api/v1/*` and the UI must present the token via
//! one of:
//!
//!   * `Authorization: Bearer <token>` header
//!   * `?token=<token>` query parameter
//!   * `coop_token=<token>` cookie (set by `POST /api/v1/auth/login`)
//!
//! A small `/login` HTML page is served so the browser UI can authenticate.
//! Healthchecks (`/api/v1/healthz`, `/api/v1/readyz`) and the login endpoints
//! themselves are always exempt.

use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// `None` = auth disabled. `Some(token)` = require this token.
    token: Option<Arc<String>>,
}

impl AuthConfig {
    pub fn from_env() -> Self {
        let token = std::env::var("COOP_API_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(Arc::new);
        Self { token }
    }

    pub fn enabled(&self) -> bool {
        self.token.is_some()
    }

    fn matches(&self, presented: &str) -> bool {
        match &self.token {
            None => true,
            Some(expected) => ct_eq(expected.as_bytes(), presented.as_bytes()),
        }
    }
}

/// Constant-time byte slice equality.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

const EXEMPT_PATHS: &[&str] = &[
    "/api/v1/healthz",
    "/api/v1/readyz",
    "/api/v1/auth/login",
    "/api/v1/auth/status",
    "/login",
    "/favicon.ico",
];

/// Wrap an existing router with the auth middleware + login routes.
pub fn install(app: Router, cfg: AuthConfig) -> Router {
    let login_routes = Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/status", get(status))
        .route("/login", get(login_page))
        .with_state(cfg.clone());

    app.merge(login_routes)
        .layer(middleware::from_fn_with_state(cfg, require_token))
}

async fn require_token(State(cfg): State<AuthConfig>, req: Request, next: Next) -> Response {
    if !cfg.enabled() {
        return next.run(req).await;
    }

    let path = req.uri().path();
    if EXEMPT_PATHS.contains(&path) {
        return next.run(req).await;
    }

    let presented = extract_token(req.headers(), req.uri().query());
    if let Some(tok) = presented {
        if cfg.matches(&tok) {
            return next.run(req).await;
        }
    }

    // Browser → redirect to /login. API client → 401 JSON.
    let wants_html = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.contains("text/html"));

    if wants_html {
        Redirect::to("/login").into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer realm=\"coopd\"")],
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response()
    }
}

fn extract_token(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    // 1. Authorization: Bearer <token>
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
    {
        if let Some(rest) = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
        {
            let t = rest.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    // 2. ?token=...
    if let Some(q) = query {
        for pair in q.split('&') {
            let mut parts = pair.splitn(2, '=');
            let k = parts.next().unwrap_or("");
            let v = parts.next().unwrap_or("");
            if k == "token" && !v.is_empty() {
                return Some(url_decode(v));
            }
        }
    }
    // 3. Cookie: coop_token=...
    if let Some(c) = headers.get(header::COOKIE).and_then(|h| h.to_str().ok()) {
        for pair in c.split(';') {
            let pair = pair.trim();
            if let Some(v) = pair.strip_prefix("coop_token=") {
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[derive(Deserialize)]
struct LoginBody {
    token: String,
}

#[derive(Serialize)]
struct StatusBody {
    enabled: bool,
    authenticated: bool,
}

async fn status(State(cfg): State<AuthConfig>, headers: HeaderMap) -> Json<StatusBody> {
    let authenticated = if !cfg.enabled() {
        true
    } else {
        extract_token(&headers, None).is_some_and(|t| cfg.matches(&t))
    };
    Json(StatusBody {
        enabled: cfg.enabled(),
        authenticated,
    })
}

async fn login(State(cfg): State<AuthConfig>, Json(body): Json<LoginBody>) -> Response {
    if !cfg.enabled() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"ok": true, "auth": "disabled"})),
        )
            .into_response();
    }
    if !cfg.matches(&body.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid token"})),
        )
            .into_response();
    }
    // 30-day session cookie. HttpOnly + SameSite=Lax. Secure flag is set if the
    // request arrives over HTTPS (which is true behind Cloudflare).
    let cookie = format!(
        "coop_token={}; Path=/; Max-Age=2592000; HttpOnly; SameSite=Lax; Secure",
        body.token
    );
    let mut resp = (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    resp
}

async fn logout() -> Response {
    let cookie = "coop_token=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax";
    let mut resp = (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_static(cookie));
    resp
}

const LOGIN_HTML: &str = include_str!("ui/login.html");

async fn login_page() -> Html<&'static str> {
    Html(LOGIN_HTML)
}

#[allow(dead_code)]
fn _body_marker(_: Body) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_works() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }

    #[test]
    fn disabled_when_env_empty() {
        // Cannot rely on env in tests; just verify the struct path.
        let cfg = AuthConfig { token: None };
        assert!(!cfg.enabled());
        assert!(cfg.matches("anything"));
    }

    #[test]
    fn enabled_matches_only_exact() {
        let cfg = AuthConfig {
            token: Some(Arc::new("s3cret".into())),
        };
        assert!(cfg.enabled());
        assert!(cfg.matches("s3cret"));
        assert!(!cfg.matches("wrong"));
    }

    #[test]
    fn url_decode_basic() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("plain"), "plain");
    }
}
