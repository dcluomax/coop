//! Brain factory: builds a `BrainAdapter` for a given Hen manifest.
//!
//! Resolves `manifest.brain.provider_id` to an Anthropic API key. Two schemes
//! are supported:
//!
//! * `vault:<secret-name>` — read from the unlocked local sealed vault.
//! * `azure-kv://<vault>/<secret>[/<version>]` — fetch from Azure Key Vault
//!   (credentials from the environment; see [`coopd_vault::azure`]).

use std::sync::Arc;

use coopd_brain::{Anthropic, CachingBrain, RoutingBrain, router::models};
use coopd_core::{AgentManifest, BrainAdapter, CoreError, Result};
use coopd_vault::{AzureKeyVault, AzureSecretRef, Vault};
use once_cell::sync::OnceCell;

/// Builds brain adapters by resolving manifests against the vault.
#[derive(Debug)]
pub struct BrainFactory {
    vault: Option<Vault>,
    /// Lazily-initialised Azure Key Vault client (built from env on first use).
    azure: OnceCell<AzureKeyVault>,
}

impl BrainFactory {
    /// Construct with an optional unlocked vault.
    #[must_use]
    pub fn new(vault: Option<Vault>) -> Self {
        Self {
            vault,
            azure: OnceCell::new(),
        }
    }

    /// Replace the vault (used by `/vault/unlock` API).
    pub fn set_vault(&mut self, vault: Vault) {
        self.vault = Some(vault);
    }

    /// Whether a vault has been unlocked.
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        self.vault.is_some()
    }

    /// Write a secret into the unlocked vault. Errors if locked.
    pub fn vault_put(&mut self, name: &str, value: &str) -> Result<()> {
        let v = self
            .vault
            .as_mut()
            .ok_or_else(|| CoreError::Other("vault is locked; POST /api/v1/vault/unlock".into()))?;
        v.put(name, value)
            .map_err(|e| CoreError::Other(format!("vault put: {e}")))?;
        Ok(())
    }

    /// List secret names (no values) from the unlocked vault.
    #[must_use]
    pub fn vault_list(&self) -> Vec<String> {
        match &self.vault {
            Some(v) => v.list().into_iter().map(str::to_string).collect(),
            None => vec![],
        }
    }

    /// Build a brain adapter from a manifest.
    pub async fn build(&self, manifest: &AgentManifest) -> Result<Arc<dyn BrainAdapter>> {
        let provider = &manifest.brain.provider_id;
        let model = manifest.brain.model.clone();

        let api_key = self.resolve_key(provider).await?;

        // Layer:  optional_cache( optional_routing( base ) )
        let base: Arc<dyn BrainAdapter> = if manifest.brain.auto_route {
            let haiku: Arc<dyn BrainAdapter> =
                Arc::new(Anthropic::new(api_key.clone(), models::HAIKU.to_string()));
            let sonnet: Arc<dyn BrainAdapter> =
                Arc::new(Anthropic::new(api_key.clone(), models::SONNET.to_string()));
            let opus: Arc<dyn BrainAdapter> =
                Arc::new(Anthropic::new(api_key, models::OPUS.to_string()));
            Arc::new(RoutingBrain::new(haiku, sonnet, opus))
        } else {
            Arc::new(Anthropic::new(api_key, model.clone()))
        };

        if manifest.brain.cache {
            // Box wrapper: CachingBrain<dyn> isn't object-safe directly, but it
            // wraps the concrete Arc<dyn BrainAdapter> via a thin newtype.
            let key_hint = if manifest.brain.auto_route {
                "auto"
            } else {
                model.as_str()
            };
            Ok(Arc::new(CachingBrain::new(DynBrain(base), key_hint)))
        } else {
            Ok(base)
        }
    }

    /// Resolve a `provider_id` to a plaintext API key from the appropriate
    /// secret backend.
    async fn resolve_key(&self, provider: &str) -> Result<String> {
        if AzureSecretRef::matches(provider) {
            let kv = self
                .azure
                .get_or_try_init(AzureKeyVault::from_env)
                .map_err(|e| CoreError::Other(format!("azure key vault: {e}")))?;
            let secret = kv
                .resolve_reference(provider)
                .await
                .map_err(|e| CoreError::Other(format!("azure key vault: {e}")))?;
            return Ok(secret.to_string());
        }

        let secret_name = provider
            .strip_prefix("vault:")
            .ok_or_else(|| CoreError::Other(format!("unsupported provider_id: {provider}")))?;

        let vault = self
            .vault
            .as_ref()
            .ok_or_else(|| CoreError::Other("vault is locked; POST /api/v1/vault/unlock".into()))?;

        Ok(vault
            .get(secret_name)
            .map_err(|e| CoreError::Other(format!("vault: {e}")))?
            .to_string())
    }
}

/// Newtype to satisfy `CachingBrain<B: BrainAdapter + 'static>` bounds for
/// trait objects (we don't have `dyn BrainAdapter: Sized`).
struct DynBrain(Arc<dyn BrainAdapter>);

#[async_trait::async_trait]
impl BrainAdapter for DynBrain {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn tier(&self) -> coopd_core::brain::Tier {
        self.0.tier()
    }
    fn capabilities(&self) -> coopd_core::brain::BrainCaps {
        self.0.capabilities()
    }
    async fn reason(&self, req: coopd_core::ReasonRequest) -> Result<coopd_core::ReasonResponse> {
        self.0.reason(req).await
    }
    async fn stream(
        &self,
        req: coopd_core::ReasonRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<coopd_core::brain::ReasonChunk>>> {
        self.0.stream(req).await
    }
    fn estimate_cost(&self, req: &coopd_core::ReasonRequest) -> coopd_core::brain::CostEstimate {
        self.0.estimate_cost(req)
    }
    async fn health_check(&self) -> Result<()> {
        self.0.health_check().await
    }
}
