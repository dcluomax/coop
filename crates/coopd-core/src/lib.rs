//! # coopd-core
//!
//! Core types, traits, and orchestration logic for the Coop agent farm OS.
//!
//! This crate contains:
//! - Identity types (`CoopId`, `HenId`, `RoostId`)
//! - `Hen` lifecycle state machine
//! - `agent.yaml` manifest schema
//! - Brain adapter trait (`BrainAdapter`)
//! - Tool ABI trait (`CoopTool`)
//! - Orchestrator command/event channels
//!
//! It deliberately has no I/O dependencies (no HTTP, no DB) so it can
//! be unit-tested in isolation and reused across `coopd` and `coop-cli`.

#![warn(missing_docs)]

pub mod brain;
pub mod error;
pub mod hen;
pub mod ids;
pub mod job;
pub mod manifest;
pub mod net;
pub mod orchestrator;
pub mod task;
pub mod tool;

pub use brain::{BrainAdapter, BrainCaps, ReasonRequest, ReasonResponse, Tier};
pub use error::{CoreError, Result};
pub use hen::{Hen, HenState, LeaseStatus};
pub use ids::{CoopId, HenId, RoostId};
pub use job::{Job, JobStatus};
pub use manifest::{AgentKind, AgentManifest};
pub use net::{NetAllow, NetPolicy, NetworkSpec, ResolvedNetPolicy};
pub use orchestrator::{OrchCmd, OrchEvent};
pub use task::{Task, TaskStatus};
pub use tool::{CoopTool, ToolCapability, ToolCtx, ToolSchema};
