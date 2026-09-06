//! # coop CLI
//!
//! Command-line client for `coopd`.

use std::{fmt, io::Write, path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use coopd_core::{AgentManifest, NetPolicy, NetworkSpec, manifest::MemorySpec};
use reqwest::{Method, RequestBuilder, StatusCode, Url, header};
use serde_json::Value;
use tokio::time::{Instant, timeout_at};
use tracing_subscriber::EnvFilter;

const DEFAULT_REQUEST_TIMEOUT_S: u64 = 300;
const MAX_REQUEST_TIMEOUT_S: u64 = 86_400;
const CONNECT_TIMEOUT_S: u64 = 10;

#[derive(Clone, Default)]
struct ApiToken(String);

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl std::str::FromStr for ApiToken {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}

impl ApiToken {
    fn redact(&self, text: &str) -> String {
        if self.0.is_empty() {
            text.to_string()
        } else {
            text.replace(&self.0, "<redacted>")
        }
    }
}

fn normalize_base_url(input: &str, option: &str) -> Result<String> {
    let invalid = || {
        anyhow!(
            "{option} must be an absolute http(s) base URL without credentials, query, or fragment"
        )
    };
    let (scheme, rest) = input.split_once("://").ok_or_else(invalid)?;
    let authority = rest.split('/').next().unwrap_or_default();
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || input
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
        || authority.is_empty()
        || authority.contains('@')
    {
        return Err(invalid());
    }
    let url = Url::parse(input).map_err(|_| invalid())?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

#[derive(Debug)]
struct ApiClient {
    client: reqwest::Client,
    base: String,
    token: ApiToken,
}

impl ApiClient {
    fn new(base: &str, token: ApiToken, request_timeout_s: u64) -> Result<Self> {
        let base = normalize_base_url(base, "--api / COOP_API")?;
        if !(1..=MAX_REQUEST_TIMEOUT_S).contains(&request_timeout_s) {
            bail!("--request-timeout-s must be between 1 and {MAX_REQUEST_TIMEOUT_S}");
        }
        if !token.0.is_empty() {
            header::HeaderValue::from_str(&format!("Bearer {}", token.0))
                .map_err(|_| anyhow!("invalid API token: cannot form an Authorization header"))?;
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(
                CONNECT_TIMEOUT_S.min(request_timeout_s),
            ))
            .timeout(Duration::from_secs(request_timeout_s))
            .build()
            .map_err(|_| anyhow!("could not initialize the HTTP client"))?;
        Ok(Self {
            client,
            base,
            token,
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        auth(
            self.client.request(method, format!("{}{path}", self.base)),
            &self.token.0,
        )
    }

    fn transport_error(&self, error: reqwest::Error) -> anyhow::Error {
        let hint = if error.is_timeout() {
            "request timed out; check the daemon or increase --request-timeout-s"
        } else if error.is_connect() {
            "cannot connect to coopd; check --api / COOP_API and that the daemon is running"
        } else {
            "HTTP request failed"
        };
        anyhow!(
            "{hint}: {}",
            self.token.redact(&error.without_url().to_string())
        )
    }

    async fn send(&self, request: RequestBuilder) -> Result<reqwest::Response> {
        let mut response = request.send().await.map_err(|e| self.transport_error(e))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        // Bound diagnostics from proxies or other non-API servers as well.
        const ERROR_BODY_LIMIT: usize = 8192;
        let mut body = Vec::new();
        while body.len() < ERROR_BODY_LIMIT {
            let chunk = response
                .chunk()
                .await
                .map_err(|e| anyhow!("HTTP {status}: {}", self.transport_error(e)))?;
            let Some(chunk) = chunk else { break };
            body.extend_from_slice(&chunk[..chunk.len().min(ERROR_BODY_LIMIT - body.len())]);
        }
        let message = if body.len() == ERROR_BODY_LIMIT {
            // A truncated body could end halfway through an echoed credential.
            "error response body is too large to display".to_string()
        } else {
            self.error_message(&body)
        };
        let hint = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "; check COOP_API_TOKEN / --token",
            _ if status.is_redirection() => "; redirects are disabled; use the final --api URL",
            _ => "",
        };
        bail!("HTTP {status}: {message}{hint}");
    }

    fn error_message(&self, body: &[u8]) -> String {
        let parsed = serde_json::from_slice::<Value>(body).ok();
        let message = parsed.as_ref().and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
                .or_else(|| value.get("message").and_then(Value::as_str))
        });
        let raw = String::from_utf8_lossy(body);
        let redacted = self.token.redact(message.unwrap_or(&raw));
        let text = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if text.is_empty() {
            return "empty response body".to_string();
        }
        let mut chars = text.chars();
        let mut summary: String = chars.by_ref().take(1024).collect();
        if chars.next().is_some() {
            summary.push('…');
        }
        summary
    }

    async fn json(&self, request: RequestBuilder) -> Result<Value> {
        let response = self.send(request).await?;
        let status = response.status();
        response.json().await.map_err(|e| {
            anyhow!(
                "HTTP {status}: invalid or incomplete JSON response: {}",
                self.transport_error(e)
            )
        })
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.json(self.request(Method::GET, path)).await
    }
}

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
    /// coopd http(s) base URL, optionally with a reverse-proxy path prefix.
    #[arg(
        long,
        global = true,
        env = "COOP_API",
        default_value = "http://127.0.0.1:9700"
    )]
    api: String,

    /// Bearer token for an auth-enabled coopd (matches the daemon's
    /// `COOP_API_TOKEN`). Empty means send no `Authorization` header.
    #[arg(
        long,
        global = true,
        env = "COOP_API_TOKEN",
        default_value = "",
        hide_env_values = true
    )]
    token: ApiToken,

    /// HTTP request deadline in seconds (1..=86400, including response body).
    #[arg(
        long,
        global = true,
        env = "COOP_REQUEST_TIMEOUT_S",
        default_value_t = DEFAULT_REQUEST_TIMEOUT_S,
        value_parser = clap::value_parser!(u64).range(1..=MAX_REQUEST_TIMEOUT_S)
    )]
    request_timeout_s: u64,

    /// Logging filter.
    #[arg(long, global = true, env = "COOP_LOG", default_value = "warn")]
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
    /// Probe daemon readiness, farm access, vault state, and session capabilities.
    Doctor {
        /// Fail if the daemon's local BYOK vault is locked.
        #[arg(long)]
        require_vault: bool,
        /// Fail if persistent-session task dispatch is unavailable.
        #[arg(long)]
        require_task_dispatch: bool,
    },
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum StarterProvider {
    Anthropic,
    Openai,
    OpenaiCompat,
}

impl StarterProvider {
    fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::OpenaiCompat => "openai-compat",
        }
    }

    fn defaults(self) -> (&'static str, &'static str) {
        match self {
            Self::Anthropic => ("vault:byok-anthropic", "claude-sonnet-4-5-20250929"),
            Self::Openai => ("vault:byok-openai", "gpt-4o-mini"),
            Self::OpenaiCompat => ("none", "llama3.1"),
        }
    }
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
        /// Model API provider.
        #[arg(long, value_enum, default_value = "anthropic")]
        provider: StarterProvider,
        /// Compat API base URL, including /v1 when required (resolved by coopd, not this CLI).
        #[arg(long, required_if_eq("provider", "openai-compat"))]
        base_url: Option<String>,
        /// Key reference (defaults: vault:byok-anthropic, vault:byok-openai, or none for compat).
        #[arg(long)]
        provider_id: Option<String>,
        /// Override the provider's starter model (Claude Sonnet 4.5, gpt-4o-mini, or llama3.1).
        #[arg(long)]
        model: Option<String>,
    },
    /// Hatch (boot) a hen.
    Hatch {
        /// Hen ID.
        id: String,
    },
    /// Put a hen to sleep; running jobs must finish first.
    Sleep {
        /// Hen ID.
        id: String,
    },
    /// Wake a sleeping hen; rejected while a job is running.
    Wake {
        /// Hen ID.
        id: String,
    },
    /// Delete a hen permanently; requires no running or queued jobs.
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
    /// Submit a new job; print {job_id}, or the terminal job with --wait.
    Run {
        /// Hen ID, e.g. `local.coop/aria`.
        hen_id: String,
        /// Prompt.
        prompt: String,
        /// Wait for completion; FAILED/CANCELLED exit nonzero after printing the job.
        #[arg(long)]
        wait: bool,
        /// Poll interval in seconds when --wait is set.
        #[arg(long, default_value_t = 2, requires = "wait", value_parser = clap::value_parser!(u64).range(1..))]
        interval_s: u64,
        /// Total submission + wait deadline in seconds when --wait is set.
        #[arg(long, default_value_t = 600, requires = "wait", value_parser = clap::value_parser!(u64).range(1..))]
        timeout_s: u64,
    },
    /// Get a job by ID.
    Get {
        /// Job ID.
        id: String,
    },
    /// List jobs (JSON array, oldest first by default).
    List(JobListOptions),
    /// Poll to a terminal state; FAILED/CANCELLED print JSON then exit nonzero.
    Wait {
        /// Job ID.
        id: String,
        /// Poll interval seconds.
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval_s: u64,
        /// Total wait deadline in seconds, including HTTP requests and sleeps.
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..))]
        timeout_s: u64,
    },
    /// Retry a FAILED/CANCELLED job as a new job, preserving the source; print {job_id}.
    Retry {
        /// Failed or cancelled job ID.
        id: String,
    },
    /// Cancel a QUEUED job only; does not stop a RUNNING job or active process.
    Cancel {
        /// Queued job ID (already CANCELLED is an idempotent success).
        id: String,
    },
}

#[derive(Args, Debug, Default)]
struct JobListOptions {
    /// Optional hen filter.
    #[arg(long)]
    hen_id: Option<String>,
    /// Filter by lifecycle status (case-insensitive).
    #[arg(long, value_parser = ["QUEUED", "RUNNING", "DONE", "FAILED", "CANCELLED"], ignore_case = true)]
    status: Option<String>,
    /// Case-insensitive text search (at most 256 UTF-8 bytes after trimming).
    #[arg(long, visible_alias = "search", value_parser = parse_search)]
    q: Option<String>,
    /// Maximum number of matching jobs (1..=500); omitted returns all.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=500))]
    limit: Option<u16>,
    /// Skip this many matching jobs (default: 0).
    #[arg(long)]
    offset: Option<u64>,
    /// Creation order (default: asc, oldest first).
    #[arg(long, value_parser = ["asc", "desc"], ignore_case = true)]
    order: Option<String>,
}

impl JobListOptions {
    fn query(&self) -> Vec<(&'static str, String)> {
        let mut query = Vec::new();
        for (key, value) in [
            ("hen_id", &self.hen_id),
            ("status", &self.status),
            ("q", &self.q),
            ("order", &self.order),
        ] {
            if let Some(value) = value {
                query.push((key, value.clone()));
            }
        }
        if let Some(limit) = self.limit {
            query.push(("limit", limit.to_string()));
        }
        if let Some(offset) = self.offset {
            query.push(("offset", offset.to_string()));
        }
        query
    }
}

fn parse_search(input: &str) -> std::result::Result<String, String> {
    let text = input.trim();
    if text.len() > 256 {
        return Err("search must be at most 256 UTF-8 bytes after trimming".to_string());
    }
    Ok(text.to_string())
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
        .with_writer(std::io::stderr)
        .with_target(false)
        .compact()
        .init();

    let token = cli.token.clone();
    execute(cli)
        .await
        .map_err(|error| anyhow!("{}", token.redact(&format!("{error:#}"))))
}

async fn execute(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Vault { cmd } => vault_cmd(cmd).await,
        cmd => {
            let api = ApiClient::new(&cli.api, cli.token, cli.request_timeout_s)?;
            http_cmd(&api, cmd).await
        }
    }
}

async fn http_cmd(api: &ApiClient, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Health => {
            println!("{}", api.get("/api/v1/healthz").await?);
            Ok(())
        }
        Cmd::Farm => print_json(&api.get("/api/v1/farm").await?),
        Cmd::Doctor {
            require_vault,
            require_task_dispatch,
        } => {
            let report = doctor_report(api, require_vault, require_task_dispatch).await;
            write_doctor_report(&report, std::io::stdout().lock())
        }
        Cmd::Hen { cmd } => hen_cmd(api, cmd).await,
        Cmd::Job { cmd } => job_cmd(api, cmd).await,
        Cmd::Vault { .. } => unreachable!("vault operations do not use HTTP"),
    }
}

fn write_json(value: &Value, output: &mut impl Write) -> Result<()> {
    serde_json::to_writer_pretty(&mut *output, value)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

fn print_json(value: &Value) -> Result<()> {
    write_json(value, &mut std::io::stdout().lock())
}

async fn hen_cmd(api: &ApiClient, cmd: HenCmd) -> Result<()> {
    match cmd {
        HenCmd::List { state } => {
            let mut request = api.request(Method::GET, "/api/v1/hens");
            if let Some(state) = state {
                request = request.query(&[("state", state)]);
            }
            print_json(&api.json(request).await?)?;
        }
        HenCmd::Get { id } => {
            print_json(&api.get(&format!("/api/v1/hens/{}", enc(&id))).await?)?;
        }
        HenCmd::Create { file } => {
            let yaml = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let id = create_hen(api, yaml).await?;
            print_json(&Value::String(id))?;
        }
        HenCmd::Starter {
            name,
            provider,
            base_url,
            provider_id,
            model,
        } => {
            let manifest =
                starter_manifest(name, provider, provider_id, model, base_url.as_deref())?;
            let id = create_starter(api, &manifest).await?;
            println!("🐣 Hatched starter Hen `{id}`");
            println!("   memory: 30 days · tools: bash, file_read, file_write");
            println!("Next:");
            println!("   coop job run {id} \"Create hello.txt with one useful idea\"");
        }
        HenCmd::Hatch { id } => print_json(&hen_action(api, &id, "hatch").await?)?,
        HenCmd::Sleep { id } => print_json(&hen_action(api, &id, "sleep").await?)?,
        HenCmd::Wake { id } => print_json(&hen_action(api, &id, "wake").await?)?,
        HenCmd::Delete { id } => {
            let resp = api
                .send(api.request(Method::DELETE, &format!("/api/v1/hens/{}", enc(&id))))
                .await?;
            println!("status: {}", resp.status());
        }
        HenCmd::Memory { id, limit } => {
            let mut request =
                api.request(Method::GET, &format!("/api/v1/hens/{}/memory", enc(&id)));
            if let Some(limit) = limit {
                request = request.query(&[("limit", limit)]);
            }
            print_json(&api.json(request).await?)?;
        }
        HenCmd::Forget { id } => {
            let request = api.request(Method::DELETE, &format!("/api/v1/hens/{}/memory", enc(&id)));
            print_json(&api.json(request).await?)?;
        }
        HenCmd::Delegate { from, to, prompt } => {
            let request = api
                .request(
                    Method::POST,
                    &format!("/api/v1/hens/{}/delegate", enc(&from)),
                )
                .json(&serde_json::json!({ "to": to, "prompt": prompt }));
            print_json(&api.json(request).await?)?;
        }
    }
    Ok(())
}

fn starter_manifest(
    name: String,
    provider: StarterProvider,
    provider_id: Option<String>,
    model: Option<String>,
    base_url: Option<&str>,
) -> Result<AgentManifest> {
    if provider != StarterProvider::OpenaiCompat && base_url.is_some() {
        bail!("--base-url is only supported with --provider openai-compat");
    }
    if provider == StarterProvider::OpenaiCompat && base_url.is_none() {
        bail!("--base-url is required with --provider openai-compat");
    }
    let (default_key, default_model) = provider.defaults();
    let mut manifest = AgentManifest::minimal(name);
    manifest.brain.provider = provider.as_str().to_string();
    manifest.brain.provider_id = provider_id.unwrap_or_else(|| default_key.to_string());
    manifest.brain.model = model.unwrap_or_else(|| default_model.to_string());
    manifest.brain.base_url = base_url
        .map(|url| normalize_base_url(url, "--base-url"))
        .transpose()?;
    if manifest.name.trim().is_empty()
        || manifest.brain.provider_id.trim().is_empty()
        || manifest.brain.model.trim().is_empty()
    {
        bail!("starter name, --provider-id, and --model must not be empty");
    }
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
        .validate()
        .context("generated starter manifest is invalid")?;
    Ok(manifest)
}

async fn create_starter(api: &ApiClient, manifest: &AgentManifest) -> Result<String> {
    let yaml = serde_yaml::to_string(manifest)?;
    ensure_provider_ready(api, &yaml).await?;
    let id = create_hen(api, yaml).await?;
    hen_action(api, &id, "hatch").await.with_context(|| {
        format!("Hen `{id}` was created but could not hatch; inspect it with `coop hen get {id}`")
    })?;
    Ok(id)
}

async fn ensure_provider_ready(api: &ApiClient, manifest_yaml: &str) -> Result<()> {
    let request = api
        .request(Method::POST, "/api/v1/hens/preflight")
        .header("content-type", "application/yaml")
        .body(manifest_yaml.to_string());
    let body = api
        .json(request)
        .await
        .context("provider preflight failed; no Hen was created")?;
    if body.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("provider preflight did not confirm readiness; no Hen was created");
    }
    Ok(())
}

async fn create_hen(api: &ApiClient, yaml: String) -> Result<String> {
    let request = api
        .request(Method::POST, "/api/v1/hens")
        .header("content-type", "application/yaml")
        .body(yaml);
    let body = api.json(request).await?;
    body.as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .context("create response did not contain a Hen id")
}

async fn hen_action(api: &ApiClient, id: &str, action: &str) -> Result<Value> {
    api.json(api.request(Method::POST, &format!("/api/v1/hens/{}/{action}", enc(id))))
        .await
}

async fn job_cmd(api: &ApiClient, cmd: JobCmd) -> Result<()> {
    match cmd {
        JobCmd::Run {
            hen_id,
            prompt,
            wait,
            interval_s,
            timeout_s,
        } => {
            let settings = wait
                .then(|| WaitSettings::new(interval_s, timeout_s))
                .transpose()?;
            let body = run_job(api, &hen_id, &prompt, settings).await?;
            if wait {
                write_terminal_job(&body, std::io::stdout().lock())?;
            } else {
                print_json(&body)?;
            }
        }
        JobCmd::Get { id } => {
            print_json(&api.get(&format!("/api/v1/jobs/{}", enc(&id))).await?)?;
        }
        JobCmd::List(options) => {
            let mut request = api.request(Method::GET, "/api/v1/jobs");
            let query = options.query();
            if !query.is_empty() {
                request = request.query(&query);
            }
            print_json(&api.json(request).await?)?;
        }
        JobCmd::Wait {
            id,
            interval_s,
            timeout_s,
        } => {
            let settings = WaitSettings::new(interval_s, timeout_s)?;
            let body = poll_job_until(api, &id, settings.interval, settings.deadline()?).await?;
            write_terminal_job(&body, std::io::stdout().lock())?;
        }
        JobCmd::Retry { id } => {
            let request = api.request(Method::POST, &format!("/api/v1/jobs/{}/retry", enc(&id)));
            let body = api.json(request).await.context(
                "retry requires a FAILED or CANCELLED source; the original job is retained",
            )?;
            print_json(&body)?;
        }
        JobCmd::Cancel { id } => {
            let request = api.request(Method::POST, &format!("/api/v1/jobs/{}/cancel", enc(&id)));
            let body = api
                .json(request)
                .await
                .context("cancel is QUEUED-only and cannot stop a RUNNING job or active process")?;
            print_json(&body)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct WaitSettings {
    interval: Duration,
    timeout: Duration,
}

impl WaitSettings {
    fn new(interval_s: u64, timeout_s: u64) -> Result<Self> {
        if interval_s == 0 || timeout_s == 0 {
            bail!("--interval-s and --timeout-s must both be greater than zero");
        }
        Ok(Self {
            interval: Duration::from_secs(interval_s),
            timeout: Duration::from_secs(timeout_s),
        })
    }

    fn deadline(self) -> Result<Instant> {
        Instant::now()
            .checked_add(self.timeout)
            .context("--timeout-s is too large")
    }
}

async fn run_job(
    api: &ApiClient,
    hen_id: &str,
    prompt: &str,
    wait: Option<WaitSettings>,
) -> Result<Value> {
    let request = api
        .request(Method::POST, &format!("/api/v1/hens/{}/jobs", enc(hen_id)))
        .json(&serde_json::json!({ "prompt": prompt }));
    let Some(settings) = wait else {
        return api.json(request).await;
    };
    let deadline = settings.deadline()?;
    let body = timeout_at(deadline, api.json(request))
        .await
        .map_err(|_| anyhow!("timeout submitting job; acceptance is unknown. Inspect `coop job list` before resubmitting"))?
        .context("job submission failed; after an ambiguous network failure, inspect `coop job list` before resubmitting")?;
    let id = body.get("job_id").and_then(Value::as_str).filter(|id| !id.is_empty())
        .context("job submission response did not contain a job_id; inspect `coop job list` before resubmitting")?;
    poll_job_until(api, id, settings.interval, deadline).await
}

fn wait_timeout(id: &str) -> anyhow::Error {
    anyhow!(
        "timeout waiting for job `{id}`; the job was not cancelled. Inspect it with `coop job get {id}`"
    )
}

async fn poll_job_until(
    api: &ApiClient,
    id: &str,
    interval: Duration,
    deadline: Instant,
) -> Result<Value> {
    let path = format!("/api/v1/jobs/{}", enc(id));
    timeout_at(deadline, async {
        loop {
            if Instant::now() >= deadline {
                return Err(wait_timeout(id));
            }
            let job = api.get(&path).await.with_context(|| {
                format!("could not poll job `{id}`; inspect it with `coop job get {id}`")
            })?;
            if Instant::now() >= deadline {
                return Err(wait_timeout(id));
            }
            match job.get("status").and_then(Value::as_str) {
                Some("DONE" | "FAILED" | "CANCELLED") => return Ok(job),
                Some("QUEUED" | "RUNNING") => {}
                _ => bail!("job `{id}` response has a missing or unknown status"),
            }
            tokio::time::sleep(interval.min(deadline.saturating_duration_since(Instant::now())))
                .await;
        }
    })
    .await
    .map_err(|_| wait_timeout(id))?
}

fn write_terminal_job(job: &Value, mut output: impl Write) -> Result<()> {
    write_json(job, &mut output)?;
    match job.get("status").and_then(Value::as_str) {
        Some("DONE") => Ok(()),
        Some(status @ ("FAILED" | "CANCELLED")) => {
            let id = job.get("id").and_then(Value::as_str).unwrap_or("unknown");
            bail!("job `{id}` ended with {status}; see the job JSON above");
        }
        _ => bail!("job response is not terminal"),
    }
}

async fn doctor_report(api: &ApiClient, require_vault: bool, require_task_dispatch: bool) -> Value {
    let (health, readiness, farm, vault, sessions) = tokio::join!(
        api.get("/api/v1/healthz"),
        api.get("/api/v1/readyz"),
        api.get("/api/v1/farm"),
        api.get("/api/v1/vault/status"),
        api.get("/api/v1/session/capabilities"),
    );
    let mut checks = serde_json::Map::new();
    for (name, result) in [
        ("health", health),
        ("readiness", readiness),
        ("farm", farm),
        ("vault", vault),
        ("sessions", sessions),
    ] {
        let check = match result {
            Ok(data) => {
                let issue = match name {
                    "health" | "readiness"
                        if data.get("ok").and_then(Value::as_bool) != Some(true) =>
                    {
                        Some("daemon did not confirm readiness")
                    }
                    "farm"
                        if data.get("coop_id").and_then(Value::as_str).is_none()
                            || data.get("hen_count").and_then(Value::as_u64).is_none()
                            || data.get("coopd_version").and_then(Value::as_str).is_none() =>
                    {
                        Some("invalid farm response")
                    }
                    "vault" if data.get("unlocked").and_then(Value::as_bool).is_none() => {
                        Some("invalid vault status response")
                    }
                    "vault" if require_vault && data["unlocked"] != true => Some(
                        "local vault is locked; configure COOP_VAULT and COOP_PASSPHRASE on the daemon",
                    ),
                    "sessions"
                        if ["shell", "persistent_session", "task_dispatch"]
                            .iter()
                            .any(|field| data.get(field).and_then(Value::as_bool).is_none())
                            || data.get("backend").and_then(Value::as_str).is_none() =>
                    {
                        Some("invalid session capabilities response")
                    }
                    "sessions"
                        if require_task_dispatch
                            && (data["task_dispatch"] != true
                                || data["persistent_session"] != true) =>
                    {
                        Some(
                            "persistent-session task dispatch is unavailable; check the daemon's session backend",
                        )
                    }
                    _ => None,
                };
                let mut check = serde_json::json!({ "ok": issue.is_none(), "response": data });
                if let Some(issue) = issue {
                    check["error"] = Value::String(issue.to_string());
                }
                check
            }
            Err(error) => serde_json::json!({
                "ok": false,
                "error": api.token.redact(&format!("{error:#}")),
            }),
        };
        checks.insert(name.to_string(), check);
    }
    let ok = checks.values().all(|check| check["ok"] == true);
    serde_json::json!({
        "ok": ok,
        "client_version": env!("CARGO_PKG_VERSION"),
        "requirements": {
            "vault_unlocked": require_vault,
            "task_dispatch": require_task_dispatch,
        },
        "checks": checks,
    })
}

fn write_doctor_report(report: &Value, mut output: impl Write) -> Result<()> {
    write_json(report, &mut output)?;
    if report.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!(
            "doctor found failed probes or unavailable required capabilities; see the report above"
        );
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
    use std::collections::BTreeMap;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    fn parse(args: &[&str]) -> Cli {
        try_parse(args).unwrap()
    }

    fn try_parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(
            [
                "coop",
                "--api",
                "http://127.0.0.1:9700",
                "--token",
                "",
                "--request-timeout-s",
                "300",
                "--log",
                "warn",
            ]
            .into_iter()
            .chain(args.iter().copied()),
        )
    }

    fn parsed_starter(args: &[&str]) -> Result<AgentManifest> {
        let cli = parse(args);
        match cli.cmd {
            Cmd::Hen {
                cmd:
                    HenCmd::Starter {
                        name,
                        provider,
                        provider_id,
                        model,
                        base_url,
                    },
            } => starter_manifest(name, provider, provider_id, model, base_url.as_deref()),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[derive(Debug)]
    struct Reply {
        path: String,
        status: &'static str,
        body: String,
        header_delay: Duration,
        body_delay: Duration,
        location: Option<String>,
    }

    impl Reply {
        fn new(path: &str, status: &'static str, body: &str) -> Self {
            Self {
                path: path.to_string(),
                status,
                body: body.to_string(),
                header_delay: Duration::ZERO,
                body_delay: Duration::ZERO,
                location: None,
            }
        }
    }

    #[derive(Debug)]
    struct RecordedRequest {
        method: String,
        target: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    async fn read_request(socket: &mut TcpStream) -> RecordedRequest {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let header_end = loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0, "request ended before its headers");
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break end + 4;
            }
            assert!(bytes.len() < 65_536);
        };
        let head = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let mut lines = head.lines();
        let mut start = lines.next().unwrap().split_whitespace();
        let method = start.next().unwrap().to_string();
        let target = start.next().unwrap().to_string();
        let headers: BTreeMap<_, _> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        let content_length = headers
            .get("content-length")
            .map_or(0, |value| value.parse::<usize>().unwrap());
        assert!(content_length < 65_536);
        while bytes.len() < header_end + content_length {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0, "request ended before its body");
            bytes.extend_from_slice(&buffer[..count]);
        }
        RecordedRequest {
            method,
            target,
            headers,
            body: String::from_utf8(bytes[header_end..header_end + content_length].to_vec())
                .unwrap(),
        }
    }

    async fn mock_api(mut replies: Vec<Reply>) -> (ApiClient, JoinHandle<Vec<RecordedRequest>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            while !replies.is_empty() {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let request =
                    tokio::time::timeout(Duration::from_secs(3), read_request(&mut socket))
                        .await
                        .unwrap();
                let path = request.target.split('?').next().unwrap();
                let index = replies
                    .iter()
                    .position(|reply| reply.path == path)
                    .unwrap_or_else(|| panic!("unexpected request: {request:?}"));
                let reply = replies.remove(index);
                requests.push(request);
                tokio::time::sleep(reply.header_delay).await;
                let location = reply
                    .location
                    .map_or_else(String::new, |url| format!("Location: {url}\r\n"));
                let headers = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{location}\r\n",
                    reply.status,
                    reply.body.len()
                );
                if socket.write_all(headers.as_bytes()).await.is_ok() {
                    tokio::time::sleep(reply.body_delay).await;
                    let _ = socket.write_all(reply.body.as_bytes()).await;
                }
            }
            requests
        });
        let api = ApiClient::new(
            &base,
            ApiToken("test-cli-auth".into()),
            DEFAULT_REQUEST_TIMEOUT_S,
        )
        .unwrap();
        (api, server)
    }

    #[test]
    fn starter_defaults_preserve_anthropic_and_support_existing_providers() {
        for (provider, key, model) in [
            (
                "anthropic",
                "vault:byok-anthropic",
                "claude-sonnet-4-5-20250929",
            ),
            ("openai", "vault:byok-openai", "gpt-4o-mini"),
            ("openai-compat", "none", "llama3.1"),
        ] {
            let mut args = vec!["hen", "starter", "--provider", provider];
            if provider == "openai-compat" {
                args.extend(["--base-url", "http://localhost:11434/v1/"]);
            }
            let manifest = parsed_starter(&args).unwrap();
            assert_eq!(manifest.name, "aria");
            assert_eq!(manifest.brain.provider, provider);
            assert_eq!(manifest.brain.provider_id, key);
            assert_eq!(manifest.brain.model, model);
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
            manifest.validate().unwrap();
            if provider == "openai-compat" {
                assert_eq!(
                    manifest.brain.base_url.as_deref(),
                    Some("http://localhost:11434/v1")
                );
            } else {
                assert!(manifest.brain.base_url.is_none());
            }
        }
        let manifest = parsed_starter(&["hen", "starter"]).unwrap();
        assert_eq!(manifest.brain.provider, "anthropic");
        assert_eq!(manifest.brain.provider_id, "vault:byok-anthropic");
        assert_eq!(manifest.brain.model, "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn starter_overrides_and_invalid_provider_settings() {
        let manifest = parsed_starter(&[
            "hen",
            "starter",
            "pepper",
            "--provider-id",
            "vault:my-key",
            "--model",
            "claude-test",
        ])
        .unwrap();
        assert_eq!(manifest.name, "pepper");
        assert_eq!(manifest.brain.provider_id, "vault:my-key");
        assert_eq!(manifest.brain.model, "claude-test");
        let manifest = parsed_starter(&[
            "hen",
            "starter",
            "--provider",
            "openai-compat",
            "--provider-id",
            "vault:remote",
            "--model",
            "my-model",
            "--base-url",
            "https://models.example.test/v1",
        ])
        .unwrap();
        assert_eq!(manifest.brain.provider_id, "vault:remote");
        assert_eq!(manifest.brain.model, "my-model");
        assert!(try_parse(&["hen", "starter", "--provider", "openai-compat"]).is_err());
        assert!(try_parse(&["hen", "starter", "--provider", "unknown"]).is_err());
        for args in [
            vec!["hen", "starter", "--provider-id", "none"],
            vec![
                "hen",
                "starter",
                "--provider",
                "openai",
                "--provider-id",
                "none",
            ],
            vec!["hen", "starter", "--base-url", "https://example.test/v1"],
            vec!["hen", "starter", "--model", " "],
            vec![
                "hen",
                "starter",
                "--provider",
                "openai-compat",
                "--base-url",
                "http://169.254.169.254/v1",
            ],
        ] {
            assert!(parsed_starter(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn legacy_commands_and_global_options_parse() {
        match parse(&["job", "run", "local.coop/aria", "hello"]).cmd {
            Cmd::Job {
                cmd:
                    JobCmd::Run {
                        wait,
                        interval_s,
                        timeout_s,
                        ..
                    },
            } => {
                assert!(!wait);
                assert_eq!((interval_s, timeout_s), (2, 600));
            }
            other => panic!("unexpected command: {other:?}"),
        }
        match parse(&["job", "wait", "job-1"]).cmd {
            Cmd::Job {
                cmd:
                    JobCmd::Wait {
                        interval_s,
                        timeout_s,
                        ..
                    },
            } => {
                assert_eq!((interval_s, timeout_s), (2, 600));
            }
            other => panic!("unexpected command: {other:?}"),
        }
        match parse(&["job", "list"]).cmd {
            Cmd::Job {
                cmd: JobCmd::List(options),
            } => assert!(options.query().is_empty()),
            other => panic!("unexpected command: {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "coop",
            "farm",
            "--api",
            "http://localhost:9700/",
            "--token",
            "debug-test-secret",
            "--request-timeout-s",
            "450",
        ])
        .unwrap();
        assert_eq!(cli.request_timeout_s, 450);
        assert!(!format!("{cli:?}").contains("debug-test-secret"));
    }

    #[test]
    fn parsing_rejects_zero_timeouts_and_unusable_filters() {
        for args in [
            vec!["job", "wait", "j", "--interval-s", "0"],
            vec!["job", "wait", "j", "--timeout-s", "0"],
            vec![
                "job",
                "run",
                "local.coop/aria",
                "hello",
                "--wait",
                "--interval-s",
                "0",
            ],
            vec![
                "job",
                "run",
                "local.coop/aria",
                "hello",
                "--wait",
                "--timeout-s",
                "0",
            ],
            vec![
                "job",
                "run",
                "local.coop/aria",
                "hello",
                "--interval-s",
                "1",
            ],
            vec!["job", "list", "--limit", "0"],
            vec!["job", "list", "--limit", "501"],
            vec!["job", "list", "--offset", "-1"],
            vec!["job", "list", "--status", "BOGUS"],
            vec!["job", "list", "--order", "random"],
        ] {
            assert!(try_parse(&args).is_err(), "{args:?}");
        }
        for value in ["0", "86401"] {
            assert!(Cli::try_parse_from(["coop", "--request-timeout-s", value, "farm"]).is_err());
        }
        assert!(WaitSettings::new(0, 1).is_err());
        assert!(WaitSettings::new(1, 0).is_err());
        assert!(WaitSettings::new(1, u64::MAX).unwrap().deadline().is_err());
        assert!(parse_search(&"é".repeat(129)).is_err());
        assert_eq!(
            parse_search(&format!(" {} ", "é".repeat(128)))
                .unwrap()
                .len(),
            256
        );
    }

    #[test]
    fn urls_are_normalized_without_exposing_invalid_credentials() {
        for (input, expected) in [
            ("http://localhost:9700///", "http://localhost:9700"),
            ("HTTPS://EXAMPLE.TEST/farm///", "https://example.test/farm"),
            ("http://[::1]:9700", "http://[::1]:9700"),
            (
                "https://example.test/my%20farm/",
                "https://example.test/my%20farm",
            ),
        ] {
            assert_eq!(normalize_base_url(input, "--api").unwrap(), expected);
        }
        for input in [
            "localhost:9700",
            "/relative",
            "ftp://example.test",
            "http:example.test",
            "http:///example.test",
            "http://",
            "https://example.test/?q=secret",
            "https://example.test/#secret",
            "http://user:secret@example.test",
            "http://@example.test",
            "http://example.test\\other",
            " http://example.test",
            "http://example.test/\n",
            "http://example.test?",
        ] {
            let error = normalize_base_url(input, "--api").unwrap_err().to_string();
            assert!(error.contains("--api"), "{input}");
            assert!(!error.contains("secret"), "{input}");
        }
        for timeout in [0, MAX_REQUEST_TIMEOUT_S + 1] {
            assert!(ApiClient::new("http://localhost", ApiToken::default(), timeout).is_err());
        }
    }

    #[test]
    fn transport_auth_and_errors_redact_tokens() {
        let api = ApiClient::new(
            "http://localhost:9700",
            ApiToken("test-cli-auth".into()),
            300,
        )
        .unwrap();
        let request = api.request(Method::GET, "/api/v1/farm").build().unwrap();
        assert!(request.headers()[header::AUTHORIZATION].is_sensitive());
        assert!(!format!("{api:?} {request:?}").contains("test-cli-auth"));
        for (body, expected) in [
            (r#"{"error":"denied test-cli-auth"}"#, "denied <redacted>"),
            (r#"{"error":{"message":"not allowed"}}"#, "not allowed"),
            (r#"{"message":"not found"}"#, "not found"),
            (" \n Bad gateway \r\n ", "Bad gateway"),
            ("", "empty response body"),
            ("\u{1b}test-cli-auth", "<redacted>"),
        ] {
            assert_eq!(api.error_message(body.as_bytes()), expected);
        }
        assert!(
            api.error_message("a".repeat(2048).as_bytes())
                .ends_with('…')
        );
        let invalid =
            ApiClient::new("http://localhost", ApiToken("bad\r\ntoken".into()), 300).unwrap_err();
        assert!(!format!("{invalid:#}").contains("bad"));
    }

    #[tokio::test]
    async fn transport_preserves_success_and_sends_auth_with_prefix() {
        let (mut api, server) = mock_api(vec![
            Reply::new("/prefix/api/v1/farm", "200 OK", r#"{"hen_count":2}"#),
            Reply::new(
                "/prefix/api/v1/hens/local.coop%2Faria",
                "204 No Content",
                "",
            ),
        ])
        .await;
        api.base = normalize_base_url(&format!("{}/prefix///", api.base), "--api").unwrap();
        assert_eq!(api.get("/api/v1/farm").await.unwrap()["hen_count"], 2);
        hen_cmd(
            &api,
            HenCmd::Delete {
                id: "local.coop/aria".into(),
            },
        )
        .await
        .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests[0].headers["authorization"], "Bearer test-cli-auth");
        assert_eq!(requests[1].method, "DELETE");
    }

    #[tokio::test]
    async fn empty_token_does_not_send_authorization() {
        let (api, server) = mock_api(vec![Reply::new(
            "/api/v1/healthz",
            "200 OK",
            r#"{"ok":true}"#,
        )])
        .await;
        let api = ApiClient::new(&api.base, ApiToken::default(), 300).unwrap();
        assert_eq!(api.get("/api/v1/healthz").await.unwrap()["ok"], true);
        assert!(
            !server.await.unwrap()[0]
                .headers
                .contains_key("authorization")
        );
    }

    #[tokio::test]
    async fn every_read_command_rejects_http_errors() {
        for (args, path) in [
            (vec!["health"], "/api/v1/healthz"),
            (vec!["farm"], "/api/v1/farm"),
            (vec!["hen", "list"], "/api/v1/hens"),
            (
                vec!["hen", "get", "local.coop/aria"],
                "/api/v1/hens/local.coop%2Faria",
            ),
            (
                vec!["hen", "memory", "local.coop/aria"],
                "/api/v1/hens/local.coop%2Faria/memory",
            ),
            (vec!["job", "get", "job-1"], "/api/v1/jobs/job-1"),
            (vec!["job", "list"], "/api/v1/jobs"),
        ] {
            let (api, server) = mock_api(vec![Reply::new(
                path,
                "401 Unauthorized",
                r#"{"error":"denied test-cli-auth"}"#,
            )])
            .await;
            let error = http_cmd(&api, parse(&args).cmd).await.unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains("HTTP 401 Unauthorized"),
                "{args:?}: {message}"
            );
            assert!(message.contains("denied <redacted>"), "{message}");
            assert!(!message.contains("test-cli-auth"));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn non_json_errors_and_invalid_success_json_are_failures() {
        for (status, body, expected) in [
            (
                "502 Bad Gateway",
                "<html>upstream unavailable</html>",
                "upstream unavailable",
            ),
            ("404 Not Found", "", "empty response body"),
            (
                "403 Forbidden",
                r#"{"message":"permission denied"}"#,
                "permission denied",
            ),
            ("200 OK", "not json", "invalid or incomplete JSON"),
        ] {
            let (api, server) = mock_api(vec![Reply::new("/api/v1/farm", status, body)]).await;
            let error = api.get("/api/v1/farm").await.unwrap_err().to_string();
            assert!(error.contains(status), "{error}");
            assert!(error.contains(expected), "{error}");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn oversized_error_body_does_not_expose_a_truncated_token() {
        let body = format!("{}test-cli-auth", "x".repeat(8186));
        let (api, server) =
            mock_api(vec![Reply::new("/api/v1/farm", "502 Bad Gateway", &body)]).await;
        let error = api.get("/api/v1/farm").await.unwrap_err().to_string();
        assert!(error.contains("HTTP 502"));
        assert!(error.contains("too large to display"));
        assert!(!error.contains("test-cli"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn redirects_are_not_followed_or_given_credentials() {
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut reply = Reply::new("/api/v1/farm", "307 Temporary Redirect", "");
        reply.location = Some(format!(
            "http://{}/capture",
            destination.local_addr().unwrap()
        ));
        let (api, server) = mock_api(vec![reply]).await;
        let error = api.get("/api/v1/farm").await.unwrap_err().to_string();
        assert!(error.contains("HTTP 307"));
        assert!(error.contains("redirects are disabled"));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), destination.accept())
                .await
                .is_err()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn list_filters_and_ids_are_safely_encoded() {
        let (api, server) = mock_api(vec![
            Reply::new("/api/v1/jobs", "200 OK", "[]"),
            Reply::new(
                "/api/v1/jobs/job%2F1%3Fextra%3Dyes",
                "200 OK",
                r#"{"status":"DONE"}"#,
            ),
        ])
        .await;
        http_cmd(
            &api,
            parse(&[
                "job",
                "list",
                "--hen-id",
                "local.coop/a&b",
                "--status",
                "failed",
                "--search",
                "hello & 世界",
                "--limit",
                "25",
                "--offset",
                "3",
                "--order",
                "DESC",
            ])
            .cmd,
        )
        .await
        .unwrap();
        job_cmd(
            &api,
            JobCmd::Get {
                id: "job/1?extra=yes".into(),
            },
        )
        .await
        .unwrap();
        let requests = server.await.unwrap();
        let url = Url::parse(&format!("http://localhost{}", requests[0].target)).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["hen_id"], "local.coop/a&b");
        assert_eq!(query["status"], "failed");
        assert_eq!(query["q"], "hello & 世界");
        assert_eq!(query["limit"], "25");
        assert_eq!(query["offset"], "3");
        assert_eq!(query["order"], "DESC");
        assert_eq!(query.len(), 6);
    }

    #[tokio::test]
    async fn starter_preflights_then_creates_and_hatches_the_same_manifest() {
        let (api, server) = mock_api(vec![
            Reply::new("/api/v1/hens/preflight", "200 OK", r#"{"ok":true}"#),
            Reply::new("/api/v1/hens", "201 Created", r#""local.coop/aria""#),
            Reply::new(
                "/api/v1/hens/local.coop%2Faria/hatch",
                "200 OK",
                r#"{"ok":true}"#,
            ),
        ])
        .await;
        let manifest = parsed_starter(&[
            "hen",
            "starter",
            "--provider",
            "openai-compat",
            "--provider-id",
            "none",
            "--base-url",
            "http://localhost:11434/v1",
        ])
        .unwrap();
        assert_eq!(
            create_starter(&api, &manifest).await.unwrap(),
            "local.coop/aria"
        );
        let requests = server.await.unwrap();
        assert!(requests.iter().all(|request| request.method == "POST"));
        assert_eq!(requests[0].target, "/api/v1/hens/preflight");
        assert_eq!(requests[1].target, "/api/v1/hens");
        assert_eq!(requests[0].body, requests[1].body);
        assert_eq!(requests[0].headers["content-type"], "application/yaml");
        let submitted = AgentManifest::parse_yaml(&requests[1].body).unwrap();
        assert_eq!(submitted.brain.provider, "openai-compat");
        assert_eq!(submitted.brain.provider_id, "none");
    }

    #[tokio::test]
    async fn starter_does_not_create_when_preflight_fails() {
        for (status, body, expected) in [
            (
                "422 Unprocessable Entity",
                r#"{"error":"vault is locked"}"#,
                "vault is locked",
            ),
            ("200 OK", r#"{"ok":false}"#, "did not confirm readiness"),
            ("500 Internal Server Error", "bad gateway", "bad gateway"),
        ] {
            let (api, server) =
                mock_api(vec![Reply::new("/api/v1/hens/preflight", status, body)]).await;
            let manifest = parsed_starter(&["hen", "starter"]).unwrap();
            let error = create_starter(&api, &manifest).await.unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            assert_eq!(server.await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn starter_hatch_failure_identifies_the_created_hen() {
        let (api, server) = mock_api(vec![
            Reply::new("/api/v1/hens/preflight", "200 OK", r#"{"ok":true}"#),
            Reply::new("/api/v1/hens", "201 Created", r#""local.coop/aria""#),
            Reply::new(
                "/api/v1/hens/local.coop%2Faria/hatch",
                "409 Conflict",
                r#"{"error":"not hatchable"}"#,
            ),
        ])
        .await;
        let manifest = parsed_starter(&["hen", "starter"]).unwrap();
        let error = create_starter(&api, &manifest).await.unwrap_err();
        assert!(format!("{error:#}").contains("local.coop/aria` was created but could not hatch"));
        assert!(format!("{error:#}").contains("HTTP 409 Conflict"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn starter_preflight_surfaces_connection_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let api = ApiClient::new(&base, ApiToken::default(), 1).unwrap();
        let error = ensure_provider_ready(&api, "unused").await.unwrap_err();
        assert!(format!("{error:#}").contains("cannot connect to coopd"));
    }

    #[tokio::test]
    async fn mutations_use_checked_transport_and_queued_only_cancel_help() {
        for (args, path) in [
            (
                vec!["hen", "delete", "local.coop/aria"],
                "/api/v1/hens/local.coop%2Faria",
            ),
            (
                vec!["hen", "hatch", "local.coop/aria"],
                "/api/v1/hens/local.coop%2Faria/hatch",
            ),
            (
                vec!["hen", "sleep", "local.coop/aria"],
                "/api/v1/hens/local.coop%2Faria/sleep",
            ),
            (
                vec!["hen", "wake", "local.coop/aria"],
                "/api/v1/hens/local.coop%2Faria/wake",
            ),
            (
                vec!["hen", "forget", "local.coop/aria"],
                "/api/v1/hens/local.coop%2Faria/memory",
            ),
            (
                vec![
                    "hen",
                    "delegate",
                    "local.coop/aria",
                    "local.coop/scout",
                    "hello",
                ],
                "/api/v1/hens/local.coop%2Faria/delegate",
            ),
            (
                vec!["job", "run", "local.coop/aria", "hello"],
                "/api/v1/hens/local.coop%2Faria/jobs",
            ),
            (vec!["job", "retry", "j"], "/api/v1/jobs/j/retry"),
            (vec!["job", "cancel", "j"], "/api/v1/jobs/j/cancel"),
        ] {
            let (api, server) = mock_api(vec![Reply::new(
                path,
                "409 Conflict",
                r#"{"error":"state conflict"}"#,
            )])
            .await;
            let error = http_cmd(&api, parse(&args).cmd).await.unwrap_err();
            assert!(
                format!("{error:#}").contains("HTTP 409 Conflict"),
                "{args:?}: {error:#}"
            );
            server.await.unwrap();
        }
        let help = try_parse(&["job", "cancel", "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("QUEUED"));
        assert!(help.contains("does not stop a RUNNING job"));
    }

    #[tokio::test]
    async fn retry_cancel_and_submission_keep_the_api_response_shape() {
        let (api, server) = mock_api(vec![
            Reply::new(
                "/api/v1/jobs/failed/retry",
                "202 Accepted",
                r#"{"job_id":"retry-1"}"#,
            ),
            Reply::new(
                "/api/v1/jobs/queued/cancel",
                "200 OK",
                r#"{"id":"queued","status":"CANCELLED"}"#,
            ),
            Reply::new(
                "/api/v1/hens/local.coop%2Faria/jobs",
                "202 Accepted",
                r#"{"job_id":"new-1"}"#,
            ),
        ])
        .await;
        job_cmd(
            &api,
            JobCmd::Retry {
                id: "failed".into(),
            },
        )
        .await
        .unwrap();
        job_cmd(
            &api,
            JobCmd::Cancel {
                id: "queued".into(),
            },
        )
        .await
        .unwrap();
        let submitted = run_job(&api, "local.coop/aria", "hello", None)
            .await
            .unwrap();
        assert_eq!(submitted, serde_json::json!({"job_id":"new-1"}));
        let requests = server.await.unwrap();
        assert!(requests.iter().all(|request| request.method == "POST"));
        assert_eq!(
            serde_json::from_str::<Value>(&requests[2].body).unwrap()["prompt"],
            "hello"
        );
    }

    #[tokio::test]
    async fn wait_polls_to_done_and_prints_unchanged_job_json() {
        let (api, server) = mock_api(vec![
            Reply::new(
                "/api/v1/jobs/j",
                "200 OK",
                r#"{"id":"j","status":"QUEUED"}"#,
            ),
            Reply::new(
                "/api/v1/jobs/j",
                "200 OK",
                r#"{"id":"j","status":"RUNNING"}"#,
            ),
            Reply::new(
                "/api/v1/jobs/j",
                "200 OK",
                r#"{"id":"j","status":"DONE","result":"hello"}"#,
            ),
        ])
        .await;
        let job = poll_job_until(
            &api,
            "j",
            Duration::from_millis(5),
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap();
        let mut output = Vec::new();
        write_terminal_job(&job, &mut output).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), job);
        assert_eq!(job["result"], "hello");
        assert_eq!(server.await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn failed_and_cancelled_waits_print_json_before_failing() {
        for status in ["FAILED", "CANCELLED"] {
            let body = serde_json::json!({"id":"j","status":status,"error":"reason"}).to_string();
            let (api, server) = mock_api(vec![Reply::new("/api/v1/jobs/j", "200 OK", &body)]).await;
            let job = poll_job_until(
                &api,
                "j",
                Duration::from_millis(1),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
            let mut output = Vec::new();
            let error = write_terminal_job(&job, &mut output).unwrap_err();
            assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), job);
            assert!(error.to_string().contains(status));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn wait_errors_on_http_failure_or_unknown_status_without_spinning() {
        for (status, body, expected) in [
            ("404 Not Found", r#"{"error":"no such job"}"#, "HTTP 404"),
            ("200 OK", r#"{"status":"UNKNOWN"}"#, "unknown status"),
            (
                "200 OK",
                r#"{"error":"not a job"}"#,
                "missing or unknown status",
            ),
        ] {
            let (api, server) = mock_api(vec![Reply::new("/api/v1/jobs/j", status, body)]).await;
            let error = poll_job_until(
                &api,
                "j",
                Duration::from_secs(1),
                Instant::now() + Duration::from_secs(2),
            )
            .await
            .unwrap_err();
            assert!(format!("{error:#}").contains(expected));
            assert_eq!(server.await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn wait_deadline_includes_headers_and_response_body() {
        for delay_body in [false, true] {
            let mut reply = Reply::new("/api/v1/jobs/j", "200 OK", r#"{"id":"j","status":"DONE"}"#);
            if delay_body {
                reply.body_delay = Duration::from_millis(600);
            } else {
                reply.header_delay = Duration::from_millis(600);
            }
            let (api, server) = mock_api(vec![reply]).await;
            let start = Instant::now();
            let error = poll_job_until(
                &api,
                "j",
                Duration::from_secs(10),
                start + Duration::from_millis(100),
            )
            .await
            .unwrap_err();
            assert!(start.elapsed() < Duration::from_millis(500));
            assert!(error.to_string().contains("timeout waiting"));
            assert!(error.to_string().contains("was not cancelled"));
            server.abort();
            let _ = server.await;
        }
    }

    #[tokio::test]
    async fn wait_deadline_interrupts_sleep_and_handles_an_expired_deadline() {
        let (api, server) = mock_api(vec![Reply::new(
            "/api/v1/jobs/j",
            "200 OK",
            r#"{"status":"RUNNING"}"#,
        )])
        .await;
        let start = Instant::now();
        let error = poll_job_until(
            &api,
            "j",
            Duration::from_secs(u64::MAX),
            start + Duration::from_millis(100),
        )
        .await
        .unwrap_err();
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(error.to_string().contains("timeout waiting"));
        server.await.unwrap();
        assert!(
            poll_job_until(&api, "j", Duration::from_secs(1), Instant::now())
                .await
                .unwrap_err()
                .to_string()
                .contains("timeout waiting")
        );
    }

    #[tokio::test]
    async fn run_wait_uses_one_deadline_for_submission_and_polling() {
        let mut submission = Reply::new(
            "/api/v1/hens/local.coop%2Faria/jobs",
            "202 Accepted",
            r#"{"job_id":"j"}"#,
        );
        submission.header_delay = Duration::from_millis(200);
        let mut completed = Reply::new("/api/v1/jobs/j", "200 OK", r#"{"id":"j","status":"DONE"}"#);
        completed.header_delay = Duration::from_millis(200);
        let (api, server) = mock_api(vec![submission, completed]).await;
        let start = Instant::now();
        let error = run_job(
            &api,
            "local.coop/aria",
            "hello",
            Some(WaitSettings {
                interval: Duration::from_millis(1),
                timeout: Duration::from_millis(300),
            }),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timeout waiting for job `j`"));
        assert!(start.elapsed() < Duration::from_millis(500));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn run_wait_returns_the_terminal_job_and_requires_an_acknowledged_id() {
        let (api, server) = mock_api(vec![
            Reply::new(
                "/api/v1/hens/local.coop%2Faria/jobs",
                "202 Accepted",
                r#"{"job_id":"j"}"#,
            ),
            Reply::new(
                "/api/v1/jobs/j",
                "200 OK",
                r#"{"id":"j","status":"DONE","result":"ready"}"#,
            ),
        ])
        .await;
        let job = run_job(
            &api,
            "local.coop/aria",
            "hello",
            Some(WaitSettings::new(1, 2).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(
            job,
            serde_json::json!({"id":"j","status":"DONE","result":"ready"})
        );
        assert_eq!(server.await.unwrap().len(), 2);

        let (api, server) = mock_api(vec![Reply::new(
            "/api/v1/hens/local.coop%2Faria/jobs",
            "202 Accepted",
            r#"{"ok":true}"#,
        )])
        .await;
        let error = run_job(
            &api,
            "local.coop/aria",
            "hello",
            Some(WaitSettings::new(1, 2).unwrap()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("did not contain a job_id"));
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn run_wait_submission_timeout_reports_unknown_acceptance() {
        let mut submission = Reply::new(
            "/api/v1/hens/local.coop%2Faria/jobs",
            "202 Accepted",
            r#"{"job_id":"j"}"#,
        );
        submission.header_delay = Duration::from_millis(600);
        let (api, server) = mock_api(vec![submission]).await;
        let error = run_job(
            &api,
            "local.coop/aria",
            "hello",
            Some(WaitSettings {
                interval: Duration::from_secs(1),
                timeout: Duration::from_millis(100),
            }),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("acceptance is unknown"));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn configured_request_timeout_bounds_response_body() {
        let mut reply = Reply::new("/api/v1/farm", "200 OK", r#"{"hen_count":0}"#);
        reply.body_delay = Duration::from_secs(3);
        let (api, server) = mock_api(vec![reply]).await;
        let api = ApiClient::new(&api.base, ApiToken::default(), 1).unwrap();
        let start = Instant::now();
        let error = api.get("/api/v1/farm").await.unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(error.to_string().contains("timed out"));
        server.abort();
        let _ = server.await;
    }

    fn doctor_replies() -> Vec<Reply> {
        vec![
            Reply::new("/api/v1/healthz", "200 OK", r#"{"ok":true}"#),
            Reply::new("/api/v1/readyz", "200 OK", r#"{"ok":true}"#),
            Reply::new(
                "/api/v1/farm",
                "200 OK",
                r#"{"coop_id":"local.coop","hen_count":0,"coopd_version":"test"}"#,
            ),
            Reply::new("/api/v1/vault/status", "200 OK", r#"{"unlocked":false}"#),
            Reply::new(
                "/api/v1/session/capabilities",
                "200 OK",
                r#"{"shell":true,"persistent_session":false,"task_dispatch":false,"backend":"plain_pty","note":"test backend"}"#,
            ),
        ]
    }

    #[tokio::test]
    async fn doctor_reports_real_optional_capabilities_and_enforces_requirements() {
        for required in [false, true] {
            let (api, server) = mock_api(doctor_replies()).await;
            let report = doctor_report(&api, required, required).await;
            assert_eq!(report["ok"], !required);
            assert_eq!(report["client_version"], env!("CARGO_PKG_VERSION"));
            assert_eq!(
                report["checks"]["farm"]["response"]["coopd_version"],
                "test"
            );
            assert_eq!(report["checks"]["vault"]["response"]["unlocked"], false);
            assert_eq!(
                report["checks"]["sessions"]["response"]["backend"],
                "plain_pty"
            );
            let mut output = Vec::new();
            assert_eq!(write_doctor_report(&report, &mut output).is_ok(), !required);
            assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), report);
            assert_eq!(server.await.unwrap().len(), 5);
        }
    }

    #[tokio::test]
    async fn doctor_fails_for_unavailable_readiness_or_unauthorized_optional_probe() {
        for index in [1, 3] {
            let mut replies = doctor_replies();
            replies[index].status = if index == 1 {
                "503 Service Unavailable"
            } else {
                "401 Unauthorized"
            };
            replies[index].body = r#"{"error":"probe failed"}"#.to_string();
            let (api, server) = mock_api(replies).await;
            let report = doctor_report(&api, false, false).await;
            assert_eq!(report["ok"], false);
            let name = if index == 1 { "readiness" } else { "vault" };
            assert!(
                report["checks"][name]["error"]
                    .as_str()
                    .unwrap()
                    .contains("probe failed")
            );
            assert!(write_doctor_report(&report, Vec::new()).is_err());
            server.await.unwrap();
        }
    }
}
