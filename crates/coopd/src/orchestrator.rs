//! Orchestrator: single Tokio task owning farm state.

use std::sync::Arc;

use coopd_core::{
    AgentManifest, CoopId, CoreError, DelegationOutcome, DelegationRequest, Delegator, Hen, HenId,
    HenState, Job, JobStatus, MemoryEntry, OrchCmd, OrchEvent, Result as CoreResult,
    validate_delegation,
};
use coopd_storage::Store;
use coopd_tools::Registry;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio::time::{Duration, interval};
use tracing::{debug, info, warn};

use crate::brain_factory::BrainFactory;
use crate::runner;

/// Handle returned by [`spawn`]. Cloneable.
#[derive(Debug, Clone)]
pub struct OrchHandle {
    cmd_tx: mpsc::Sender<OrchCmd>,
    /// Broadcast for subscribers (used by WSS /watch).
    pub events: broadcast::Sender<OrchEvent>,
    /// The Coop ID this daemon represents.
    pub coop_id: CoopId,
    /// Shared brain factory (vault-aware).
    pub brain_factory: Arc<Mutex<BrainFactory>>,
    /// Per-hen workdir root: `<workdir_base>/<hen.workdir_key()>/`
    /// (unique per instance: `<sanitized-coop>__<name>`).
    pub workdir_base: std::path::PathBuf,
    /// Hens for whom the configured CLI agent has been auto-launched
    /// in their persistent tmux session already. Lives for the daemon's
    /// lifetime so reconnects don't re-launch the CLI.
    pub auto_launched: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl OrchHandle {
    /// Send a command and await its reply.
    pub async fn send(&self, cmd: OrchCmd) -> CoreResult<()> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|_| CoreError::Other("orchestrator channel closed".into()))
    }

    /// Create a Hen.
    pub async fn create_hen(&self, manifest: AgentManifest) -> CoreResult<HenId> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::CreateHen {
            manifest,
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Get a Hen.
    pub async fn get_hen(&self, id: HenId) -> CoreResult<Hen> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::GetHen { id, reply: tx }).await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// List Hens.
    pub async fn list_hens(&self, state: Option<HenState>) -> CoreResult<Vec<Hen>> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::ListHens { state, reply: tx }).await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Transition a Hen's state.
    pub async fn transition_hen(&self, id: HenId, next: HenState) -> CoreResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::TransitionHen {
            id,
            next,
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Delete a Hen.
    pub async fn delete_hen(&self, id: HenId) -> CoreResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::DeleteHen { id, reply: tx }).await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Submit a new Job.
    pub async fn submit_job(&self, hen_id: HenId, prompt: String) -> CoreResult<String> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::SubmitJob {
            hen_id,
            prompt,
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Fetch a Job.
    pub async fn get_job(&self, id: String) -> CoreResult<Job> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::GetJob { id, reply: tx }).await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// List Jobs (optionally filter by Hen).
    pub async fn list_jobs(&self, hen_id: Option<HenId>) -> CoreResult<Vec<Job>> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::ListJobs { hen_id, reply: tx }).await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Update a Job (runner internal).
    pub async fn update_job(&self, job: Job) -> CoreResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::UpdateJob { job, reply: tx }).await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Record an episodic memory for a Hen.
    pub async fn record_memory(&self, entry: MemoryEntry) -> CoreResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::RecordMemory { entry, reply: tx })
            .await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Load a Hen's recent episodic memories (oldest-first), capped at `limit`.
    pub async fn load_memories(
        &self,
        hen_id: HenId,
        limit: Option<usize>,
    ) -> CoreResult<Vec<MemoryEntry>> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::LoadMemories {
            hen_id,
            limit,
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Forget all of a Hen's episodic memories; returns the count removed.
    pub async fn forget_memories(&self, hen_id: HenId) -> CoreResult<usize> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::ForgetMemories { hen_id, reply: tx })
            .await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Dispatch the oldest Queued job for `hen_id`, if any.
    pub async fn dispatch_next_queued(&self, hen_id: HenId) -> CoreResult<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::DispatchNextQueued { hen_id, reply: tx })
            .await?;
        rx.await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))?
    }

    /// Initiate graceful shutdown.
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(OrchCmd::Shutdown).await;
    }
}

/// Terminal poll interval while waiting on a delegated sub-job.
const DELEGATE_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[async_trait::async_trait]
impl Delegator for OrchHandle {
    async fn delegate(&self, req: DelegationRequest) -> CoreResult<DelegationOutcome> {
        let to = req.to.clone();
        let timeout = req.timeout;
        // 1) Ask the orchestrator to create the sub-job (non-blocking: it just
        //    validates, enqueues and spawns, then returns the new job id).
        let (tx, rx) = oneshot::channel();
        self.send(OrchCmd::Delegate {
            from: req.from,
            to: req.to,
            prompt: req.prompt,
            parent_depth: req.parent_depth,
            reply: tx,
        })
        .await?;
        let job_id = rx
            .await
            .map_err(|_| CoreError::Other("orchestrator dropped reply".into()))??;

        // 2) Poll for completion client-side so the orchestrator loop stays free
        //    to process the worker's job updates (no self-deadlock).
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let job = self.get_job(job_id.clone()).await?;
            match job.status {
                JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled => {
                    let output = match job.status {
                        JobStatus::Done => job.result.unwrap_or_default(),
                        JobStatus::Failed => job.error.unwrap_or_default(),
                        _ => "cancelled".to_string(),
                    };
                    return Ok(DelegationOutcome {
                        job_id,
                        status: job.status,
                        output,
                        depth: job.delegation_depth,
                    });
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CoreError::Other(format!(
                    "delegation to {to} timed out after {}s (job {job_id} still {:?})",
                    timeout.as_secs(),
                    job.status
                )));
            }
            tokio::time::sleep(DELEGATE_POLL_INTERVAL).await;
        }
    }
}

/// Spawn the orchestrator task. Returns a clonable handle.
pub fn spawn(
    store: Store,
    tools: Arc<Registry>,
    brain_factory: Arc<Mutex<BrainFactory>>,
    workdir_base: std::path::PathBuf,
) -> OrchHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel::<OrchCmd>(1024);
    let (event_tx, _event_rx) = broadcast::channel::<OrchEvent>(1024);
    let coop_id = CoopId::new("local.coop").expect("valid coop_id");

    let handle = OrchHandle {
        cmd_tx,
        events: event_tx.clone(),
        coop_id: coop_id.clone(),
        brain_factory: brain_factory.clone(),
        workdir_base: workdir_base.clone(),
        auto_launched: Arc::new(Mutex::new(std::collections::HashSet::new())),
    };

    let handle_for_task = handle.clone();
    tokio::spawn(async move {
        run(
            cmd_rx,
            event_tx,
            store,
            tools,
            brain_factory,
            workdir_base,
            handle_for_task,
            coop_id,
        )
        .await;
    });

    handle
}

#[allow(clippy::too_many_arguments)]
async fn run(
    mut cmd_rx: mpsc::Receiver<OrchCmd>,
    events: broadcast::Sender<OrchEvent>,
    store: Store,
    tools: Arc<Registry>,
    brain_factory: Arc<Mutex<BrainFactory>>,
    workdir_base: std::path::PathBuf,
    self_handle: OrchHandle,
    coop_id: CoopId,
) {
    info!("orchestrator started");

    // Idle-hen dehydrate: every 60s, transition Idle hens with no recent
    // activity to Dormant. Threshold is conservative (10 minutes) to avoid
    // surprising the operator. Wake-up happens automatically when a job is
    // submitted (see `handle_submit`).
    let mut dehydrate_tick = interval(Duration::from_secs(60));
    dehydrate_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let idle_threshold_secs: i64 = std::env::var("COOPD_IDLE_DEHYDRATE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);

    loop {
        tokio::select! {
            biased;
            maybe = cmd_rx.recv() => {
                let Some(cmd) = maybe else { break };
                match cmd {
            OrchCmd::CreateHen { manifest, reply } => {
                let res = handle_create(&store, &coop_id, manifest, &events);
                let _ = reply.send(res);
            }
            OrchCmd::GetHen { id, reply } => {
                let res = store.get_hen(&id).map_err(map_storage_err);
                let _ = reply.send(res);
            }
            OrchCmd::ListHens { state, reply } => {
                let res = store.list_hens().map_err(map_storage_err).map(|hens| {
                    if let Some(s) = state {
                        hens.into_iter().filter(|h| h.state == s).collect()
                    } else {
                        hens
                    }
                });
                let _ = reply.send(res);
            }
            OrchCmd::TransitionHen { id, next, reply } => {
                let res = handle_transition(&store, &id, next, &events);
                let _ = reply.send(res);
            }
            OrchCmd::DeleteHen { id, reply } => {
                let res = handle_delete(&store, &id, &events);
                let _ = reply.send(res);
            }
            OrchCmd::SubmitJob {
                hen_id,
                prompt,
                reply,
            } => {
                let res = handle_submit(
                    &store,
                    hen_id,
                    prompt,
                    &events,
                    &self_handle,
                    &tools,
                    &brain_factory,
                    &workdir_base,
                );
                let _ = reply.send(res);
            }
            OrchCmd::GetJob { id, reply } => {
                let res = store.get_job(&id).map_err(map_storage_err);
                let _ = reply.send(res);
            }
            OrchCmd::ListJobs { hen_id, reply } => {
                let res = store.list_jobs(hen_id.as_ref()).map_err(map_storage_err);
                let _ = reply.send(res);
            }
            OrchCmd::UpdateJob { job, reply } => {
                let status = job.status;
                let job_id = job.id.clone();
                let res = store.put_job(&job).map_err(map_storage_err);
                if res.is_ok() {
                    let _ = events.send(OrchEvent::JobStatusChanged { job_id, status });
                }
                let _ = reply.send(res);
            }
            OrchCmd::RecordMemory { entry, reply } => {
                let res = handle_record_memory(&store, &entry, &events);
                let _ = reply.send(res);
            }
            OrchCmd::LoadMemories {
                hen_id,
                limit,
                reply,
            } => {
                let res = store
                    .list_memories(&hen_id, limit)
                    .map_err(map_storage_err);
                let _ = reply.send(res);
            }
            OrchCmd::ForgetMemories { hen_id, reply } => {
                let res = store.delete_memories(&hen_id).map_err(map_storage_err);
                let _ = reply.send(res);
            }
            OrchCmd::DispatchNextQueued { hen_id, reply } => {
                let res = handle_dispatch_next(
                    &store,
                    &hen_id,
                    &events,
                    &self_handle,
                    &tools,
                    &brain_factory,
                    &workdir_base,
                );
                let _ = reply.send(res);
            }
            OrchCmd::Delegate {
                from,
                to,
                prompt,
                parent_depth,
                reply,
            } => {
                let res = handle_delegate(
                    &store,
                    from,
                    to,
                    prompt,
                    parent_depth,
                    &events,
                    &self_handle,
                    &tools,
                    &brain_factory,
                    &workdir_base,
                );
                let _ = reply.send(res);
            }
            OrchCmd::Shutdown => {
                info!("orchestrator shutting down");
                let _ = events.send(OrchEvent::ShuttingDown);
                break;
            }
                }
            }
            _ = dehydrate_tick.tick() => {
                dehydrate_idle_hens(&store, &events, idle_threshold_secs);
            }
        }
    }
    debug!("orchestrator stopped");
}

/// Transition Idle hens that have been quiet for `threshold_secs` to Dormant.
///
/// Designed to free per-hen working set (tool sandboxes, watchers) in a future
/// release; for v0.1 it just emits the state transition so the UI / federation
/// layer can act on it.
fn dehydrate_idle_hens(store: &Store, events: &broadcast::Sender<OrchEvent>, threshold_secs: i64) {
    use time::OffsetDateTime;
    let Ok(hens) = store.list_hens() else { return };
    let now = OffsetDateTime::now_utc();
    for hen in hens {
        if hen.state != HenState::Idle {
            continue;
        }
        let idle_for = (now - hen.updated_at).whole_seconds();
        if idle_for < threshold_secs {
            continue;
        }
        match handle_transition(store, &hen.id, HenState::Dormant, events) {
            Ok(()) => info!(hen_id = %hen.id, idle_for, "dehydrated idle hen"),
            Err(e) => warn!(hen_id = %hen.id, error = %e, "dehydrate failed"),
        }
    }
}

fn map_storage_err(e: coopd_storage::StorageError) -> CoreError {
    match e {
        coopd_storage::StorageError::NotFound(s) => CoreError::HenNotFound(s),
        other => CoreError::Other(other.to_string()),
    }
}

fn handle_create(
    store: &Store,
    coop_id: &CoopId,
    manifest: AgentManifest,
    events: &broadcast::Sender<OrchEvent>,
) -> CoreResult<HenId> {
    manifest.validate()?;
    let id = HenId::new(coop_id, &manifest.name)?;
    if store.get_hen(&id).is_ok() {
        return Err(CoreError::Other(format!("hen already exists: {id}")));
    }
    let mut hen = Hen::new(id.clone(), manifest);
    // Wire MemorySpec.inherit_from: a freshly created Hen can inherit a
    // parent's episodic memory (copied under fresh ids) and record lineage,
    // so it does not start from zero. Missing/invalid parents are non-fatal.
    let inherit_from = hen
        .manifest
        .memory
        .as_ref()
        .and_then(|m| m.inherit_from.clone());
    if let Some(parent_ref) = inherit_from {
        match HenId::parse(&parent_ref) {
            Ok(parent_id) => match store.get_hen(&parent_id) {
                Ok(parent) => {
                    let inherited = store
                        .list_memories(&parent_id, None)
                        .map_err(map_storage_err)?;
                    let copied = inherited.len();
                    for mem in &inherited {
                        store
                            .put_memory(&mem.reparented_to(id.clone()))
                            .map_err(map_storage_err)?;
                    }
                    hen.lineage.parent = Some(parent_id.to_string());
                    hen.lineage.generation = parent.lineage.generation.max(1) + 1;
                    info!(%id, parent = %parent_id, copied, "hen inherited memory");
                }
                Err(_) => {
                    warn!(%id, parent = %parent_ref, "inherit_from: parent not found; starting fresh");
                }
            },
            Err(e) => {
                warn!(%id, parent = %parent_ref, error = %e, "inherit_from: invalid parent id; starting fresh");
            }
        }
    }
    store.put_hen(&hen).map_err(map_storage_err)?;
    let _ = events.send(OrchEvent::HenCreated { id: id.clone() });
    info!(%id, "hen created");
    Ok(id)
}

fn handle_transition(
    store: &Store,
    id: &HenId,
    next: HenState,
    events: &broadcast::Sender<OrchEvent>,
) -> CoreResult<()> {
    let mut hen = store.get_hen(id).map_err(map_storage_err)?;
    let from = hen.state;
    hen.transition(next)?;
    store.put_hen(&hen).map_err(map_storage_err)?;
    let _ = events.send(OrchEvent::HenStateChanged {
        id: id.clone(),
        from,
        to: next,
    });
    info!(%id, ?from, ?next, "hen state changed");
    Ok(())
}

fn handle_record_memory(
    store: &Store,
    entry: &MemoryEntry,
    events: &broadcast::Sender<OrchEvent>,
) -> CoreResult<()> {
    let hen_id = entry.hen_id.clone();
    let entry_id = entry.id.clone();
    store.put_memory(entry).map_err(map_storage_err)?;
    let _ = events.send(OrchEvent::MemoryRecorded {
        hen_id: hen_id.clone(),
        entry_id,
    });
    // Enforce episodic retention (governance): drop episodes older than the
    // manifest's window. Missing/zero retention keeps memory indefinitely.
    if let Ok(hen) = store.get_hen(&hen_id)
        && let Some(days) = hen
            .manifest
            .memory
            .as_ref()
            .and_then(|m| m.episodic_retention_days)
        && days > 0
    {
        let cutoff = time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days));
        match store.prune_memories(&hen_id, cutoff) {
            Ok(n) if n > 0 => {
                info!(%hen_id, pruned = n, retention_days = days, "pruned expired memories")
            }
            Ok(_) => {}
            Err(e) => warn!(%hen_id, error = %e, "memory prune failed"),
        }
    }
    Ok(())
}

fn handle_delete(
    store: &Store,
    id: &HenId,
    events: &broadcast::Sender<OrchEvent>,
) -> CoreResult<()> {
    let removed = store.delete_hen(id).map_err(map_storage_err)?;
    if !removed {
        warn!(%id, "delete: hen not found");
        return Err(CoreError::HenNotFound(id.to_string()));
    }
    // Right-to-forget: a deleted Hen's episodic memory is purged too.
    match store.delete_memories(id) {
        Ok(n) if n > 0 => info!(%id, purged = n, "purged hen memories on delete"),
        Ok(_) => {}
        Err(e) => warn!(%id, error = %e, "failed to purge hen memories on delete"),
    }
    let _ = events.send(OrchEvent::HenDeleted { id: id.clone() });
    info!(%id, "hen deleted");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_submit(
    store: &Store,
    hen_id: HenId,
    prompt: String,
    events: &broadcast::Sender<OrchEvent>,
    handle: &OrchHandle,
    tools: &Arc<Registry>,
    brain_factory: &Arc<Mutex<BrainFactory>>,
    workdir_base: &std::path::Path,
) -> CoreResult<String> {
    enqueue_job(
        store,
        hen_id,
        prompt,
        0,
        events,
        handle,
        tools,
        brain_factory,
        workdir_base,
    )
}

/// Rehydrate `hen_id` if needed, persist a Queued job at `depth`, emit
/// `JobSubmitted`, and spawn the runner immediately if the hen is Idle.
/// Shared by farmer submission (depth 0) and delegation (depth +1).
#[allow(clippy::too_many_arguments)]
fn enqueue_job(
    store: &Store,
    hen_id: HenId,
    prompt: String,
    depth: u32,
    events: &broadcast::Sender<OrchEvent>,
    handle: &OrchHandle,
    tools: &Arc<Registry>,
    brain_factory: &Arc<Mutex<BrainFactory>>,
    workdir_base: &std::path::Path,
) -> CoreResult<String> {
    let hen = store.get_hen(&hen_id).map_err(map_storage_err)?;
    // Auto-rehydrate hens on job submission.
    // Dormant/Sleeping -> Idle (single hop). Defined -> Hatching -> Idle (two hops).
    let hen = match hen.state {
        HenState::Dormant | HenState::Sleeping => {
            handle_transition(store, &hen_id, HenState::Idle, events)?;
            store.get_hen(&hen_id).map_err(map_storage_err)?
        }
        HenState::Defined => {
            handle_transition(store, &hen_id, HenState::Hatching, events)?;
            handle_transition(store, &hen_id, HenState::Idle, events)?;
            store.get_hen(&hen_id).map_err(map_storage_err)?
        }
        _ => hen,
    };
    // Persist the job as Queued regardless of state — the runner drains the
    // per-hen queue on completion, so callers can stream prompts even while
    // the hen is Working/Hatching/Leased.
    let job = Job::new(hen_id.clone(), prompt).at_depth(depth);
    store.put_job(&job).map_err(map_storage_err)?;
    let _ = events.send(OrchEvent::JobSubmitted {
        job_id: job.id.clone(),
        hen_id,
    });
    let job_id = job.id.clone();
    if matches!(hen.state, HenState::Idle) {
        runner::spawn_job_task(
            handle.clone(),
            tools.clone(),
            brain_factory.clone(),
            workdir_base.to_path_buf(),
            job,
        );
    }
    Ok(job_id)
}

/// Create a delegated sub-job: validate (self/cycle/depth), confirm the target
/// exists, enqueue at `parent_depth + 1`, and emit `Delegated`.
#[allow(clippy::too_many_arguments)]
fn handle_delegate(
    store: &Store,
    from: HenId,
    to: HenId,
    prompt: String,
    parent_depth: u32,
    events: &broadcast::Sender<OrchEvent>,
    handle: &OrchHandle,
    tools: &Arc<Registry>,
    brain_factory: &Arc<Mutex<BrainFactory>>,
    workdir_base: &std::path::Path,
) -> CoreResult<String> {
    let next_depth = parent_depth + 1;
    // Authoritative re-validation (the tool/API pre-validate for nicer errors).
    validate_delegation(&from, &to, next_depth)?;
    // Confirm the target hen exists before enqueuing.
    store.get_hen(&to).map_err(map_storage_err)?;
    let job_id = enqueue_job(
        store,
        to.clone(),
        prompt,
        next_depth,
        events,
        handle,
        tools,
        brain_factory,
        workdir_base,
    )?;
    let _ = events.send(OrchEvent::Delegated {
        from,
        to,
        job_id: job_id.clone(),
    });
    Ok(job_id)
}

#[allow(clippy::too_many_arguments)]
fn handle_dispatch_next(
    store: &Store,
    hen_id: &HenId,
    _events: &broadcast::Sender<OrchEvent>,
    handle: &OrchHandle,
    tools: &Arc<Registry>,
    brain_factory: &Arc<Mutex<BrainFactory>>,
    workdir_base: &std::path::Path,
) -> CoreResult<Option<String>> {
    use coopd_core::JobStatus;
    let hen = store.get_hen(hen_id).map_err(map_storage_err)?;
    if !matches!(hen.state, HenState::Idle) {
        return Ok(None);
    }
    let mut jobs = store
        .list_jobs(Some(hen_id))
        .map_err(map_storage_err)?
        .into_iter()
        .filter(|j| matches!(j.status, JobStatus::Queued))
        .collect::<Vec<_>>();
    if jobs.is_empty() {
        return Ok(None);
    }
    jobs.sort_by_key(|j| j.created_at);
    let job = jobs.remove(0);
    let job_id = job.id.clone();
    runner::spawn_job_task(
        handle.clone(),
        tools.clone(),
        brain_factory.clone(),
        workdir_base.to_path_buf(),
        job,
    );
    Ok(Some(job_id))
}
