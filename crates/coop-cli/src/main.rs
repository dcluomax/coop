//! # coop CLI
//!
//! Command-line client for `coopd`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
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
            println!("{}", serde_json::to_string_pretty(&body)?);
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
    }
    Ok(())
}

async fn simple_post(
    client: &reqwest::Client,
    api: &str,
    token: &str,
    id: &str,
    action: &str,
) -> Result<()> {
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
