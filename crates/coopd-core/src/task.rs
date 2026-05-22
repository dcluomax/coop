//! Farm-wide task queue.
//!
//! A [`Task`] is a unit of work that is not tied to any specific hen at
//! submission time. The orchestrator routes it to the first eligible hen
//! based on [`crate::AgentKind`] matching (and, in the future, flock /
//! tags). When dispatched it is **delivered into the hen's persistent
//! tmux session** via `tmux send-keys`, so the CLI agent (claude, codex,
//! gh copilot) running in that session sees the prompt as if the human
//! had typed it.
//!
//! This is intentionally **separate from [`crate::Job`]** because:
//!
//! * `Job` is per-hen and consumed by the in-process Anthropic runner.
//! * `Task` is per-farm and consumed by *any* matching hen via tmux.
//!
//! The two queues coexist; jobs are the right primitive for the v0.1
//! Anthropic-brain workflow, tasks are the right primitive for CLI-agent
//! hens (which own their own auth + model selection).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::HenId;
use crate::manifest::AgentKind;

/// Lifecycle status of a [`Task`].
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TaskStatus {
    /// Submitted, waiting for a matching hen to claim it.
    Pending,
    /// Sent into a hen's tmux session; the CLI agent is now working on it.
    /// Coopd has no reliable way to observe "done" from inside tmux yet,
    /// so the farmer marks it done from the UI (or it stays Dispatched).
    Dispatched,
    /// Marked done by the farmer (or future tmux-output heuristic).
    Done,
    /// Cancelled before dispatch.
    Cancelled,
}

/// A farm-wide task that any matching hen can pick up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// UUIDv7 task identifier.
    pub id: String,
    /// Human-readable prompt typed into the hen's tmux session verbatim.
    pub prompt: String,
    /// Required agent runtime; `None` means "any tmux-driven CLI agent".
    #[serde(default)]
    pub required_agent_kind: Option<AgentKind>,
    /// Current status.
    pub status: TaskStatus,
    /// Hen that claimed this task (set on `Dispatched`).
    #[serde(default)]
    pub claimed_by: Option<HenId>,
    /// Submission timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last status change.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Task {
    /// Construct a new Pending task.
    #[must_use]
    pub fn new(prompt: String, required_agent_kind: Option<AgentKind>) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: Uuid::now_v7().to_string(),
            prompt,
            required_agent_kind,
            status: TaskStatus::Pending,
            claimed_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Mark the task as dispatched to a hen.
    pub fn mark_dispatched(&mut self, hen: HenId) {
        self.claimed_by = Some(hen);
        self.status = TaskStatus::Dispatched;
        self.updated_at = OffsetDateTime::now_utc();
    }

    /// Mark the task as done.
    pub fn mark_done(&mut self) {
        self.status = TaskStatus::Done;
        self.updated_at = OffsetDateTime::now_utc();
    }

    /// Mark the task as cancelled.
    pub fn mark_cancelled(&mut self) {
        self.status = TaskStatus::Cancelled;
        self.updated_at = OffsetDateTime::now_utc();
    }
}
