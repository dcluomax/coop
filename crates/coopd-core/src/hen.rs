//! Hen (agent) data model and lifecycle state machine.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{CoreError, Result};
use crate::ids::{HenId, RoostId};
use crate::manifest::AgentManifest;

/// Lifecycle states of a Hen.
///
/// See `docs/coop-l1-os.md` for the full state diagram.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HenState {
    /// Manifest on disk; never hatched.
    Defined,
    /// Currently being booted (container/process spinning up).
    Hatching,
    /// Booted, no active task.
    Idle,
    /// Actively executing a job.
    Working,
    /// Currently leased out to another farmer.
    Leased,
    /// Process down, state persisted.
    Sleeping,
    /// 90 days without activity — XP decay begins.
    Dormant,
    /// Permanently archived; read-only, memory may be inherited.
    Archived,
}

impl HenState {
    /// Returns true iff `self -> next` is a valid lifecycle transition.
    #[must_use]
    pub fn can_transition_to(self, next: HenState) -> bool {
        use HenState::*;
        matches!(
            (self, next),
            (Defined, Hatching)
                | (Hatching, Idle)
                | (Hatching, Defined)
                | (Idle, Working)
                | (Idle, Sleeping)
                | (Idle, Leased)
                | (Working, Idle)
                | (Working, Sleeping)
                | (Leased, Idle)
                | (Sleeping, Idle)
                | (Sleeping, Dormant)
                | (Idle, Dormant)
                | (Dormant, Idle)
                | (Dormant, Archived)
                | (Archived, Archived) // idempotent
        )
    }
}

/// Lease status of a Hen.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeaseStatus {
    /// Hen is operated by its owner (default).
    Owner,
    /// Hen is leased out to a remote farmer.
    LeasedOut {
        /// Remote renter Coop ID.
        renter: String,
        /// Lease identifier.
        lease_id: String,
    },
    /// Hen is leased in from a remote farmer.
    LeasedIn {
        /// Remote provider Coop ID.
        provider: String,
        /// Lease identifier.
        lease_id: String,
    },
}

impl Default for LeaseStatus {
    fn default() -> Self {
        Self::Owner
    }
}

/// XP and level data for a Hen.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HenStats {
    /// Total XP accumulated.
    pub xp: u64,
    /// Current level (derived from XP via leveling curve).
    pub level: u32,
    /// Lifetime quest count.
    pub quests_completed: u64,
    /// Lifetime Grain earned for the owner.
    pub grain_earned: u64,
}

/// Lineage / inheritance info.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lineage {
    /// Parent HenId if memory was inherited.
    pub parent: Option<String>,
    /// Generation count (1 = original).
    pub generation: u32,
}

/// Full Hen record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hen {
    /// Globally unique identifier.
    pub id: HenId,
    /// Current lifecycle state.
    pub state: HenState,
    /// Lease status.
    pub lease: LeaseStatus,
    /// User-provided manifest (agent.yaml contents).
    pub manifest: AgentManifest,
    /// Currently assigned Roost (if any).
    pub assigned_roost: Option<RoostId>,
    /// Stats / progression.
    pub stats: HenStats,
    /// Lineage / inheritance.
    pub lineage: Lineage,
    /// Creation timestamp (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last state change.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Tags / labels (free-form, for filtering).
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

impl Hen {
    /// Create a new Hen from a parsed manifest.
    pub fn new(id: HenId, manifest: AgentManifest) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id,
            state: HenState::Defined,
            lease: LeaseStatus::default(),
            manifest,
            assigned_roost: None,
            stats: HenStats::default(),
            lineage: Lineage::default(),
            created_at: now,
            updated_at: now,
            tags: HashMap::new(),
        }
    }

    /// Attempt to transition this Hen's state. Returns `Err` if illegal.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidTransition`] if the requested next state
    /// is not reachable from the current state per the lifecycle FSM.
    pub fn transition(&mut self, next: HenState) -> Result<()> {
        if !self.state.can_transition_to(next) {
            return Err(CoreError::InvalidTransition {
                from: format!("{:?}", self.state),
                to: format!("{next:?}"),
            });
        }
        self.state = next;
        self.updated_at = OffsetDateTime::now_utc();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CoopId;

    fn make_hen() -> Hen {
        let coop = CoopId::new("alice.coop").unwrap();
        let id = HenId::new(&coop, "aria").unwrap();
        let manifest = AgentManifest::minimal("aria".to_string());
        Hen::new(id, manifest)
    }

    #[test]
    fn lifecycle_happy_path() {
        let mut hen = make_hen();
        assert_eq!(hen.state, HenState::Defined);
        hen.transition(HenState::Hatching).unwrap();
        hen.transition(HenState::Idle).unwrap();
        hen.transition(HenState::Working).unwrap();
        hen.transition(HenState::Idle).unwrap();
        hen.transition(HenState::Sleeping).unwrap();
    }

    #[test]
    fn illegal_transition_rejected() {
        let mut hen = make_hen();
        // Cannot go Defined -> Working directly.
        assert!(hen.transition(HenState::Working).is_err());
    }

    #[test]
    fn archive_terminal() {
        let mut hen = make_hen();
        hen.state = HenState::Dormant;
        hen.transition(HenState::Archived).unwrap();
        // Archived cannot go elsewhere (except idempotent self-transition).
        assert!(hen.transition(HenState::Idle).is_err());
        assert!(hen.transition(HenState::Archived).is_ok());
    }
}
