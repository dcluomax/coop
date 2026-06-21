//! Orchestrator command and event channels.
//!
//! The orchestrator is a single Tokio task that owns the authoritative farm
//! state. All mutations flow through it via [`OrchCmd`]. Subscribers observe
//! changes via [`OrchEvent`] broadcasts.

use serde::Serialize;
use tokio::sync::oneshot;

use crate::error::Result;
use crate::hen::{Hen, HenState};
use crate::ids::HenId;
use crate::manifest::AgentManifest;

/// Commands sent into the orchestrator.
///
/// Every variant carries a `oneshot::Sender` reply channel so the caller can
/// observe success / failure synchronously.
#[derive(Debug)]
pub enum OrchCmd {
    /// Create a new Hen from a manifest.
    CreateHen {
        /// Validated manifest.
        manifest: AgentManifest,
        /// Reply channel.
        reply: oneshot::Sender<Result<HenId>>,
    },
    /// Fetch a Hen by ID.
    GetHen {
        /// Hen identifier.
        id: HenId,
        /// Reply channel.
        reply: oneshot::Sender<Result<Hen>>,
    },
    /// List all known Hens.
    ListHens {
        /// Optional filter on state.
        state: Option<HenState>,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<Hen>>>,
    },
    /// Transition a Hen to a new state.
    TransitionHen {
        /// Hen identifier.
        id: HenId,
        /// Target state.
        next: HenState,
        /// Reply channel.
        reply: oneshot::Sender<Result<()>>,
    },
    /// Delete a Hen permanently.
    DeleteHen {
        /// Hen identifier.
        id: HenId,
        /// Reply channel.
        reply: oneshot::Sender<Result<()>>,
    },
    /// Submit a new Job for execution.
    SubmitJob {
        /// Target Hen.
        hen_id: HenId,
        /// User prompt.
        prompt: String,
        /// Reply channel returning the Job ID.
        reply: oneshot::Sender<Result<String>>,
    },
    /// Fetch a job by ID.
    GetJob {
        /// Job ID.
        id: String,
        /// Reply channel.
        reply: oneshot::Sender<Result<crate::Job>>,
    },
    /// List jobs (optionally filter by Hen).
    ListJobs {
        /// Optional filter.
        hen_id: Option<HenId>,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<crate::Job>>>,
    },
    /// Persist an updated Job (used by runner tasks).
    UpdateJob {
        /// Updated record.
        job: crate::Job,
        /// Reply channel.
        reply: oneshot::Sender<Result<()>>,
    },
    /// Record an episodic memory for a Hen (used by runner after a job
    /// reaches a terminal state).
    RecordMemory {
        /// The episode to persist.
        entry: crate::MemoryEntry,
        /// Reply channel.
        reply: oneshot::Sender<Result<()>>,
    },
    /// Load a Hen's most recent episodic memories (oldest-first), capped at
    /// `limit` when provided.
    LoadMemories {
        /// Hen identifier.
        hen_id: HenId,
        /// Maximum number of most-recent episodes to return.
        limit: Option<usize>,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<crate::MemoryEntry>>>,
    },
    /// Forget all of a Hen's episodic memories. Replies with the count removed.
    ForgetMemories {
        /// Hen identifier.
        hen_id: HenId,
        /// Reply channel returning the number of episodes deleted.
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Delegate a subtask from one Hen to another (creates a sub-job on the
    /// target and replies with its job ID; the caller polls for the result).
    Delegate {
        /// Delegating ("manager") Hen.
        from: HenId,
        /// Target Hen that will perform the subtask.
        to: HenId,
        /// Subtask prompt.
        prompt: String,
        /// Delegation depth of the calling job (sub-job runs at `+1`).
        parent_depth: u32,
        /// Reply channel returning the created sub-job ID.
        reply: oneshot::Sender<Result<String>>,
    },
    /// Try to pick up the next Queued job for a Hen (used by runner after a
    /// job completes; the orchestrator inspects state and spawns the next
    /// runner if the hen is Idle and a Queued job exists).
    DispatchNextQueued {
        /// Hen identifier.
        hen_id: HenId,
        /// Reply channel (Some(job_id) if dispatched, None otherwise).
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    /// Trigger graceful shutdown.
    Shutdown,
}

/// Events broadcast by the orchestrator to subscribers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrchEvent {
    /// A new Hen was created.
    HenCreated {
        /// Hen identifier.
        id: HenId,
    },
    /// A Hen changed state.
    HenStateChanged {
        /// Hen identifier.
        id: HenId,
        /// Previous state.
        from: HenState,
        /// New state.
        to: HenState,
    },
    /// A Hen was deleted.
    HenDeleted {
        /// Hen identifier.
        id: HenId,
    },
    /// A new Job was submitted.
    JobSubmitted {
        /// Job ID.
        job_id: String,
        /// Owning Hen.
        hen_id: HenId,
    },
    /// A Job changed status.
    JobStatusChanged {
        /// Job ID.
        job_id: String,
        /// New status.
        status: crate::JobStatus,
    },
    /// An episodic memory was recorded for a Hen (audit/legibility).
    MemoryRecorded {
        /// Owning Hen.
        hen_id: HenId,
        /// Episode identifier.
        entry_id: String,
    },
    /// One Hen delegated a subtask to another (audit/legibility).
    Delegated {
        /// Delegating ("manager") Hen.
        from: HenId,
        /// Target Hen performing the subtask.
        to: HenId,
        /// The created sub-job.
        job_id: String,
    },
    /// Orchestrator is shutting down.
    ShuttingDown,
}
