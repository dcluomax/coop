//! Persistent episodic memory for Hens.
//!
//! A [`MemoryEntry`] is a compact, deterministic record of one completed job
//! (its prompt, a short outcome summary, turn count, and result status). The
//! runner appends one entry per finished job and injects the most recent
//! entries back into the next job's system prompt — so a Hen is *not* starting
//! from zero on every task. This is "episodic" memory: literal traces of what
//! the Hen did, with no model summarization (that "semantic" layer is a future
//! phase and would require an LLM call).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::HenId;
use crate::job::{Job, JobStatus};

/// Outcome of the job an episode records.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOutcome {
    /// Job completed successfully.
    Done,
    /// Job failed.
    Failed,
}

impl MemoryOutcome {
    /// Short human label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MemoryOutcome::Done => "ok",
            MemoryOutcome::Failed => "failed",
        }
    }
}

/// Max characters retained for the prompt slice of an episode.
pub const MEMORY_PROMPT_BUDGET: usize = 400;
/// Max characters retained for the summary slice of an episode.
pub const MEMORY_SUMMARY_BUDGET: usize = 600;
/// Default number of recent episodes injected into a Hen's working context.
pub const DEFAULT_MEMORY_CONTEXT_ENTRIES: usize = 8;

/// A single episodic memory: a compact record of one completed job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// UUIDv7 identifier (chronologically sortable, like job IDs).
    pub id: String,
    /// Owning Hen.
    pub hen_id: HenId,
    /// Job this episode summarizes.
    pub job_id: String,
    /// When the episode was recorded (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The task prompt (truncated to [`MEMORY_PROMPT_BUDGET`]).
    pub prompt: String,
    /// A short outcome summary (truncated to [`MEMORY_SUMMARY_BUDGET`]).
    pub summary: String,
    /// Reason/tool turns the job consumed.
    pub turns: u32,
    /// Whether the job succeeded or failed.
    pub outcome: MemoryOutcome,
}

impl MemoryEntry {
    /// Construct an episode, generating a fresh UUIDv7 id and `at = now`.
    /// `prompt` and `summary` are trimmed and truncated to their budgets.
    #[must_use]
    pub fn new(
        hen_id: HenId,
        job_id: String,
        prompt: &str,
        summary: &str,
        turns: u32,
        outcome: MemoryOutcome,
    ) -> Self {
        Self {
            id: Uuid::now_v7().to_string(),
            hen_id,
            job_id,
            at: OffsetDateTime::now_utc(),
            prompt: truncate_chars(prompt, MEMORY_PROMPT_BUDGET),
            summary: truncate_chars(summary, MEMORY_SUMMARY_BUDGET),
            turns,
            outcome,
        }
    }

    /// Build an episode from a terminal [`Job`]. Returns `None` for jobs that
    /// are not in a terminal `Done`/`Failed` state (nothing to remember yet).
    #[must_use]
    pub fn from_job(job: &Job) -> Option<Self> {
        let (outcome, summary) = match job.status {
            JobStatus::Done => (MemoryOutcome::Done, job.result.clone().unwrap_or_default()),
            JobStatus::Failed => (
                MemoryOutcome::Failed,
                format!(
                    "(failed) {}",
                    job.error.as_deref().unwrap_or("unknown error")
                ),
            ),
            _ => return None,
        };
        Some(Self::new(
            job.hen_id.clone(),
            job.id.clone(),
            &job.prompt,
            &summary,
            job.turns,
            outcome,
        ))
    }

    /// Re-key this episode for a new owner, minting a fresh id. Used when a
    /// child Hen inherits its parent's memory on creation.
    #[must_use]
    pub fn reparented_to(&self, new_owner: HenId) -> Self {
        Self {
            id: Uuid::now_v7().to_string(),
            hen_id: new_owner,
            ..self.clone()
        }
    }
}

/// Render recent episodes into a system-prompt section. `entries` are expected
/// in chronological order (oldest first); the rendered list preserves that so
/// the model reads its history top-to-bottom. Returns an empty string when
/// there is nothing to remember.
#[must_use]
pub fn render_memory_context(entries: &[MemoryEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Memory — your recent episodes\n\
         These are compact records of tasks you handled before, oldest first. \
         Use them for continuity; do not repeat completed work.\n",
    );
    for e in entries {
        let date =
            e.at.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| e.at.unix_timestamp().to_string());
        out.push_str(&format!(
            "- [{date}] ({}, {} turn{}) task: {:?} → {}\n",
            e.outcome.label(),
            e.turns,
            if e.turns == 1 { "" } else { "s" },
            e.prompt,
            e.summary,
        ));
    }
    out
}

/// Truncate `s` to at most `max` characters (char-boundary safe), trimming
/// surrounding whitespace and appending an ellipsis when content was dropped.
fn truncate_chars(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    let mut out: String = trimmed.chars().take(max).collect();
    if trimmed.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CoopId, HenId};
    use crate::job::Job;

    fn hen() -> HenId {
        let coop = CoopId::new("alice.coop").unwrap();
        HenId::new(&coop, "aria").unwrap()
    }

    #[test]
    fn truncate_respects_char_boundaries_and_budget() {
        let s = "héllo wörld ☃ a lot of text here";
        let t = truncate_chars(s, 5);
        assert_eq!(t.chars().count(), 6); // 5 + ellipsis
        assert!(t.ends_with('…'));
        // Short strings are returned untouched (minus trim).
        assert_eq!(truncate_chars("  hi  ", 10), "hi");
    }

    #[test]
    fn from_job_done_and_failed() {
        let mut j = Job::new(hen(), "raise a hen".to_string());
        assert!(
            MemoryEntry::from_job(&j).is_none(),
            "queued job not terminal"
        );

        j.mark_done("the hen is alive and IDLE".to_string());
        j.turns = 3;
        let m = MemoryEntry::from_job(&j).unwrap();
        assert_eq!(m.outcome, MemoryOutcome::Done);
        assert_eq!(m.turns, 3);
        assert_eq!(m.prompt, "raise a hen");
        assert_eq!(m.summary, "the hen is alive and IDLE");
        assert_eq!(m.job_id, j.id);

        let mut f = Job::new(hen(), "do the thing".to_string());
        f.mark_failed("brain: 401 unauthorized".to_string());
        let mf = MemoryEntry::from_job(&f).unwrap();
        assert_eq!(mf.outcome, MemoryOutcome::Failed);
        assert!(mf.summary.starts_with("(failed) brain: 401"));
    }

    #[test]
    fn render_is_empty_when_no_entries() {
        assert_eq!(render_memory_context(&[]), "");
    }

    #[test]
    fn render_lists_entries_oldest_first() {
        let a = MemoryEntry::new(
            hen(),
            "j1".into(),
            "first task",
            "did A",
            1,
            MemoryOutcome::Done,
        );
        let b = MemoryEntry::new(
            hen(),
            "j2".into(),
            "second task",
            "did B",
            2,
            MemoryOutcome::Failed,
        );
        let rendered = render_memory_context(&[a, b]);
        assert!(rendered.contains("recent episodes"));
        let ia = rendered.find("first task").unwrap();
        let ib = rendered.find("second task").unwrap();
        assert!(ia < ib, "oldest entry should render first");
        assert!(rendered.contains("(ok, 1 turn)"));
        assert!(rendered.contains("(failed, 2 turns)"));
    }

    #[test]
    fn reparent_mints_new_id_and_owner() {
        let coop = CoopId::new("bob.coop").unwrap();
        let child = HenId::new(&coop, "bolt").unwrap();
        let parent_mem =
            MemoryEntry::new(hen(), "j1".into(), "task", "result", 1, MemoryOutcome::Done);
        let inherited = parent_mem.reparented_to(child.clone());
        assert_eq!(inherited.hen_id, child);
        assert_ne!(inherited.id, parent_mem.id);
        assert_eq!(inherited.summary, parent_mem.summary);
    }
}
