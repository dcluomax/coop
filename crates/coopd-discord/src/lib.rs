//! # `coopd-discord` — optional Discord connector for Coop
//!
//! Bridges a Discord guild to the Coop daemon's HTTP API.
//!
//! Inspired by `copilot-bridge`'s channel-per-session pattern: each Discord
//! text channel maps 1:1 to a chicken (channel name = chicken's local name).
//!
//! ## Activation
//!
//! Enabled when both env vars are present at daemon startup:
//!
//! - `COOP_DISCORD_TOKEN` — your bot's token
//! - `COOP_DISCORD_GUILD_ID` — the numeric Discord guild (server) snowflake
//!
//! Optional:
//!
//! - `COOP_DISCORD_PREFIX` (default `!coop`) — message prefix the bot listens for
//! - `COOP_API_BASE`       (default `http://127.0.0.1:9700`) — coopd HTTP base
//!
//! ## Usage in Discord
//!
//! ```text
//! User → #aria :  !coop hello, list files in your workdir
//! Bot  → #aria :  🐔 aria · job 019e3a72 submitted (state=QUEUED)
//! Bot  → #aria :  ✅ done in 4.2s  (XP +12, 🪙 +3)
//! ```
//!
//! Commands the bot understands inside a chicken's channel:
//!
//! | message                | action                              |
//! |------------------------|-------------------------------------|
//! | `!coop <prompt>`       | submit a job to the chicken         |
//! | `!coop status`         | show the chicken's current state    |
//! | `!coop hatch`          | hatch a DEFINED chicken             |
//! | `!coop sleep` / `wake` | put chicken to sleep / wake it      |
//! | `!coop help`           | command list                        |

use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serenity::all::{Context as Ctx, EventHandler, GatewayIntents, GuildId, Message, Ready};
use serenity::{Client, async_trait};
use tracing::{error, info, warn};

/// Runtime config for the Discord connector.
#[derive(Debug, Clone)]
pub struct DiscordConfig {
    /// Bot token (from <https://discord.com/developers/applications>).
    pub token: String,
    /// Snowflake of the guild the bot operates in.
    pub guild_id: u64,
    /// Command prefix (default `!coop`).
    pub prefix: String,
    /// Coopd HTTP base URL (default `http://127.0.0.1:9700`).
    pub api_base: String,
    /// Coop ID (e.g. `local.coop`) used to build full hen IDs from channel names.
    pub coop_id: String,
    /// Discord user IDs allowed to issue commands. If empty the bot refuses
    /// every prompt (failsafe: any guild member could otherwise execute
    /// arbitrary jobs against the farm). Set via `COOP_DISCORD_ALLOWED_USERS`.
    pub allowed_user_ids: Vec<u64>,
}

impl DiscordConfig {
    /// Build a config from env vars. Returns `None` if Discord is not configured.
    #[must_use]
    pub fn from_env(coop_id: &str) -> Option<Self> {
        let token = std::env::var("COOP_DISCORD_TOKEN").ok()?;
        let guild_id: u64 = std::env::var("COOP_DISCORD_GUILD_ID").ok()?.parse().ok()?;
        let prefix = std::env::var("COOP_DISCORD_PREFIX").unwrap_or_else(|_| "!coop".to_string());
        let api_base =
            std::env::var("COOP_API_BASE").unwrap_or_else(|_| "http://127.0.0.1:9700".to_string());
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
        Some(Self {
            token,
            guild_id,
            prefix,
            api_base,
            coop_id: coop_id.to_string(),
            allowed_user_ids,
        })
    }
}

/// Spawn the Discord bot in the background. Returns immediately.
///
/// The task logs and reconnects on transient failures; it is intended to run
/// for the lifetime of the daemon.
///
/// # Errors
///
/// Returns `Err` only if the initial Serenity client construction fails (e.g.
/// the token is malformed). Network and gateway errors are handled internally
/// by Serenity's reconnection logic.
pub async fn spawn(cfg: DiscordConfig) -> Result<tokio::task::JoinHandle<()>> {
    let http = reqwest::Client::builder()
        .user_agent("coopd-discord/0.1")
        .build()
        .context("build reqwest client")?;
    let handler = Handler {
        cfg: cfg.clone(),
        http: Arc::new(http),
    };
    // GUILD_MESSAGES + MESSAGE_CONTENT are the minimum to read channel chatter.
    let intents = GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    let mut client = Client::builder(&cfg.token, intents)
        .event_handler(handler)
        .await
        .context("build serenity client")?;

    info!(
        guild_id = cfg.guild_id,
        api_base = %cfg.api_base,
        prefix = %cfg.prefix,
        "discord connector starting"
    );

    Ok(tokio::spawn(async move {
        if let Err(e) = client.start().await {
            error!("discord client exited: {e:#}");
        }
    }))
}

struct Handler {
    cfg: DiscordConfig,
    http: Arc<reqwest::Client>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Ctx, ready: Ready) {
        info!(
            bot = %ready.user.name,
            session_id = %ready.session_id,
            "discord bot ready"
        );
    }

    async fn message(&self, ctx: Ctx, msg: Message) {
        if msg.author.bot {
            return;
        }
        // Guild-scope filter.
        if msg.guild_id != Some(GuildId::new(self.cfg.guild_id)) {
            return;
        }
        // M6: default-deny. Empty allowlist = bot is dormant — any guild
        // member could otherwise dispatch arbitrary jobs/prompts.
        if self.cfg.allowed_user_ids.is_empty() {
            warn!(
                "discord: ignoring message — COOP_DISCORD_ALLOWED_USERS is empty (set to comma-separated user IDs to enable)"
            );
            return;
        }
        if !self.cfg.allowed_user_ids.contains(&msg.author.id.get()) {
            warn!(
                user_id = msg.author.id.get(),
                user_name = %msg.author.name,
                "discord: refusing message from non-allowlisted user"
            );
            return;
        }
        let content = msg.content.trim();
        let Some(rest) = content.strip_prefix(&self.cfg.prefix) else {
            return;
        };
        let arg = rest.trim();

        // Channel name → chicken local name → full id "<coop_id>/<name>".
        let channel_name = match msg.channel_id.to_channel(&ctx.http).await {
            Ok(ch) => ch.guild().map(|g| g.name).unwrap_or_default(),
            Err(_) => String::new(),
        };
        if channel_name.is_empty() {
            return;
        }
        let chicken_id = format!("{}/{}", self.cfg.coop_id, channel_name);

        let reply = match self.dispatch(&chicken_id, arg).await {
            Ok(s) => s,
            Err(e) => format!("⚠️  {e:#}"),
        };
        if let Err(e) = msg.channel_id.say(&ctx.http, reply).await {
            warn!("failed to send discord reply: {e}");
        }
    }
}

impl Handler {
    async fn dispatch(&self, chicken_id: &str, arg: &str) -> Result<String> {
        match arg {
            "" | "help" => Ok(help_text()),
            "status" => self.status(chicken_id).await,
            "hatch" => self.simple_post(chicken_id, "hatch", "🐣 hatching…").await,
            "sleep" => self.simple_post(chicken_id, "sleep", "💤 sleeping").await,
            "wake" => self.simple_post(chicken_id, "wake", "🌅 awake").await,
            prompt => self.submit_job(chicken_id, prompt).await,
        }
    }

    async fn status(&self, chicken_id: &str) -> Result<String> {
        let url = format!(
            "{}/api/v1/hens/{}",
            self.cfg.api_base,
            urlencoding::encode(chicken_id)
        );
        let hen: HenView = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let sex = hen.manifest.sex.as_deref().unwrap_or("hen");
        let face = if sex == "rooster" { "🐓" } else { "🐔" };
        Ok(format!(
            "{face} `{}` — state **{}** · brain `{}` · tools: {}",
            hen.id,
            hen.state,
            hen.manifest.brain.model,
            hen.manifest.tools.join(", ")
        ))
    }

    async fn simple_post(&self, chicken_id: &str, verb: &str, ok_msg: &str) -> Result<String> {
        let url = format!(
            "{}/api/v1/hens/{}/{}",
            self.cfg.api_base,
            urlencoding::encode(chicken_id),
            verb
        );
        let r = self.http.post(&url).send().await?;
        if r.status().is_success() {
            Ok(ok_msg.to_string())
        } else {
            Ok(format!("⚠️  {} failed: {}", verb, r.status()))
        }
    }

    async fn submit_job(&self, chicken_id: &str, prompt: &str) -> Result<String> {
        let url = format!(
            "{}/api/v1/hens/{}/jobs",
            self.cfg.api_base,
            urlencoding::encode(chicken_id)
        );
        let body = serde_json::json!({ "prompt": prompt });
        let r = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let v: serde_json::Value = r.json().await?;
        let job_id = v
            .get("id")
            .or_else(|| v.get("job_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        Ok(format!(
            "🐔 `{chicken_id}` · job `{job_id}` submitted (state=QUEUED)"
        ))
    }
}

fn help_text() -> String {
    [
        "**Coop bot commands** (inside a chicken's channel):",
        "`!coop <prompt>` — submit a job",
        "`!coop status`   — show chicken state",
        "`!coop hatch`    — hatch a defined chicken",
        "`!coop sleep`    — put chicken to sleep",
        "`!coop wake`     — wake a sleeping chicken",
        "`!coop help`     — this message",
    ]
    .join("\n")
}

// Minimal mirror of coopd's HEN view — we don't depend on coopd directly to
// avoid a circular crate dependency.
#[derive(Debug, Deserialize, Serialize)]
struct HenView {
    id: String,
    state: String,
    manifest: ManifestView,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestView {
    #[serde(default)]
    sex: Option<String>,
    brain: BrainView,
    #[serde(default)]
    tools: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BrainView {
    model: String,
    provider_id: String,
}

// Lightweight url-encoder (avoid pulling another crate for one function).
mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char);
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::urlencoding::encode;

    #[test]
    fn url_encodes_slash() {
        assert_eq!(encode("local.coop/aria"), "local.coop%2Faria");
    }

    #[test]
    fn url_encodes_spaces_and_unicode() {
        assert_eq!(encode("hi world"), "hi%20world");
        assert!(encode("🐔").starts_with('%'));
    }

    #[test]
    fn help_lists_all_commands() {
        let h = super::help_text();
        for cmd in ["status", "hatch", "sleep", "wake", "help"] {
            assert!(h.contains(cmd), "help text missing {cmd}");
        }
    }
}
