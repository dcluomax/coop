//! Local HTTP API exposed by `coopd`.

use axum::{
    Json, Router,
    extract::{Path, Query, State, WebSocketUpgrade, ws::Message as WsMsg, ws::WebSocket},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use coopd_core::{AgentKind, AgentManifest, Hen, HenId, HenState, Job, Task};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::discord_supervisor::{DiscordConfig, DiscordSupervisor};
use crate::location;
use crate::orchestrator::OrchHandle;
use crate::tasks::TaskService;

/// Build the HTTP router.
pub fn router(
    orch: OrchHandle,
    discord: DiscordSupervisor,
    tasks: TaskService,
    bound_addr: String,
) -> Router {
    let discord_routes = Router::new()
        .route(
            "/api/v1/config/discord",
            get(get_discord_config).put(put_discord_config),
        )
        .with_state(discord);
    let location_routes = Router::new().route(
        "/api/v1/farm/location",
        get(move || {
            let addr = bound_addr.clone();
            async move { Json(location::compute(&addr)) }
        }),
    );
    let task_routes = Router::new()
        .route("/api/v1/tasks", get(list_tasks).post(submit_task))
        .route("/api/v1/tasks/:id/done", post(mark_task_done))
        .route("/api/v1/tasks/:id", axum::routing::delete(cancel_task))
        .with_state(tasks);
    Router::new()
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/readyz", get(readyz))
        .route("/api/v1/config/market", get(get_market_config))
        .route("/api/v1/session/capabilities", get(session_capabilities))
        .route("/api/v1/farm", get(farm))
        .route("/api/v1/hens", get(list_hens).post(create_hen))
        .route("/api/v1/hens/:id", get(get_hen).delete(delete_hen))
        .route("/api/v1/hens/:id/hatch", post(hatch_hen))
        .route("/api/v1/hens/:id/sleep", post(sleep_hen))
        .route("/api/v1/hens/:id/wake", post(wake_hen))
        .route("/api/v1/hens/:id/jobs", post(submit_job))
        .route("/api/v1/hens/:id/shell/send", post(shell_send))
        .route("/api/v1/jobs", get(list_jobs))
        .route("/api/v1/jobs/:id", get(get_job))
        .route("/api/v1/vault/unlock", post(vault_unlock))
        .route("/api/v1/vault/status", get(vault_status))
        .route(
            "/api/v1/vault/secrets",
            get(vault_list_secrets).put(vault_put_secret),
        )
        .route("/api/v1/watch", get(watch))
        .route("/api/v1/hens/:id/shell", get(crate::shell::shell))
        .with_state(orch)
        .merge(discord_routes)
        .merge(location_routes)
        .merge(task_routes)
}

async fn get_discord_config(State(s): State<DiscordSupervisor>) -> Json<DiscordConfig> {
    Json(s.snapshot().await)
}

async fn put_discord_config(
    State(s): State<DiscordSupervisor>,
    Json(body): Json<DiscordConfig>,
) -> Result<Json<DiscordConfig>, AppError> {
    let applied = s
        .apply(body)
        .await
        .map_err(|e| AppError::bad_request(format!("{e:#}")))?;
    Ok(Json(applied))
}

#[allow(dead_code)]
fn _location_marker() {}

#[derive(Serialize)]
struct OkBody {
    ok: bool,
}

async fn healthz() -> impl IntoResponse {
    Json(OkBody { ok: true })
}

async fn readyz(State(_orch): State<OrchHandle>) -> impl IntoResponse {
    Json(OkBody { ok: true })
}

async fn session_capabilities() -> Json<crate::session::SessionCapabilities> {
    Json(crate::session::capabilities())
}

#[derive(Serialize)]
struct MarketConfig {
    public_url: String,
}

async fn get_market_config() -> Json<MarketConfig> {
    let public_url = std::env::var("COOP_MARKET_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://farm.startcaas.com".to_string());
    Json(MarketConfig { public_url })
}

#[derive(Serialize)]
struct FarmInfo {
    coop_id: String,
    coopd_version: &'static str,
    hen_count: usize,
}

async fn farm(State(orch): State<OrchHandle>) -> Result<Json<FarmInfo>, AppError> {
    let hens = orch.list_hens(None).await?;
    Ok(Json(FarmInfo {
        coop_id: orch.coop_id.to_string(),
        coopd_version: env!("CARGO_PKG_VERSION"),
        hen_count: hens.len(),
    }))
}

#[derive(Deserialize)]
struct ListQuery {
    state: Option<String>,
}

async fn list_hens(
    State(orch): State<OrchHandle>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Hen>>, AppError> {
    let state = parse_state_filter(q.state.as_deref())?;
    let hens = orch.list_hens(state).await?;
    Ok(Json(hens))
}

fn parse_state_filter(s: Option<&str>) -> Result<Option<HenState>, AppError> {
    Ok(match s {
        Some("DEFINED") => Some(HenState::Defined),
        Some("HATCHING") => Some(HenState::Hatching),
        Some("IDLE") => Some(HenState::Idle),
        Some("WORKING") => Some(HenState::Working),
        Some("LEASED") => Some(HenState::Leased),
        Some("SLEEPING") => Some(HenState::Sleeping),
        Some("DORMANT") => Some(HenState::Dormant),
        Some("ARCHIVED") => Some(HenState::Archived),
        Some(other) => return Err(AppError::bad_request(format!("unknown state: {other}"))),
        None => None,
    })
}

async fn create_hen(
    State(orch): State<OrchHandle>,
    body: String,
) -> Result<(StatusCode, Json<HenId>), AppError> {
    let manifest = AgentManifest::parse_yaml(&body)
        .map_err(|e| AppError::bad_request(format!("invalid manifest: {e}")))?;
    let id = orch.create_hen(manifest).await?;
    Ok((StatusCode::CREATED, Json(id)))
}

async fn get_hen(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<Json<Hen>, AppError> {
    let id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    let hen = orch.get_hen(id).await?;
    Ok(Json(hen))
}

async fn delete_hen(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    orch.delete_hen(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn hatch_hen(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<Json<OkBody>, AppError> {
    let id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    orch.transition_hen(id.clone(), HenState::Hatching).await?;
    orch.transition_hen(id, HenState::Idle).await?;
    Ok(Json(OkBody { ok: true }))
}

async fn sleep_hen(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<Json<OkBody>, AppError> {
    let id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    orch.transition_hen(id, HenState::Sleeping).await?;
    Ok(Json(OkBody { ok: true }))
}

async fn wake_hen(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<Json<OkBody>, AppError> {
    let id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    orch.transition_hen(id, HenState::Idle).await?;
    Ok(Json(OkBody { ok: true }))
}

#[derive(Deserialize)]
struct JobBody {
    prompt: String,
}

async fn submit_job(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
    Json(body): Json<JobBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    // Topic filter: if the hen is currently leased and the manifest defines
    // a topic_filter, every prompt must pass it before dispatch.
    if let Ok(hen) = orch.get_hen(id.clone()).await {
        if let Err(reason) = enforce_lease_topic(&hen, &body.prompt) {
            return Err(AppError::forbidden(reason));
        }
    }
    let job_id = orch.submit_job(id, body.prompt).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_id": job_id })),
    ))
}

/// Returns `Err(reason)` if `prompt` violates the active lease policy.
pub(crate) fn enforce_lease_topic(
    hen: &coopd_core::Hen,
    prompt: &str,
) -> std::result::Result<(), String> {
    use coopd_core::LeaseStatus;
    let leased = !matches!(hen.lease, LeaseStatus::Owner);
    if !leased {
        return Ok(());
    }
    if let Some(tf) = hen
        .manifest
        .lease
        .as_ref()
        .and_then(|l| l.topic_filter.as_ref())
    {
        tf.check(prompt)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct ListJobsQ {
    hen_id: Option<String>,
}

async fn list_jobs(
    State(orch): State<OrchHandle>,
    Query(q): Query<ListJobsQ>,
) -> Result<Json<Vec<Job>>, AppError> {
    let hen_id = q
        .hen_id
        .as_deref()
        .map(HenId::parse)
        .transpose()
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Json(orch.list_jobs(hen_id).await?))
}

async fn get_job(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<Json<Job>, AppError> {
    Ok(Json(orch.get_job(id).await?))
}

#[derive(Deserialize)]
struct VaultUnlockBody {
    path: String,
    passphrase: String,
}

async fn vault_unlock(
    State(orch): State<OrchHandle>,
    Json(body): Json<VaultUnlockBody>,
) -> Result<Json<OkBody>, AppError> {
    let vault = coopd_vault::Vault::open(&body.path, &body.passphrase)
        .map_err(|e| AppError::bad_request(format!("vault unlock: {e}")))?;
    orch.brain_factory.lock().await.set_vault(vault);
    Ok(Json(OkBody { ok: true }))
}

#[derive(Serialize)]
struct VaultStatus {
    unlocked: bool,
}

async fn vault_status(State(orch): State<OrchHandle>) -> Json<VaultStatus> {
    let bf = orch.brain_factory.lock().await;
    Json(VaultStatus {
        unlocked: bf.is_unlocked(),
    })
}

#[derive(Serialize)]
struct VaultSecrets {
    names: Vec<String>,
}

async fn vault_list_secrets(State(orch): State<OrchHandle>) -> Json<VaultSecrets> {
    let bf = orch.brain_factory.lock().await;
    Json(VaultSecrets {
        names: bf.vault_list(),
    })
}

#[derive(Deserialize)]
struct VaultPutBody {
    name: String,
    value: String,
}

async fn vault_put_secret(
    State(orch): State<OrchHandle>,
    Json(body): Json<VaultPutBody>,
) -> Result<Json<OkBody>, AppError> {
    if body.name.is_empty() || body.value.is_empty() {
        return Err(AppError::bad_request("name and value are required"));
    }
    orch.brain_factory
        .lock()
        .await
        .vault_put(&body.name, &body.value)
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Json(OkBody { ok: true }))
}

#[derive(Deserialize)]
struct ShellSendBody {
    /// Text to inject (no trailing newline needed; Enter is sent automatically
    /// unless `no_enter` is true).
    keys: String,
    #[serde(default)]
    no_enter: bool,
}

async fn shell_send(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
    Json(body): Json<ShellSendBody>,
) -> Result<Json<OkBody>, AppError> {
    let hen_id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    if body.no_enter {
        // Special path: raw send-keys without Enter; useful for slash commands
        // that need precise key handling. Still attaches to an existing
        // session only — we don't auto-create here because the typical caller
        // is the in-tmux UI.
        if !crate::session::tmux_available() {
            return Err(AppError::bad_request(
                "raw shell/send requires a persistent tmux session; native Windows currently supports only ephemeral PTY shells",
            ));
        }
        let sess = crate::session::tmux_session_name(&hen_id);
        let workdir = orch.workdir_base.join(hen_id.name());
        let tmux_dir = crate::session::tmux_socket_dir(&sess, &workdir);
        crate::session::ensure_tmux_socket_dir(&tmux_dir)
            .map_err(|e| AppError::bad_request(format!("mkdir tmux socket dir: {e}")))?;
        let keys = body.keys;
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let status = std::process::Command::new("tmux")
                .env("TMUX_TMPDIR", &tmux_dir)
                .args(["-L", "coop", "send-keys", "-t", &sess, &keys])
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other(format!(
                    "tmux send-keys exited with {status}"
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| AppError::bad_request(format!("send-keys join: {e}")))?
        .map_err(|e| AppError::bad_request(format!("send-keys: {e}")))?;
    } else {
        // Standard path: ensure the session exists (creates + auto-launches
        // the CLI if needed) then send the keys + Enter.
        crate::tasks::send_keys_to_hen(&orch, &hen_id, &body.keys)
            .await
            .map_err(|e| AppError::bad_request(format!("send-keys: {e}")))?;
    }
    Ok(Json(OkBody { ok: true }))
}

#[derive(Deserialize)]
struct TaskBody {
    prompt: String,
    #[serde(default)]
    required_agent_kind: Option<AgentKind>,
}

async fn submit_task(
    State(svc): State<TaskService>,
    Json(body): Json<TaskBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    if body.prompt.trim().is_empty() {
        return Err(AppError::bad_request("prompt is empty"));
    }
    let id = svc.submit(body.prompt, body.required_agent_kind).await;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "task_id": id })),
    ))
}

async fn list_tasks(State(svc): State<TaskService>) -> Json<Vec<Task>> {
    Json(svc.list().await)
}

async fn mark_task_done(
    State(svc): State<TaskService>,
    Path(id): Path<String>,
) -> Result<Json<OkBody>, AppError> {
    if svc.mark_done(&id).await {
        Ok(Json(OkBody { ok: true }))
    } else {
        Err(AppError::bad_request("task not found"))
    }
}

async fn cancel_task(
    State(svc): State<TaskService>,
    Path(id): Path<String>,
) -> Result<Json<OkBody>, AppError> {
    if svc.cancel(&id).await {
        Ok(Json(OkBody { ok: true }))
    } else {
        Err(AppError::bad_request("task not pending"))
    }
}

async fn watch(ws: WebSocketUpgrade, State(orch): State<OrchHandle>) -> impl IntoResponse {
    // H6: /watch is purely server→client; clients only send pings/closes.
    let ws = ws.max_message_size(64 * 1024).max_frame_size(64 * 1024);
    ws.on_upgrade(move |socket| handle_ws(socket, orch))
}

async fn handle_ws(mut socket: WebSocket, orch: OrchHandle) {
    let mut rx = orch.events.subscribe();
    debug!("ws subscriber attached");
    loop {
        tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Ok(event) => {
                        let Ok(payload) = serde_json::to_string(&event) else {
                            continue;
                        };
                        if socket.send(WsMsg::Text(payload)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(WsMsg::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
    debug!("ws subscriber detached");
}

/// Unified error envelope.
#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: msg.into(),
        }
    }
}

impl From<coopd_core::CoreError> for AppError {
    fn from(e: coopd_core::CoreError) -> Self {
        use coopd_core::CoreError as E;
        let status = match &e {
            E::HenNotFound(_) => StatusCode::NOT_FOUND,
            E::InvalidId(_) | E::InvalidManifest(_) | E::InvalidTransition { .. } => {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: e.to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        if self.status.is_server_error() {
            warn!(status = %self.status, message = %self.message, "server error");
        }
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}
