//! Local HTTP API exposed by `coopd`.

use axum::{
    Json, Router,
    extract::{Path, Query, State, WebSocketUpgrade, ws::Message as WsMsg, ws::WebSocket},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use coopd_core::{
    AgentKind, AgentManifest, Delegator, Hen, HenId, HenState, Job, JobQuery, JobStatus,
    MemoryEntry, Task,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::discord_supervisor::{DiscordConfig, DiscordSupervisor};
pub(crate) use crate::execution_policy::enforce_lease_topic;
use crate::execution_policy::{check_prompt_len, ensure_hatch_supported};
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
        .route("/api/v1/session/capabilities", get(session_capabilities))
        .route("/api/v1/farm", get(farm))
        .route("/api/v1/hens", get(list_hens).post(create_hen))
        .route("/api/v1/hens/preflight", post(preflight_hen))
        .route("/api/v1/hens/:id", get(get_hen).delete(delete_hen))
        .route("/api/v1/hens/:id/hatch", post(hatch_hen))
        .route("/api/v1/hens/:id/sleep", post(sleep_hen))
        .route("/api/v1/hens/:id/wake", post(wake_hen))
        .route("/api/v1/hens/:id/jobs", post(submit_job))
        .route("/api/v1/hens/:id/delegate", post(delegate_hen))
        .route(
            "/api/v1/hens/:id/memory",
            get(get_hen_memory).delete(forget_hen_memory),
        )
        .route("/api/v1/hens/:id/shell/send", post(shell_send))
        .route("/api/v1/jobs", get(list_jobs))
        .route("/api/v1/jobs/:id", get(get_job))
        .route("/api/v1/jobs/:id/cancel", post(cancel_job))
        .route("/api/v1/jobs/:id/retry", post(retry_job))
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

async fn readyz(State(orch): State<OrchHandle>) -> Result<Json<OkBody>, AppError> {
    match tokio::time::timeout(std::time::Duration::from_secs(2), orch.list_hens(None)).await {
        Ok(Ok(_)) => Ok(Json(OkBody { ok: true })),
        _ => Err(AppError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "orchestrator is unavailable".into(),
        }),
    }
}

async fn session_capabilities() -> Json<crate::session::SessionCapabilities> {
    Json(crate::session::capabilities())
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

async fn preflight_hen(
    State(orch): State<OrchHandle>,
    body: String,
) -> Result<Json<OkBody>, AppError> {
    let manifest = AgentManifest::parse_yaml(&body)
        .map_err(|e| AppError::bad_request(format!("invalid manifest: {e}")))?;
    let factory = orch.brain_factory.lock().await;
    factory
        .build(&manifest)
        .await
        .map_err(|e| AppError::unprocessable(format!("provider preflight failed: {e}")))?;
    Ok(Json(OkBody { ok: true }))
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
    let hen = orch.get_hen(id.clone()).await?;
    ensure_hatch_supported(&hen)?;
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
    let job_id = orch.submit_job(id, body.prompt).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_id": job_id })),
    ))
}

#[derive(Deserialize)]
struct DelegateBody {
    to: String,
    prompt: String,
}

async fn delegate_hen(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
    Json(body): Json<DelegateBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let from = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    let to = HenId::parse(&body.to).map_err(|e| AppError::bad_request(e.to_string()))?;
    check_prompt_len(&body.prompt)?;
    if body.prompt.trim().is_empty() {
        return Err(AppError::bad_request("prompt is empty"));
    }
    // Pre-validate for a clean 400 (self-delegation / depth). The orchestrator
    // re-validates authoritatively. A top-level delegation runs at depth 1.
    coopd_core::validate_delegation(&from, &to, 1)
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    let outcome = Delegator::delegate(
        &orch,
        coopd_core::DelegationRequest {
            from,
            to: to.clone(),
            prompt: body.prompt,
            parent_depth: 0,
            timeout: delegate_api_timeout(),
        },
    )
    .await?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "hen": to.to_string(),
            "job_id": outcome.job_id,
            "status": format!("{:?}", outcome.status),
            "output": outcome.output,
        })),
    ))
}

/// Effective wait for an API-initiated delegation, from
/// `COOP_DELEGATE_TIMEOUT_SECS` (default 180s).
fn delegate_api_timeout() -> std::time::Duration {
    let secs = std::env::var("COOP_DELEGATE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(180);
    std::time::Duration::from_secs(secs)
}

#[derive(Default, Deserialize)]
struct ListJobsQ {
    hen_id: Option<String>,
    status: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
    #[serde(default)]
    offset: usize,
    order: Option<String>,
}

impl ListJobsQ {
    fn into_query(self) -> Result<JobQuery, AppError> {
        let hen_id = self
            .hen_id
            .as_deref()
            .map(HenId::parse)
            .transpose()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        let status = self
            .status
            .map(|status| match status.trim().to_ascii_uppercase().as_str() {
                "QUEUED" => Ok(JobStatus::Queued),
                "RUNNING" => Ok(JobStatus::Running),
                "DONE" => Ok(JobStatus::Done),
                "FAILED" => Ok(JobStatus::Failed),
                "CANCELLED" => Ok(JobStatus::Cancelled),
                _ => Err(AppError::bad_request(format!(
                    "unknown job status: {status}"
                ))),
            })
            .transpose()?;
        if self.limit.is_some_and(|limit| !(1..=500).contains(&limit)) {
            return Err(AppError::bad_request("limit must be between 1 and 500"));
        }
        let search = self
            .q
            .map(|q| q.trim().to_string())
            .filter(|q| !q.is_empty());
        if search.as_ref().is_some_and(|q| q.len() > 256) {
            return Err(AppError::bad_request("q must be at most 256 UTF-8 bytes"));
        }
        let descending = match self
            .order
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("asc") => false,
            Some("desc") => true,
            _ => return Err(AppError::bad_request("order must be asc or desc")),
        };
        Ok(JobQuery {
            hen_id,
            status,
            search,
            limit: self.limit,
            offset: self.offset,
            descending,
        })
    }
}

async fn list_jobs(
    State(orch): State<OrchHandle>,
    Query(q): Query<ListJobsQ>,
) -> Result<Json<Vec<Job>>, AppError> {
    Ok(Json(orch.query_jobs(q.into_query()?).await?))
}

async fn get_job(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<Json<Job>, AppError> {
    Ok(Json(orch.get_job(id).await?))
}

async fn cancel_job(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<Json<Job>, AppError> {
    Ok(Json(orch.cancel_job(id).await?))
}

async fn retry_job(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let job_id = orch.retry_job(id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_id": job_id })),
    ))
}

#[derive(Deserialize)]
struct MemoryQ {
    limit: Option<usize>,
}

async fn get_hen_memory(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
    Query(q): Query<MemoryQ>,
) -> Result<Json<Vec<MemoryEntry>>, AppError> {
    let id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Json(orch.load_memories(id, q.limit).await?))
}

#[derive(Serialize)]
struct ForgetResult {
    forgotten: usize,
}

async fn forget_hen_memory(
    State(orch): State<OrchHandle>,
    Path(id): Path<String>,
) -> Result<Json<ForgetResult>, AppError> {
    let id = HenId::parse(&id).map_err(|e| AppError::bad_request(e.to_string()))?;
    let forgotten = orch.forget_memories(id).await?;
    Ok(Json(ForgetResult { forgotten }))
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
        let workdir = orch.workdir_base.join(hen_id.workdir_key());
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
    check_prompt_len(&body.prompt)?;
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
    fn unprocessable(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: msg.into(),
        }
    }
}

impl From<coopd_core::CoreError> for AppError {
    fn from(e: coopd_core::CoreError) -> Self {
        use coopd_core::CoreError as E;
        let status = match &e {
            E::HenNotFound(_) | E::JobNotFound(_) => StatusCode::NOT_FOUND,
            E::Conflict(_) => StatusCode::CONFLICT,
            E::PermissionDenied(_) => StatusCode::FORBIDDEN,
            E::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            E::InvalidId(_)
            | E::InvalidManifest(_)
            | E::InvalidTransition { .. }
            | E::InvalidInput(_) => StatusCode::BAD_REQUEST,
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[test]
    fn job_query_validates_bounds_and_keeps_legacy_defaults() {
        let legacy = ListJobsQ::default().into_query().unwrap();
        assert_eq!(legacy.limit, None);
        assert_eq!(legacy.offset, 0);
        assert!(!legacy.descending);
        let query: ListJobsQ = serde_json::from_value(serde_json::json!({
            "hen_id": "local.coop/aria", "status": "failed", "q": " repair ",
            "limit": 50, "offset": 100, "order": "desc"
        }))
        .unwrap();
        let query = query.into_query().unwrap();
        assert_eq!(query.status, Some(JobStatus::Failed));
        assert_eq!(query.search.as_deref(), Some("repair"));
        assert!(query.descending);
        for value in [
            serde_json::json!({ "limit": 0 }),
            serde_json::json!({ "limit": 501 }),
            serde_json::json!({ "status": "complete" }),
            serde_json::json!({ "order": "random" }),
            serde_json::json!({ "q": "x".repeat(257) }),
            serde_json::json!({ "hen_id": "missing-separator" }),
        ] {
            let query: ListJobsQ = serde_json::from_value(value).unwrap();
            assert_eq!(
                query.into_query().unwrap_err().status,
                StatusCode::BAD_REQUEST
            );
        }
    }

    async fn request(app: &Router, method: &str, path: &str) -> (StatusCode, serde_json::Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value = if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        };
        (status, value)
    }

    #[tokio::test]
    async fn job_routes_preserve_response_shapes_and_report_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let store = coopd_storage::Store::open(dir.path().join("api.redb")).unwrap();
        let hen_id = HenId::parse("local.coop/aria").unwrap();
        let mut hen = Hen::new(hen_id.clone(), AgentManifest::minimal("aria".into()));
        hen.state = HenState::Working;
        store.put_hen(&hen).unwrap();
        let mut running = Job::new(hen_id.clone(), "active".into());
        running.mark_running();
        let queued = Job::new(hen_id, "queued repair".into());
        store.put_job(&running).unwrap();
        store.put_job(&queued).unwrap();
        let orch = crate::orchestrator::spawn(
            store.clone(),
            Arc::new(coopd_tools::Registry::new()),
            Arc::new(tokio::sync::Mutex::new(
                crate::brain_factory::BrainFactory::new(None),
            )),
            dir.path().join("workdirs"),
        );
        let discord =
            DiscordSupervisor::new(dir.path(), "local.coop", "http://127.0.0.1:9700".into());
        let app = router(
            orch.clone(),
            discord,
            TaskService::new(orch.clone()),
            "127.0.0.1:9700".into(),
        );
        assert_eq!(
            request(&app, "GET", "/api/v1/readyz").await.0,
            StatusCode::OK
        );
        let (status, page) = request(
            &app,
            "GET",
            "/api/v1/jobs?status=queued&q=REPAIR&limit=1&order=desc",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page.as_array().unwrap().len(), 1);
        assert_eq!(page[0]["id"], queued.id);

        let cancel = format!("/api/v1/jobs/{}/cancel", queued.id);
        let (status, cancelled) = request(&app, "POST", &cancel).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cancelled["status"], "CANCELLED");
        assert_eq!(request(&app, "POST", &cancel).await.1, cancelled);
        let retry = format!("/api/v1/jobs/{}/retry", queued.id);
        let (status, result) = request(&app, "POST", &retry).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let retried = store.get_job(result["job_id"].as_str().unwrap()).unwrap();
        assert_eq!(retried.retry_of.as_deref(), Some(queued.id.as_str()));
        assert_eq!(retried.status, JobStatus::Queued);

        for path in [
            format!("/api/v1/jobs/{}/cancel", running.id),
            format!("/api/v1/jobs/{}/retry", running.id),
            "/api/v1/hens/local.coop%2Faria/sleep".into(),
        ] {
            assert_eq!(request(&app, "POST", &path).await.0, StatusCode::CONFLICT);
        }
        let (status, missing) = request(&app, "GET", "/api/v1/jobs/missing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(missing["error"].as_str().unwrap().contains("job not found"));
        assert_eq!(
            request(&app, "GET", "/api/v1/jobs?limit=0").await.0,
            StatusCode::BAD_REQUEST,
        );
        orch.shutdown().await;
        assert_eq!(
            request(&app, "GET", "/api/v1/readyz").await.0,
            StatusCode::SERVICE_UNAVAILABLE,
        );
    }
}
