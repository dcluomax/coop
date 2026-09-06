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

impl JobStatus {
    /// Whether this status can no longer execute.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// Optional filters for a creation-ordered job history.
#[derive(Debug, Clone, Default)]
pub struct JobQuery {
    /// Restrict the history to one Hen.
    pub hen_id: Option<HenId>,
    /// Restrict the history to one lifecycle status.
    pub status: Option<JobStatus>,
    /// Case-insensitive text search across identifiers, prompt, result and error.
    pub search: Option<String>,
    /// Maximum number of matching jobs; absent preserves the full history.
    pub limit: Option<usize>,
    /// Number of matching jobs to skip before collecting results.
    pub offset: usize,
    /// Return newest jobs first instead of the legacy oldest-first order.
    pub descending: bool,
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
    /// Original failed or cancelled job when this job is an explicit retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
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
            retry_of: None,
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

    /// Mark a queued job as cancelled without executing it.
    pub fn mark_cancelled(&mut self) {
        self.status = JobStatus::Cancelled;
        self.updated_at = OffsetDateTime::now_utc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_job_records_load_without_retry_metadata() {
        let job = Job::new(HenId::parse("local.coop/aria").unwrap(), "work".into());
        let mut value = serde_json::to_value(&job).unwrap();
        value.as_object_mut().unwrap().remove("retry_of");
        let loaded: Job = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.retry_of, None);
        assert_eq!(loaded.status, JobStatus::Queued);
    }

    #[test]
    fn cancelled_job_is_terminal_and_preserves_history() {
        let mut job = Job::new(HenId::parse("local.coop/aria").unwrap(), "work".into());
        let created = job.created_at;
        job.mark_cancelled();
        assert!(job.status.is_terminal());
        assert_eq!(job.created_at, created);
        assert!(job.result.is_none());
        assert!(!JobStatus::Queued.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
    }
}
