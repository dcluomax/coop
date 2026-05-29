//! Farm-wide task queue.
//!
//! See [`coopd_core::Task`] for the data model. This module owns the
//! in-memory registry + dispatcher. Tasks are routed to **CLI-agent
//! hens** by piping `prompt + Enter` into their persistent session backend.
//! On Unix/macOS this is tmux; on native Windows the browser shell is
//! currently ephemeral and task dispatch returns a clear unsupported error.
//! No task persistence yet — the queue lives for the daemon's lifetime; can
//! be swapped for redb later.

use std::collections::HashMap;
use std::sync::Arc;

use coopd_core::{AgentKind, HenId, Task, TaskStatus};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::orchestrator::OrchHandle;
use crate::session;

/// Clonable handle to the in-memory task registry.
#[derive(Clone)]
pub struct TaskService {
    inner: Arc<Mutex<HashMap<String, Task>>>,
    orch: OrchHandle,
}

impl TaskService {
    /// Construct.
    #[must_use]
    pub fn new(orch: OrchHandle) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            orch,
        }
    }

    /// Submit a task and immediately try to dispatch it. Returns the task id.
    pub async fn submit(&self, prompt: String, required: Option<AgentKind>) -> String {
        let mut task = Task::new(prompt, required);
        let id = task.id.clone();

        if let Some(hen_id) = self.find_match(required).await {
            if let Err(e) = send_keys_to_hen(&self.orch, &hen_id, &task.prompt).await {
                warn!(?e, hen = %hen_id, "task dispatch send-keys failed");
            } else {
                task.mark_dispatched(hen_id.clone());
                info!(task = %id, hen = %hen_id, "task dispatched");
            }
        } else {
            info!(task = %id, ?required, "task queued (no matching hen)");
        }

        self.inner.lock().await.insert(id.clone(), task);
        id
    }

    /// List all tasks (newest first).
    pub async fn list(&self) -> Vec<Task> {
        let g = self.inner.lock().await;
        let mut v: Vec<Task> = g.values().cloned().collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        v
    }

    /// Mark a pending or dispatched task done.
    pub async fn mark_done(&self, id: &str) -> bool {
        let mut g = self.inner.lock().await;
        if let Some(t) = g.get_mut(id) {
            t.mark_done();
            true
        } else {
            false
        }
    }

    /// Cancel a pending task (no-op for already-dispatched ones).
    pub async fn cancel(&self, id: &str) -> bool {
        let mut g = self.inner.lock().await;
        if let Some(t) = g.get_mut(id) {
            if matches!(t.status, TaskStatus::Pending) {
                t.mark_cancelled();
                return true;
            }
        }
        false
    }

    /// Re-attempt dispatch of all pending tasks. Called after vault unlock
    /// or any other "world state changed" event.
    #[allow(dead_code)]
    pub async fn redispatch_pending(&self) {
        let mut to_dispatch: Vec<(String, Option<AgentKind>, String)> = vec![];
        {
            let g = self.inner.lock().await;
            for (id, t) in g.iter() {
                if matches!(t.status, TaskStatus::Pending) {
                    to_dispatch.push((id.clone(), t.required_agent_kind, t.prompt.clone()));
                }
            }
        }
        for (id, kind, prompt) in to_dispatch {
            if let Some(hen_id) = self.find_match(kind).await {
                if send_keys_to_hen(&self.orch, &hen_id, &prompt).await.is_ok() {
                    let mut g = self.inner.lock().await;
                    if let Some(t) = g.get_mut(&id) {
                        t.mark_dispatched(hen_id);
                    }
                }
            }
        }
    }

    /// Find a hen whose manifest declares the matching `AgentKind`. If
    /// `required` is `None`, accept any tmux-driven CLI agent. Picks the
    /// first match in the list (stable order by id).
    async fn find_match(&self, required: Option<AgentKind>) -> Option<HenId> {
        let hens = self.orch.list_hens(None).await.ok()?;
        let mut candidates: Vec<&coopd_core::Hen> = hens
            .iter()
            .filter(|h| {
                let k = h.manifest.agent_kind;
                match required {
                    Some(r) => k == r,
                    None => k.is_tmux_agent() && k != AgentKind::Shell,
                }
            })
            .collect();
        candidates.sort_by_key(|a| a.id.to_string());
        candidates.first().map(|h| h.id.clone())
    }
}

/// Inject `text\n` into the hen's persistent tmux session.
/// If the session doesn't exist yet, create it detached (so tasks can be
/// dispatched to hens that nobody has attached to via the browser yet),
/// auto-launch the configured CLI, then send-keys the prompt.
pub async fn send_keys_to_hen(
    orch: &OrchHandle,
    hen_id: &HenId,
    text: &str,
) -> std::io::Result<()> {
    // Topic filter: leased hens enforce their owner's allow/deny keywords
    // on every prompt that flows through send-keys (covers both the task
    // queue and the /shell/send endpoint).
    if let Ok(hen) = orch.get_hen(hen_id.clone()).await {
        if let Err(reason) = crate::api::enforce_lease_topic(&hen, text) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                reason,
            ));
        }
    }
    if !session::tmux_available() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "persistent task dispatch requires tmux; native Windows currently supports only ephemeral PTY shells (use WSL+tmux for persistent sessions)",
        ));
    }

    let sess_name = session::tmux_session_name(hen_id);
    let workdir = orch.workdir_base.join(hen_id.workdir_key());
    let tmux_dir = session::tmux_socket_dir(&sess_name, &workdir);
    // Ensure the workdir + tmux socket dir exist before we shell out.
    let _ = tokio::fs::create_dir_all(&workdir).await;
    session::ensure_tmux_socket_dir(&tmux_dir)?;

    // Look up the hen so we can auto-launch its CLI if we're creating the
    // session for the first time.
    let cli_to_launch = orch
        .get_hen(hen_id.clone())
        .await
        .ok()
        .and_then(|h| h.manifest.agent_kind.launch_cmd().map(str::to_string));
    let had_cli = cli_to_launch.is_some();

    let hen_key = hen_id.to_string();
    let already_launched = {
        let g = orch.auto_launched.lock().await;
        g.contains(&hen_key)
    };

    let text = text.to_string();
    let workdir_str = workdir.to_string_lossy().to_string();
    let user_shell = session::default_shell();

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let base = || {
            let mut c = std::process::Command::new("tmux");
            c.env("TMUX_TMPDIR", &tmux_dir).args(["-L", "coop"]);
            c
        };
        // Has session?
        let has = base()
            .args(["has-session", "-t", &sess_name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !has {
            // Create detached session in the workdir.
            let status = base()
                .args([
                    "new-session",
                    "-d",
                    "-s",
                    &sess_name,
                    "-c",
                    &workdir_str,
                    &user_shell,
                ])
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other(format!(
                    "tmux new-session exited with {status}"
                )));
            }
            if let Some(cli) = &cli_to_launch {
                let _ = base()
                    .args(["send-keys", "-t", &sess_name, cli, "Enter"])
                    .status();
                // Give the CLI a moment to come up before we feed the prompt.
                std::thread::sleep(std::time::Duration::from_millis(800));
            }
        }
        let status = base()
            .args(["send-keys", "-t", &sess_name, &text, "Enter"])
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "tmux send-keys exited with {status}"
            )));
        }
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)??;

    // Record that the CLI has been launched so the browser shell.rs path
    // doesn't double-launch it on first attach.
    if !already_launched && had_cli {
        orch.auto_launched.lock().await.insert(hen_key);
    }
    Ok(())
}
