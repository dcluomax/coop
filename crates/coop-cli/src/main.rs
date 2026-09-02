//! # coop CLI
//!
//! Command-line client for `coopd`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use coopd_core::{AgentManifest, NetPolicy, NetworkSpec, manifest::MemorySpec};
use serde_json::Value;
use tracing_subscriber::EnvFilter;

/// Percent-encode a Hen ID for use as a single path segment.
///
/// Hen IDs are `coop_id/name` (e.g. `local.coop/aria`); the `/` separator
/// must be escaped to `%2F` so the server's `:id` path parameter captures
/// the whole thing instead of treating it as two segments and returning 404.
fn enc(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 4);
    for b in id.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Attach `Authorization: Bearer <token>` to a request when a token is set.
///
/// An empty token (the default) leaves the request unauthenticated, matching a
/// coopd started without `COOP_API_TOKEN`.
fn auth(rb: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    if token.is_empty() {
        rb
    } else {
        rb.bearer_auth(token)
    }
}

#[derive(Parser, Debug)]
#[command(name = "coop", version, about = "Coop CLI")]
struct Cli {
    /// coopd API base URL.
    #[arg(long, env = "COOP_API", default_value = "http://127.0.0.1:9700")]
    api: String,

    /// Bearer token for an auth-enabled coopd (matches the daemon's
    /// `COOP_API_TOKEN`). Empty means send no `Authorization` header.
    #[arg(
        long,
        env = "COOP_API_TOKEN",
        default_value = "",
        hide_env_values = true
    )]
    token: String,

    /// Logging filter.
    #[arg(long, env = "COOP_LOG", default_value = "warn")]
    log: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Show farm summary.
    Farm,
    /// Health probe.
    Health,
    /// Hen operations.
    Hen {
        #[command(subcommand)]
        cmd: HenCmd,
    },
    /// Job operations.
    Job {
        #[command(subcommand)]
        cmd: JobCmd,
    },
    /// Vault operations.
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
}

#[derive(Subcommand, Debug)]
enum HenCmd {
    /// List hens.
    List {
        /// Filter by state (DEFINED|IDLE|WORKING|...).
        #[arg(long)]
        state: Option<String>,
    },
    /// Show a single hen.
    Get {
        /// Hen ID, e.g. `alice.coop/aria`.
        id: String,
    },
    /// Create a hen from an agent.yaml file.
    Create {
        /// Path to manifest YAML.
        file: PathBuf,
    },
    /// Create and hatch a ready-to-work starter Hen without writing YAML.
    Starter {
        /// Local Hen name (the id becomes `local.coop/<name>`).
        #[arg(default_value = "aria")]
        name: String,
        /// Vault/Azure secret reference containing the Anthropic API key.
        #[arg(long, default_value = "vault:byok-anthropic")]
        provider_id: String,
        /// Anthropic model available to your account.
        #[arg(long, default_value = "claude-sonnet-4-5-20250929")]
        model: String,
    },
    /// Hatch (boot) a hen.
    Hatch {
        /// Hen ID.
        id: String,
    },
    /// Put a hen to sleep.
    Sleep {
        /// Hen ID.
        id: String,
    },
    /// Wake a sleeping hen.
    Wake {
        /// Hen ID.
        id: String,
    },
    /// Delete a hen permanently.
    Delete {
        /// Hen ID.
        id: String,
    },
    /// Show a hen's recent episodic memories (oldest first).
    Memory {
        /// Hen ID.
        id: String,
        /// Max number of most-recent episodes to show.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Forget (delete) all of a hen's episodic memories.
    Forget {
        /// Hen ID.
        id: String,
    },
    /// Delegate a subtask from one hen to another and print the result.
    Delegate {
        /// Delegating ("manager") hen ID, e.g. `local.coop/aria`.
        from: String,
        /// Target hen ID that performs the subtask, e.g. `local.coop/scout`.
        to: String,
        /// The subtask prompt.
        prompt: String,
    },
}

#[derive(Subcommand, Debug)]
enum JobCmd {
    /// Submit a new job to a hen and print the job ID.
    Run {
        /// Hen ID, e.g. `local.coop/aria`.
        hen_id: String,
        /// Prompt.
        prompt: String,
    },
    /// Get a job by ID.
    Get {
        /// Job ID.
        id: String,
    },
    /// List jobs.
    List {
        /// Optional hen filter.
        #[arg(long)]
        hen_id: Option<String>,
    },
    /// Poll a job until it reaches a terminal state.
    Wait {
        /// Job ID.
        id: String,
        /// Poll interval seconds.
        #[arg(long, default_value_t = 2)]
        interval_s: u64,
        /// Max wait in seconds.
        #[arg(long, default_value_t = 600)]
        timeout_s: u64,
    },
}

#[derive(Subcommand, Debug)]
enum VaultCmd {
    /// Initialize a fresh vault at `path` using `COOP_PASSPHRASE`.
    Init {
        /// Path to vault file.
        path: PathBuf,
    },
    /// Store a secret (reads value from `COOP_SECRET_VALUE`).
    Put {
        /// Path to vault file.
        path: PathBuf,
        /// Key name.
        name: String,
    },
    /// List secret names.
    List {
        /// Path to vault file.
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&cli.log).unwrap_or_else(|_| EnvFilter::new("warn")))
        .with_target(false)
        .compact()
        .init();

    match cli.cmd {
        Cmd::Health => {
            let client = reqwest::Client::new();
            let v: Value = auth(
                client.get(format!("{}/api/v1/healthz", cli.api)),
                &cli.token,
            )
            .send()
            .await?
            .json()
            .await?;
            println!("{v}");
        }
        Cmd::Farm => {
            let client = reqwest::Client::new();
            let v: Value = auth(client.get(format!("{}/api/v1/farm", cli.api)), &cli.token)
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Hen { cmd } => hen_cmd(&cli.api, &cli.token, cmd).await?,
        Cmd::Job { cmd } => job_cmd(&cli.api, &cli.token, cmd).await?,
        Cmd::Vault { cmd } => vault_cmd(cmd).await?,
    }
    Ok(())
}

async fn hen_cmd(api: &str, token: &str, cmd: HenCmd) -> Result<()> {
    let client = reqwest::Client::new();
    match cmd {
        HenCmd::List { state } => {
            let url = if let Some(s) = state {
                format!("{api}/api/v1/hens?state={s}")
            } else {
                format!("{api}/api/v1/hens")
            };
            let v: Value = auth(client.get(&url), token).send().await?.json().await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        HenCmd::Get { id } => {
            let v: Value = auth(client.get(format!("{api}/api/v1/hens/{}", enc(&id))), token)
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        HenCmd::Create { file } => {
            let yaml = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let id = create_hen(&client, api, token, yaml).await?;
            println!("{}", serde_json::to_string_pretty(&Value::String(id))?);
        }
        HenCmd::Starter {
            name,
            provider_id,
            model,
        } => {
            let manifest = starter_manifest(name, provider_id, model);
            manifest
                .validate()
                .context("generated starter manifest is invalid")?;
            let yaml = serde_yaml::to_string(&manifest)?;
            ensure_provider_ready(&client, api, token, &yaml).await?;
            let id = create_hen(&client, api, token, yaml).await?;
            hen_action(&client, api, token, &id, "hatch")
                .await
                .with_context(|| format!("Hen `{id}` was created but could not hatch"))?;
            println!("🐣 Hatched starter Hen `{id}`");
            println!("   memory: 30 days · tools: bash, file_read, file_write");
            println!("Next:");
            println!("   coop job run {id} \"Create hello.txt with one useful idea\"");
        }
        HenCmd::Hatch { id } => simple_post(&client, api, token, &id, "hatch").await?,
        HenCmd::Sleep { id } => simple_post(&client, api, token, &id, "sleep").await?,
        HenCmd::Wake { id } => simple_post(&client, api, token, &id, "wake").await?,
        HenCmd::Delete { id } => {
            let resp = auth(
                client.delete(format!("{api}/api/v1/hens/{}", enc(&id))),
                token,
            )
            .send()
            .await?;
            println!("status: {}", resp.status());
        }
        HenCmd::Memory { id, limit } => {
            let url = match limit {
                Some(n) => format!("{api}/api/v1/hens/{}/memory?limit={n}", enc(&id)),
                None => format!("{api}/api/v1/hens/{}/memory", enc(&id)),
            };
            let v: Value = auth(client.get(&url), token).send().await?.json().await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        HenCmd::Forget { id } => {
            let resp = auth(
                client.delete(format!("{api}/api/v1/hens/{}/memory", enc(&id))),
                token,
            )
            .send()
            .await?;
            let status = resp.status();
            let body: Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
            if !status.is_success() {
                bail!("forget failed ({status}): {body}");
            }
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        HenCmd::Delegate { from, to, prompt } => {
            let resp = auth(
                client.post(format!("{api}/api/v1/hens/{}/delegate", enc(&from))),
                token,
            )
            .json(&serde_json::json!({ "to": to, "prompt": prompt }))
            .send()
            .await?;
            let status = resp.status();
            let body: Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
            if !status.is_success() {
                bail!("delegate failed ({status}): {body}");
            }
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
    }
    Ok(())
}

fn starter_manifest(name: String, provider_id: String, model: String) -> AgentManifest {
    let mut manifest = AgentManifest::minimal(name);
    manifest.brain.provider_id = provider_id;
    manifest.brain.model = model;
    manifest.memory = Some(MemorySpec {
        episodic_retention_days: Some(30),
        semantic_summarize_every: None,
        inherit_from: None,
    });
    // Explicit `open` keeps the first-hatch path portable on hosts without an
    // OS network sandbox. The starter has no HTTP tool; users can tighten the
    // policy when they add networked tools.
    manifest.network = Some(NetworkSpec {
        policy: NetPolicy::Open,
        allow: vec![],
    });
    manifest
}

async fn ensure_provider_ready(
    client: &reqwest::Client,
    api: &str,
    token: &str,
    manifest_yaml: &str,
) -> Result<()> {
    let resp = auth(client.post(format!("{api}/api/v1/hens/preflight")), token)
        .header("content-type", "application/yaml")
        .body(manifest_yaml.to_string())
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        let message = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("provider is not ready");
        bail!("{message}");
    }
    Ok(())
}

async fn create_hen(
    client: &reqwest::Client,
    api: &str,
    token: &str,
    yaml: String,
) -> Result<String> {
    let resp = auth(client.post(format!("{api}/api/v1/hens")), token)
        .header("content-type", "application/yaml")
        .body(yaml)
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        bail!("create failed ({status}): {body}");
    }
    body.as_str()
        .map(str::to_string)
        .context("create response did not contain a Hen id")
}

async fn hen_action(
    client: &reqwest::Client,
    api: &str,
    token: &str,
    id: &str,
    action: &str,
) -> Result<Value> {
    let resp = auth(
        client.post(format!("{api}/api/v1/hens/{}/{action}", enc(id))),
        token,
    )
    .send()
    .await?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    if !status.is_success() {
        bail!("{action} failed ({status}): {body}");
    }
    Ok(body)
}

async fn simple_post(
    client: &reqwest::Client,
    api: &str,
    token: &str,
    id: &str,
    action: &str,
) -> Result<()> {
    let body = hen_action(client, api, token, id, action).await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

async fn job_cmd(api: &str, token: &str, cmd: JobCmd) -> Result<()> {
    let client = reqwest::Client::new();
    match cmd {
        JobCmd::Run { hen_id, prompt } => {
            let resp = auth(
                client.post(format!("{api}/api/v1/hens/{}/jobs", enc(&hen_id))),
                token,
            )
            .json(&serde_json::json!({ "prompt": prompt }))
            .send()
            .await?;
            let status = resp.status();
            let body: Value = resp.json().await?;
            if !status.is_success() {
                bail!("job run failed ({status}): {body}");
            }
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        JobCmd::Get { id } => {
            let v: Value = auth(client.get(format!("{api}/api/v1/jobs/{id}")), token)
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        JobCmd::List { hen_id } => {
            let url = if let Some(h) = hen_id {
                format!("{api}/api/v1/jobs?hen_id={}", enc(&h))
            } else {
                format!("{api}/api/v1/jobs")
            };
            let v: Value = auth(client.get(&url), token).send().await?.json().await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        JobCmd::Wait {
            id,
            interval_s,
            timeout_s,
        } => {
            let start = std::time::Instant::now();
            loop {
                let v: Value = auth(client.get(format!("{api}/api/v1/jobs/{id}")), token)
                    .send()
                    .await?
                    .json()
                    .await?;
                let status = v.get("status").and_then(Value::as_str).unwrap_or("");
                if matches!(status, "DONE" | "FAILED" | "CANCELLED") {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                    return Ok(());
                }
                if start.elapsed().as_secs() > timeout_s {
                    bail!("timeout waiting for job {id}");
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval_s)).await;
            }
        }
    }
    Ok(())
}

async fn vault_cmd(cmd: VaultCmd) -> Result<()> {
    let passphrase = std::env::var("COOP_PASSPHRASE")
        .context("COOP_PASSPHRASE env var is required for vault operations")?;
    match cmd {
        VaultCmd::Init { path } => {
            let _ = coopd_vault::Vault::create(&path, &passphrase)?;
            println!("vault created at {}", path.display());
        }
        VaultCmd::Put { path, name } => {
            let value = std::env::var("COOP_SECRET_VALUE")
                .context("COOP_SECRET_VALUE env var is required for vault put")?;
            let mut v = coopd_vault::Vault::open(&path, &passphrase)?;
            v.put(&name, &value)?;
            println!("stored secret `{name}`");
        }
        VaultCmd::List { path } => {
            let v = coopd_vault::Vault::open(&path, &passphrase)?;
            for name in v.list() {
                println!("{name}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_manifest_is_valid_and_experience_ready() {
        let manifest = starter_manifest(
            "pepper".to_string(),
            "vault:my-key".to_string(),
            "claude-test".to_string(),
        );

        manifest.validate().unwrap();
        assert_eq!(manifest.name, "pepper");
        assert_eq!(manifest.brain.provider_id, "vault:my-key");
        assert_eq!(manifest.brain.model, "claude-test");
        assert_eq!(manifest.tools, ["bash", "file_read", "file_write"]);
        assert_eq!(
            manifest
                .memory
                .as_ref()
                .and_then(|m| m.episodic_retention_days),
            Some(30)
        );
        assert_eq!(
            manifest.network.as_ref().map(|n| n.policy),
            Some(NetPolicy::Open)
        );
    }

    #[test]
    fn starter_command_defaults_to_aria() {
        let cli = Cli::try_parse_from(["coop", "hen", "starter"]).unwrap();
        match cli.cmd {
            Cmd::Hen {
                cmd:
                    HenCmd::Starter {
                        name,
                        provider_id,
                        model,
                    },
            } => {
                assert_eq!(name, "aria");
                assert_eq!(provider_id, "vault:byok-anthropic");
                assert_eq!(model, "claude-sonnet-4-5-20250929");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn starter_preflight_surfaces_connection_errors() {
        let manifest = starter_manifest(
            "aria".into(),
            "unsupported:key".into(),
            "claude-test".into(),
        );
        let yaml = serde_yaml::to_string(&manifest).unwrap();
        let err = ensure_provider_ready(&reqwest::Client::new(), "http://127.0.0.1:1", "", &yaml)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("error sending request"));
    }
}
