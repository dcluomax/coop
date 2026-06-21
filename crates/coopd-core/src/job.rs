//! Job (quest) data model.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::HenId;

/// Lifecycle status of a Job.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum JobStatus {
    /// Submitted but not yet picked up by a runner.
    Queued,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Done,
    /// Failed with an error.
    Failed,
    /// Cancelled by the farmer.
    Cancelled,
}

/// A single quest assigned to a Hen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// UUIDv7 job identifier.
    pub id: String,
    /// Hen this job is assigned to.
    pub hen_id: HenId,
    /// User prompt / task description.
    pub prompt: String,
    /// Current status.
    pub status: JobStatus,
    /// Final assistant text (set on Done).
    #[serde(default)]
    pub result: Option<String>,
    /// Error message (set on Failed).
    #[serde(default)]
    pub error: Option<String>,
    /// Number of reason/tool turns consumed.
    #[serde(default)]
    pub turns: u32,
    /// Delegation depth: 0 for a farmer-submitted job, +1 per delegation hop.
    /// Bounds delegation recursion (see `coopd_core::delegation`).
    #[serde(default)]
    pub delegation_depth: u32,
    /// Total Grain cost.
    #[serde(default)]
    pub grain_spent: u64,
    /// Creation timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last status change.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Job {
    /// Construct a new Queued job.
    pub fn new(hen_id: HenId, prompt: String) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: Uuid::now_v7().to_string(),
            hen_id,
            prompt,
            status: JobStatus::Queued,
            result: None,
            error: None,
            turns: 0,
            delegation_depth: 0,
            grain_spent: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Set the delegation depth (builder-style). Used by the orchestrator when
    /// a job is created via delegation.
    #[must_use]
    pub fn at_depth(mut self, depth: u32) -> Self {
        self.delegation_depth = depth;
        self
    }

    /// Mark the job as running.
    pub fn mark_running(&mut self) {
        self.status = JobStatus::Running;
        self.updated_at = OffsetDateTime::now_utc();
    }

    /// Mark the job as completed successfully.
    pub fn mark_done(&mut self, result: String) {
        self.status = JobStatus::Done;
        self.result = Some(result);
        self.updated_at = OffsetDateTime::now_utc();
    }

    /// Mark the job as failed.
    pub fn mark_failed(&mut self, error: String) {
        self.status = JobStatus::Failed;
        self.error = Some(error);
        self.updated_at = OffsetDateTime::now_utc();
    }
}
