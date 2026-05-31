//! Azure Key Vault BYOK backend.
//!
//! Lets a Hen's `brain.provider_id` resolve to a secret stored in **Azure Key
//! Vault** instead of (or alongside) the local sealed file vault. The local
//! vault is still the zero-dependency default; Azure KV is for farmers who
//! already centralize their model keys in Azure and want Coop to fetch them at
//! run time rather than copy them onto the box.
//!
//! ## Provider reference syntax
//!
//! ```text
//! azure-kv://<vault-name>/<secret-name>
//! azure-kv://<vault-name>/<secret-name>/<version>
//! ```
//!
//! e.g. `azure-kv://my-coop-kv/byok-anthropic`.
//!
//! ## Authentication (BYOK, env-driven — the Azure `EnvironmentCredential` model)
//!
//! In order of precedence:
//!
//! 1. **Static bearer token** — `AZURE_KEYVAULT_TOKEN`. Bring your own
//!    already-acquired AAD access token (e.g. minted by a managed identity or
//!    `az account get-access-token --resource https://vault.azure.net`). Coop
//!    never refreshes it; supply a fresh one if it expires.
//! 2. **Service principal (client credentials)** — `AZURE_TENANT_ID`,
//!    `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`. Coop performs the OAuth2
//!    client-credentials flow against AAD and caches the token until shortly
//!    before it expires.
//!
//! Optional overrides for sovereign / national clouds:
//!
//! * `AZURE_KEYVAULT_DNS_SUFFIX` (default `vault.azure.net`)
//! * `AZURE_AUTHORITY_HOST` (default `https://login.microsoftonline.com`)
//!
//! Secret values are wrapped in [`zeroize::Zeroizing`] and never logged.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

/// Azure Key Vault REST API version targeted by this client.
const KEYVAULT_API_VERSION: &str = "7.4";
/// AAD scope requested for Key Vault data-plane access.
const KEYVAULT_SCOPE: &str = "https://vault.azure.net/.default";
/// Default public-cloud Key Vault DNS suffix.
const DEFAULT_DNS_SUFFIX: &str = "vault.azure.net";
/// Default public-cloud AAD authority host.
const DEFAULT_AUTHORITY_HOST: &str = "https://login.microsoftonline.com";
/// Refresh a cached token this long before its real expiry (clock skew guard).
const TOKEN_SKEW: Duration = Duration::from_secs(60);

/// Errors emitted by the Azure Key Vault backend.
#[derive(Debug, Error)]
pub enum AzureError {
    /// Required credential environment variables are missing or incomplete.
    #[error("azure credentials not configured: {0}")]
    MissingCredentials(String),
    /// The `azure-kv://` reference could not be parsed.
    #[error("invalid azure-kv reference: {0}")]
    InvalidReference(String),
    /// The HTTP client could not be constructed.
    #[error("http client: {0}")]
    Client(String),
    /// A network/transport error talking to AAD or Key Vault.
    #[error("request failed: {0}")]
    Transport(String),
    /// AAD token acquisition was rejected.
    #[error("token acquisition failed ({status}): {body}")]
    Token {
        /// HTTP status code returned by AAD.
        status: u16,
        /// Response body (may contain an AAD error code; never a secret).
        body: String,
    },
    /// Key Vault rejected the secret request.
    #[error("key vault returned {status} for secret '{secret}': {body}")]
    Secret {
        /// HTTP status code returned by Key Vault.
        status: u16,
        /// The secret name requested (not its value).
        secret: String,
        /// Response body (Key Vault error envelope; never a secret value).
        body: String,
    },
}

/// A parsed `azure-kv://<vault>/<secret>[/<version>]` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureSecretRef {
    /// Key Vault name (the `<vault>` in `https://<vault>.vault.azure.net`).
    pub vault: String,
    /// Secret name within the vault.
    pub secret: String,
    /// Optional explicit secret version (defaults to the latest enabled one).
    pub version: Option<String>,
}

impl AzureSecretRef {
    /// Scheme prefix that selects the Azure Key Vault backend.
    pub const SCHEME: &'static str = "azure-kv://";

    /// Parse an `azure-kv://<vault>/<secret>[/<version>]` reference.
    ///
    /// # Errors
    ///
    /// Returns [`AzureError::InvalidReference`] if the scheme is missing or the
    /// vault / secret components are absent or empty.
    pub fn parse(reference: &str) -> Result<Self, AzureError> {
        let rest = reference.strip_prefix(Self::SCHEME).ok_or_else(|| {
            AzureError::InvalidReference(format!("expected '{}' scheme", Self::SCHEME))
        })?;
        let mut parts = rest.splitn(3, '/');
        let vault = parts.next().unwrap_or("").trim();
        let secret = parts.next().unwrap_or("").trim();
        let version = parts.next().map(str::trim).filter(|s| !s.is_empty());
        if vault.is_empty() {
            return Err(AzureError::InvalidReference("vault name is empty".into()));
        }
        if secret.is_empty() {
            return Err(AzureError::InvalidReference("secret name is empty".into()));
        }
        Ok(Self {
            vault: vault.to_string(),
            secret: secret.to_string(),
            version: version.map(str::to_string),
        })
    }

    /// Whether `reference` uses the `azure-kv://` scheme.
    #[must_use]
    pub fn matches(reference: &str) -> bool {
        reference.starts_with(Self::SCHEME)
    }
}

/// How the client authenticates to AAD / Key Vault.
#[derive(Clone)]
enum Credential {
    /// A caller-supplied, already-valid bearer token (never refreshed).
    StaticToken(Zeroizing<String>),
    /// OAuth2 client-credentials (service principal).
    ClientSecret {
        tenant_id: String,
        client_id: String,
        client_secret: Zeroizing<String>,
    },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaticToken(_) => f.write_str("StaticToken(<redacted>)"),
            Self::ClientSecret {
                tenant_id,
                client_id,
                ..
            } => f
                .debug_struct("ClientSecret")
                .field("tenant_id", tenant_id)
                .field("client_id", client_id)
                .field("client_secret", &"<redacted>")
                .finish(),
        }
    }
}

impl Credential {
    /// Resolve a credential from explicit values (testable core of `from_env`).
    fn resolve(
        token: Option<String>,
        tenant_id: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Result<Self, AzureError> {
        if let Some(t) = token
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            return Ok(Self::StaticToken(Zeroizing::new(t)));
        }
        match (tenant_id, client_id, client_secret) {
            (Some(tenant), Some(client), Some(secret))
                if !tenant.trim().is_empty()
                    && !client.trim().is_empty()
                    && !secret.trim().is_empty() =>
            {
                Ok(Self::ClientSecret {
                    tenant_id: tenant.trim().to_string(),
                    client_id: client.trim().to_string(),
                    client_secret: Zeroizing::new(secret.trim().to_string()),
                })
            }
            _ => Err(AzureError::MissingCredentials(
                "set AZURE_KEYVAULT_TOKEN, or AZURE_TENANT_ID + AZURE_CLIENT_ID + \
                 AZURE_CLIENT_SECRET"
                    .into(),
            )),
        }
    }
}

#[derive(Clone)]
struct CachedToken {
    token: Zeroizing<String>,
    expires_at: Instant,
}

impl std::fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug)]
struct Inner {
    credential: Credential,
    dns_suffix: String,
    authority_host: String,
    http: reqwest::Client,
    cached: Mutex<Option<CachedToken>>,
}

/// A handle to an Azure Key Vault BYOK backend. Cheaply cloneable; clones share
/// the same cached AAD token.
#[derive(Debug, Clone)]
pub struct AzureKeyVault {
    inner: Arc<Inner>,
}

impl AzureKeyVault {
    /// Build a client from the process environment.
    ///
    /// See the [module docs](self) for the variables consulted.
    ///
    /// # Errors
    ///
    /// Returns [`AzureError::MissingCredentials`] if neither a static token nor
    /// a complete service-principal triple is present, or [`AzureError::Client`]
    /// if the HTTP client cannot be built.
    pub fn from_env() -> Result<Self, AzureError> {
        let credential = Credential::resolve(
            std::env::var("AZURE_KEYVAULT_TOKEN").ok(),
            std::env::var("AZURE_TENANT_ID").ok(),
            std::env::var("AZURE_CLIENT_ID").ok(),
            std::env::var("AZURE_CLIENT_SECRET").ok(),
        )?;
        let dns_suffix = env_or("AZURE_KEYVAULT_DNS_SUFFIX", DEFAULT_DNS_SUFFIX);
        let authority_host = env_or("AZURE_AUTHORITY_HOST", DEFAULT_AUTHORITY_HOST)
            .trim_end_matches('/')
            .to_string();
        let http = reqwest::Client::builder()
            .https_only(true)
            .build()
            .map_err(|e| AzureError::Client(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                credential,
                dns_suffix,
                authority_host,
                http,
                cached: Mutex::new(None),
            }),
        })
    }

    /// Fetch a secret's value, refreshing the AAD token if needed.
    ///
    /// # Errors
    ///
    /// Propagates [`AzureError`] for credential, transport, token, or Key Vault
    /// failures.
    pub async fn get_secret(
        &self,
        reference: &AzureSecretRef,
    ) -> Result<Zeroizing<String>, AzureError> {
        let token = self.access_token().await?;
        let url = secret_url(
            &reference.vault,
            &self.inner.dns_suffix,
            &reference.secret,
            reference.version.as_deref(),
        );
        let resp = self
            .inner
            .http
            .get(&url)
            .bearer_auth(token.as_str())
            .send()
            .await
            .map_err(|e| AzureError::Transport(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AzureError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(AzureError::Secret {
                status: status.as_u16(),
                secret: reference.secret.clone(),
                body: truncate(&body),
            });
        }
        let parsed: SecretBundle =
            serde_json::from_str(&body).map_err(|e| AzureError::Transport(e.to_string()))?;
        Ok(Zeroizing::new(parsed.value))
    }

    /// Convenience: parse an `azure-kv://` reference and fetch its secret.
    ///
    /// # Errors
    ///
    /// Propagates parse and fetch errors.
    pub async fn resolve_reference(
        &self,
        reference: &str,
    ) -> Result<Zeroizing<String>, AzureError> {
        let parsed = AzureSecretRef::parse(reference)?;
        self.get_secret(&parsed).await
    }

    async fn access_token(&self) -> Result<Zeroizing<String>, AzureError> {
        if let Credential::StaticToken(t) = &self.inner.credential {
            return Ok(t.clone());
        }
        let mut guard = self.inner.cached.lock().await;
        if let Some(cached) = guard.as_ref() {
            if Instant::now() < cached.expires_at {
                return Ok(cached.token.clone());
            }
        }
        let fresh = self.acquire_token().await?;
        let token = fresh.token.clone();
        *guard = Some(fresh);
        Ok(token)
    }

    async fn acquire_token(&self) -> Result<CachedToken, AzureError> {
        let Credential::ClientSecret {
            tenant_id,
            client_id,
            client_secret,
        } = &self.inner.credential
        else {
            // Static tokens never reach here (short-circuited in `access_token`).
            return Err(AzureError::MissingCredentials(
                "no refreshable credential".into(),
            ));
        };
        let url = token_url(&self.inner.authority_host, tenant_id);
        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("scope", KEYVAULT_SCOPE),
        ];
        let resp = self
            .inner
            .http
            .post(&url)
            .form(&form)
            .send()
            .await
            .map_err(|e| AzureError::Transport(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AzureError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(AzureError::Token {
                status: status.as_u16(),
                body: truncate(&body),
            });
        }
        let parsed: TokenResponse =
            serde_json::from_str(&body).map_err(|e| AzureError::Transport(e.to_string()))?;
        let ttl = Duration::from_secs(parsed.expires_in.max(60));
        let expires_at = Instant::now() + ttl.saturating_sub(TOKEN_SKEW);
        Ok(CachedToken {
            token: Zeroizing::new(parsed.access_token),
            expires_at,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SecretBundle {
    value: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_expiry")]
    expires_in: u64,
}

fn default_expiry() -> u64 {
    3600
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn secret_url(vault: &str, dns_suffix: &str, secret: &str, version: Option<&str>) -> String {
    let suffix = dns_suffix.trim_matches('/');
    match version {
        Some(v) => format!(
            "https://{vault}.{suffix}/secrets/{secret}/{v}?api-version={KEYVAULT_API_VERSION}"
        ),
        None => {
            format!("https://{vault}.{suffix}/secrets/{secret}?api-version={KEYVAULT_API_VERSION}")
        }
    }
}

fn token_url(authority_host: &str, tenant_id: &str) -> String {
    let host = authority_host.trim_end_matches('/');
    format!("{host}/{tenant_id}/oauth2/v2.0/token")
}

fn truncate(body: &str) -> String {
    const MAX: usize = 512;
    if body.len() <= MAX {
        body.to_string()
    } else {
        let mut end = MAX;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &body[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vault_and_secret() {
        let r = AzureSecretRef::parse("azure-kv://my-kv/byok-anthropic").unwrap();
        assert_eq!(r.vault, "my-kv");
        assert_eq!(r.secret, "byok-anthropic");
        assert_eq!(r.version, None);
    }

    #[test]
    fn parses_optional_version() {
        let r = AzureSecretRef::parse("azure-kv://my-kv/key/abc123").unwrap();
        assert_eq!(r.version.as_deref(), Some("abc123"));
    }

    #[test]
    fn rejects_wrong_scheme() {
        assert!(matches!(
            AzureSecretRef::parse("vault:byok"),
            Err(AzureError::InvalidReference(_))
        ));
    }

    #[test]
    fn rejects_missing_secret() {
        assert!(matches!(
            AzureSecretRef::parse("azure-kv://only-vault"),
            Err(AzureError::InvalidReference(_))
        ));
        assert!(matches!(
            AzureSecretRef::parse("azure-kv://vault/"),
            Err(AzureError::InvalidReference(_))
        ));
    }

    #[test]
    fn matches_scheme() {
        assert!(AzureSecretRef::matches("azure-kv://a/b"));
        assert!(!AzureSecretRef::matches("vault:a"));
    }

    #[test]
    fn static_token_takes_precedence() {
        let c = Credential::resolve(
            Some("tok".into()),
            Some("t".into()),
            Some("c".into()),
            Some("s".into()),
        )
        .unwrap();
        assert!(matches!(c, Credential::StaticToken(_)));
    }

    #[test]
    fn service_principal_resolves() {
        let c = Credential::resolve(
            None,
            Some("tenant".into()),
            Some("client".into()),
            Some("secret".into()),
        )
        .unwrap();
        match c {
            Credential::ClientSecret {
                tenant_id,
                client_id,
                ..
            } => {
                assert_eq!(tenant_id, "tenant");
                assert_eq!(client_id, "client");
            }
            Credential::StaticToken(_) => panic!("expected client-secret credential"),
        }
    }

    #[test]
    fn incomplete_credentials_error() {
        assert!(matches!(
            Credential::resolve(None, Some("t".into()), None, Some("s".into())),
            Err(AzureError::MissingCredentials(_))
        ));
        assert!(matches!(
            Credential::resolve(Some("  ".into()), None, None, None),
            Err(AzureError::MissingCredentials(_))
        ));
    }

    #[test]
    fn builds_secret_url() {
        assert_eq!(
            secret_url("kv", "vault.azure.net", "byok", None),
            "https://kv.vault.azure.net/secrets/byok?api-version=7.4"
        );
        assert_eq!(
            secret_url("kv", "vault.azure.net", "byok", Some("v2")),
            "https://kv.vault.azure.net/secrets/byok/v2?api-version=7.4"
        );
    }

    #[test]
    fn builds_token_url() {
        assert_eq!(
            token_url("https://login.microsoftonline.com", "tenant-123"),
            "https://login.microsoftonline.com/tenant-123/oauth2/v2.0/token"
        );
        // trailing slash on authority host is normalized
        assert_eq!(
            token_url("https://login.microsoftonline.com/", "t"),
            "https://login.microsoftonline.com/t/oauth2/v2.0/token"
        );
    }

    #[test]
    fn credential_debug_redacts_secrets() {
        let c = Credential::resolve(
            None,
            Some("tenant".into()),
            Some("client".into()),
            Some("super-secret".into()),
        )
        .unwrap();
        let dbg = format!("{c:?}");
        assert!(dbg.contains("tenant"));
        assert!(!dbg.contains("super-secret"));
        assert!(dbg.contains("redacted"));
    }
}
