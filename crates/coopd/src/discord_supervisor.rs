//! Runtime supervisor for the optional Discord connector.
//!
//! Owns:
//! - The bot's [`JoinHandle`], so we can abort it on reconfiguration.
//! - On-disk persistence of the connector config at
//!   `<data_dir>/discord.json` (mode 0600).
//!
//! Lifecycle:
//! - On daemon boot: load persisted config (or env vars as fallback); if
//!   `enabled && token && guild_id`, spawn the bot.
//! - On `PUT /api/v1/config/discord`: persist, abort the running task (if
//!   any), respawn under the new config if `enabled`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// User-visible Discord connector config (no secrets returned to clients
/// other than the token itself, which the client must already possess to
/// have set it).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscordConfig {
    /// Whether the connector should run.
    #[serde(default)]
    pub enabled: bool,
    /// Bot token (sensitive). Stored on disk in `discord.json`.
    #[serde(default)]
    pub token: String,
    /// Numeric Discord guild (server) snowflake.
    #[serde(default)]
    pub guild_id: Option<u64>,
    /// Command prefix; defaults to `!coop`.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// HTTP base the bot uses to talk back to coopd. Defaults to
    /// `http://127.0.0.1:9700` on the daemon side.
    #[serde(default)]
    pub api_base: Option<String>,
    /// Discord user IDs (snowflakes) allowed to issue commands. Empty
    /// list = bot silently ignores every message (M6 default-deny).
    #[serde(default)]
    pub allowed_user_ids: Vec<u64>,
}

fn default_prefix() -> String {
    "!coop".to_string()
}

impl DiscordConfig {
    /// Redact the token before sending to clients.
    #[must_use]
    pub fn redacted(&self) -> Self {
        Self {
            enabled: self.enabled,
            token: if self.token.is_empty() {
                String::new()
            } else {
                "***".to_string()
            },
            guild_id: self.guild_id,
            prefix: self.prefix.clone(),
            api_base: self.api_base.clone(),
            allowed_user_ids: self.allowed_user_ids.clone(),
        }
    }

    /// Did the client send a literal `***` token? If so we preserve the
    /// previously-stored value (treat as "unchanged").
    #[must_use]
    pub fn token_is_placeholder(&self) -> bool {
        self.token == "***"
    }
}

/// Supervisor handle shared between the API layer and the daemon main.
#[derive(Clone)]
pub struct DiscordSupervisor {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    cfg_path: PathBuf,
    current_cfg: DiscordConfig,
    handle: Option<JoinHandle<()>>,
    default_api_base: String,
    coop_id: String,
}

impl DiscordSupervisor {
    /// Create a new supervisor. Does NOT load from disk or spawn yet.
    #[must_use]
    pub fn new(data_dir: &Path, coop_id: &str, default_api_base: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                cfg_path: data_dir.join("discord.json"),
                current_cfg: DiscordConfig::default(),
                handle: None,
                default_api_base,
                coop_id: coop_id.to_string(),
            })),
        }
    }

    /// Bootstrap from on-disk config and env-var fallback. Spawns the bot
    /// if `enabled` and `token` + `guild_id` are present.
    pub async fn bootstrap(&self) {
        let mut g = self.inner.lock().await;
        let cfg = load_or_env(&g.cfg_path);
        g.current_cfg = cfg.clone();
        if cfg.enabled {
            spawn_locked(&mut g).await;
        } else {
            info!(
                "discord connector disabled (configure via PUT /api/v1/config/discord or env vars)"
            );
        }
    }

    /// Return the redacted current config (safe for API clients).
    pub async fn snapshot(&self) -> DiscordConfig {
        self.inner.lock().await.current_cfg.redacted()
    }

    /// Apply a new config: persist, abort running task, respawn if enabled.
    /// If `incoming.token` is the placeholder `***`, keep the previous one.
    pub async fn apply(&self, mut incoming: DiscordConfig) -> Result<DiscordConfig> {
        let mut g = self.inner.lock().await;
        if incoming.token_is_placeholder() {
            incoming.token = g.current_cfg.token.clone();
        }
        if incoming.prefix.trim().is_empty() {
            incoming.prefix = default_prefix();
        }
        // Persist to disk.
        persist(&g.cfg_path, &incoming).context("persisting discord.json")?;
        g.current_cfg = incoming.clone();

        // Abort any running task (best-effort).
        if let Some(h) = g.handle.take() {
            h.abort();
            info!("discord connector stopped (reconfiguring)");
        }
        if incoming.enabled {
            spawn_locked(&mut g).await;
        }
        Ok(incoming.redacted())
    }
}

async fn spawn_locked(g: &mut Inner) {
    let cfg = g.current_cfg.clone();
    let Some(guild_id) = cfg.guild_id else {
        warn!("discord enabled but guild_id missing");
        return;
    };
    if cfg.token.is_empty() {
        warn!("discord enabled but token empty");
        return;
    }
    let bot_cfg = coopd_discord::DiscordConfig {
        token: cfg.token,
        guild_id,
        prefix: cfg.prefix,
        api_base: cfg.api_base.unwrap_or_else(|| g.default_api_base.clone()),
        coop_id: g.coop_id.clone(),
        allowed_user_ids: cfg.allowed_user_ids,
    };
    match coopd_discord::spawn(bot_cfg).await {
        Ok(h) => {
            g.handle = Some(h);
            info!("discord connector running");
        }
        Err(e) => warn!(error = %e, "discord connector failed to start"),
    }
}

fn load_or_env(path: &Path) -> DiscordConfig {
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok(cfg) = serde_json::from_slice::<DiscordConfig>(&bytes) {
            return cfg;
        }
        warn!(path = %path.display(), "discord.json present but unparseable; ignoring");
    }
    // Env fallback (legacy / one-shot deploys).
    let token = std::env::var("COOP_DISCORD_TOKEN").unwrap_or_default();
    let guild_id = std::env::var("COOP_DISCORD_GUILD_ID")
        .ok()
        .and_then(|s| s.parse().ok());
    let prefix = std::env::var("COOP_DISCORD_PREFIX").unwrap_or_else(|_| default_prefix());
    let enabled = !token.is_empty() && guild_id.is_some();
    let allowed_user_ids = std::env::var("COOP_DISCORD_ALLOWED_USERS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse::<u64>().ok())
                .collect()
        })
        .unwrap_or_default();
    DiscordConfig {
        enabled,
        token,
        guild_id,
        prefix,
        api_base: std::env::var("COOP_API_BASE").ok(),
        allowed_user_ids,
    }
}

fn persist(path: &Path, cfg: &DiscordConfig) -> Result<()> {
    let json = serde_json::to_vec_pretty(cfg)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(path)?.permissions();
        p.set_mode(0o600);
        std::fs::set_permissions(path, p)?;
    }
    Ok(())
}
