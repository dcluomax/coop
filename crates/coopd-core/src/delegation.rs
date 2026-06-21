//! In-farm Hen delegation.
//!
//! Coop's farm is a *flock* of Hens. Delegation lets one Hen — acting as a
//! "manager" — hand a subtask to another Hen on the **same farm** and wait for
//! its result, turning the flock into an org chart (the lowest level of the
//! "organization" tier of agent systems).
//!
//! This is the in-process, open-source counterpart to cross-farm market
//! leasing: it never crosses the farm boundary and has no market awareness.
//!
//! Mechanism: delegation submits a normal [`crate::Job`] to the target Hen
//! (so it runs through the same runner, sandbox, and memory machinery) and the
//! caller polls for the terminal result. To stay safe it is **governed**:
//!
//! * A Hen may not delegate to itself ([`validate_delegation`]).
//! * Delegation chains are depth-capped at [`MAX_DELEGATION_DEPTH`], which also
//!   breaks accidental cycles (A→B→A increments depth each hop).
//! * The `delegate` tool is opt-in per Hen via the manifest `tools:` list.
//! * Every delegation emits an audit event (`OrchEvent::Delegated`).

use std::time::Duration;

use async_trait::async_trait;

use crate::error::{CoreError, Result};
use crate::ids::HenId;
use crate::job::JobStatus;

/// Maximum delegation depth. A job submitted directly by a farmer is depth 0;
/// each delegation hop adds one. A hop that would exceed this is refused, which
/// also bounds fan-out recursion and breaks cycles.
pub const MAX_DELEGATION_DEPTH: u32 = 3;

/// A request to delegate a subtask from one Hen to another.
#[derive(Debug, Clone)]
pub struct DelegationRequest {
    /// The delegating ("manager") Hen.
    pub from: HenId,
    /// The Hen that should perform the subtask.
    pub to: HenId,
    /// The subtask prompt.
    pub prompt: String,
    /// Delegation depth of the *calling* job (the sub-job runs at `+1`).
    pub parent_depth: u32,
    /// How long the caller will wait for a terminal result before giving up.
    pub timeout: Duration,
}

/// The result of a completed (or failed) delegation.
#[derive(Debug, Clone)]
pub struct DelegationOutcome {
    /// The sub-job that was created on the target Hen.
    pub job_id: String,
    /// Terminal status of the sub-job.
    pub status: JobStatus,
    /// The sub-job's result text (on success) or error (on failure).
    pub output: String,
    /// Delegation depth at which the sub-job ran.
    pub depth: u32,
}

/// Validate a prospective delegation hop. `next_depth` is the depth the
/// sub-job would run at (i.e. caller depth + 1).
///
/// # Errors
///
/// Returns [`CoreError::Other`] if a Hen delegates to itself, or if
/// `next_depth` exceeds [`MAX_DELEGATION_DEPTH`].
pub fn validate_delegation(from: &HenId, to: &HenId, next_depth: u32) -> Result<()> {
    if from == to {
        return Err(CoreError::Other(
            "a hen cannot delegate to itself".to_string(),
        ));
    }
    if next_depth > MAX_DELEGATION_DEPTH {
        return Err(CoreError::Other(format!(
            "delegation depth {next_depth} exceeds the maximum of {MAX_DELEGATION_DEPTH}"
        )));
    }
    Ok(())
}

/// Abstraction the `delegate` tool uses to reach the farm orchestrator without
/// depending on the daemon crate. Implemented by `coopd`'s `OrchHandle`.
#[async_trait]
pub trait Delegator: Send + Sync {
    /// Dispatch a subtask to another Hen and wait (up to `req.timeout`) for its
    /// terminal result.
    ///
    /// # Errors
    ///
    /// Returns an error if the delegation is invalid (see
    /// [`validate_delegation`]), the target Hen does not exist, the
    /// orchestrator channel is closed, or the wait times out. A sub-job that
    /// runs but *fails* is reported as `Ok` with a failed
    /// [`DelegationOutcome`], not an error.
    async fn delegate(&self, req: DelegationRequest) -> Result<DelegationOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CoopId;

    fn hen(name: &str) -> HenId {
        let coop = CoopId::new("alice.coop").unwrap();
        HenId::new(&coop, name).unwrap()
    }

    #[test]
    fn rejects_self_delegation() {
        let a = hen("aria");
        let err = validate_delegation(&a, &a, 1).unwrap_err();
        assert!(format!("{err}").contains("itself"));
    }

    #[test]
    fn allows_within_depth_limit() {
        let a = hen("aria");
        let b = hen("bolt");
        for d in 1..=MAX_DELEGATION_DEPTH {
            assert!(
                validate_delegation(&a, &b, d).is_ok(),
                "depth {d} should pass"
            );
        }
    }

    #[test]
    fn rejects_beyond_depth_limit() {
        let a = hen("aria");
        let b = hen("bolt");
        let err = validate_delegation(&a, &b, MAX_DELEGATION_DEPTH + 1).unwrap_err();
        assert!(format!("{err}").contains("exceeds the maximum"));
    }
}
