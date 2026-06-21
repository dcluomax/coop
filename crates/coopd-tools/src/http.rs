//! `http` tool: make an HTTP request.

use async_trait::async_trait;
use coopd_core::{CoopTool, CoreError, Result, ToolCapability, ToolCtx, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

/// `http` tool — perform an HTTP request.
#[derive(Debug)]
pub struct Http {
    client: reqwest::Client,
}

impl Http {
    /// Construct a new HTTP tool with a shared `reqwest` client.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest::Client` builder fails (e.g. the
    /// platform has no TLS backend available). In practice this only fails
    /// in extremely broken environments and is treated as unrecoverable at
    /// daemon startup.
    pub fn new() -> Self {
        // Disable auto-redirects — we run our own bounded, SSRF-checked loop.
        let client = reqwest::Client::builder()
            .user_agent(concat!("coopd-tools/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build reqwest client");
        Self { client }
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct Input {
    url: String,
    #[serde(default = "default_method")]
    method: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

fn default_method() -> String {
    "GET".to_string()
}

#[derive(Debug, Serialize)]
struct Output {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
    truncated: bool,
}

const MAX_BODY: usize = 1024 * 1024; // 1 MiB cap returned to model.
const CAPS: &[ToolCapability] = &[ToolCapability::NetOut];

#[async_trait]
impl CoopTool for Http {
    fn name(&self) -> &'static str {
        "http"
    }
    fn version(&self) -> &'static str {
        "v1.0.0"
    }
    fn capabilities(&self) -> &'static [ToolCapability] {
        CAPS
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            description: "Make an HTTP request (GET/POST/PUT/DELETE) and return the response."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "method": { "type": "string", "enum": ["GET","POST","PUT","DELETE","PATCH","HEAD"], "default": "GET" },
                    "headers": { "type": "object", "additionalProperties": { "type": "string" } },
                    "body": { "type": ["string","null"] }
                },
                "required": ["url"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "status": { "type": "integer" },
                    "headers": { "type": "object" },
                    "body": { "type": "string" },
                    "truncated": { "type": "boolean" }
                },
                "required": ["status","headers","body","truncated"]
            }),
            examples: vec![],
        }
    }
    async fn invoke(&self, ctx: &ToolCtx, input: Value) -> Result<Value> {
        let inp: Input = serde_json::from_value(input)?;
        let method = reqwest::Method::from_bytes(inp.method.as_bytes())
            .map_err(|e| CoreError::Other(format!("invalid method: {e}")))?;

        let mut current_url = inp.url.clone();
        let mut hops = 0usize;
        let resp = loop {
            // Per-hen network policy (L7): off => deny; allowlist => host+port
            // must match. `open` falls through to the SSRF guard below. This is
            // enforced for the initial URL *and* every redirect target.
            crate::safe_net::enforce_policy(&ctx.net_policy, &current_url)?;
            crate::safe_net::validate_url(&current_url).await?;
            let mut req = self.client.request(method.clone(), &current_url);
            for (k, v) in &inp.headers {
                req = req.header(k, v);
            }
            if let Some(ref b) = inp.body {
                req = req.body(b.clone());
            }
            let r = req
                .send()
                .await
                .map_err(|e| CoreError::Other(format!("http: {e}")))?;
            if r.status().is_redirection() {
                if hops >= crate::safe_net::MAX_REDIRECTS {
                    return Err(CoreError::Other(format!(
                        "too many redirects (> {})",
                        crate::safe_net::MAX_REDIRECTS
                    )));
                }
                let Some(loc) = r.headers().get(reqwest::header::LOCATION) else {
                    break r;
                };
                let loc_str = loc
                    .to_str()
                    .map_err(|e| CoreError::Other(format!("bad Location header: {e}")))?;
                // Resolve relative redirects against the previous URL.
                let next = reqwest::Url::parse(&current_url)
                    .and_then(|base| base.join(loc_str))
                    .map_err(|e| CoreError::Other(format!("bad redirect target: {e}")))?;
                current_url = next.into();
                hops += 1;
                continue;
            }
            break r;
        };
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| CoreError::Other(format!("http body: {e}")))?;
        let truncated = bytes.len() > MAX_BODY;
        let slice = if truncated {
            &bytes[..MAX_BODY]
        } else {
            &bytes[..]
        };
        let body = String::from_utf8_lossy(slice).into_owned();
        Ok(serde_json::to_value(Output {
            status,
            headers,
            body,
            truncated,
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::CoopTool;

    #[tokio::test]
    async fn ssrf_blocks_loopback() {
        let h = Http::new();
        let ctx = crate::test_ctx(std::env::temp_dir());
        let err = h
            .invoke(&ctx, json!({ "url": "http://127.0.0.1:1/" }))
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("ssrf") || msg.contains("disallowed"),
            "got: {msg}"
        );
    }

    #[tokio::test]
    async fn ssrf_blocks_file_scheme() {
        let h = Http::new();
        let ctx = crate::test_ctx(std::env::temp_dir());
        let err = h
            .invoke(&ctx, json!({ "url": "file:///etc/passwd" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("scheme"));
    }

    fn ctx_with_policy(p: coopd_core::ResolvedNetPolicy) -> ToolCtx {
        ToolCtx {
            agent_id: "alice.coop/aria".into(),
            session_id: "test".into(),
            lease_id: None,
            workdir: std::env::temp_dir(),
            net_policy: p,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
            delegation_depth: 0,
            delegator: None,
        }
    }

    #[tokio::test]
    async fn off_policy_denies_http_tool() {
        let h = Http::new();
        let policy = coopd_core::ResolvedNetPolicy::from_spec(Some(&coopd_core::NetworkSpec {
            policy: coopd_core::NetPolicy::Off,
            allow: vec![],
        }));
        let err = h
            .invoke(
                &ctx_with_policy(policy),
                json!({ "url": "https://example.com/" }),
            )
            .await
            .unwrap_err();
        // Denied by policy BEFORE any DNS/connect happens (hermetic).
        assert!(format!("{err}").contains("network policy"), "got: {err}");
    }

    #[tokio::test]
    async fn allowlist_policy_denies_unlisted_host() {
        let h = Http::new();
        let policy = coopd_core::ResolvedNetPolicy::from_spec(Some(&coopd_core::NetworkSpec {
            policy: coopd_core::NetPolicy::Allowlist,
            allow: vec![coopd_core::NetAllow {
                host: "api.anthropic.com".into(),
                ports: vec![443],
            }],
        }));
        let err = h
            .invoke(
                &ctx_with_policy(policy),
                json!({ "url": "https://evil.example.com/" }),
            )
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("network policy"),
            "unlisted host should be denied, got: {err}"
        );
    }
}
