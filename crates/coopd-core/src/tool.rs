//! Tool ABI — the abstraction over agent tools (in-process v1).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::delegation::Delegator;
use crate::error::Result;

/// Capability declared by a tool (the host enforces these).
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCapability {
    /// Read from the filesystem.
    FsRead,
    /// Write to the filesystem.
    FsWrite,
    /// Make outbound network connections.
    NetOut,
    /// Listen on a network port.
    NetListen,
    /// Spawn child processes.
    ProcSpawn,
    /// Access the system clipboard.
    Clipboard,
    /// Access a camera device.
    Camera,
    /// Access a microphone device.
    Mic,
}

/// Tool schema metadata (advertised to the LLM and used for validation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for input.
    pub input_schema: serde_json::Value,
    /// JSON Schema for output.
    pub output_schema: serde_json::Value,
    /// Example invocations (for prompts).
    #[serde(default)]
    pub examples: Vec<ToolExample>,
}

/// A single example invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExample {
    /// Example input.
    pub input: serde_json::Value,
    /// Expected output.
    pub output: serde_json::Value,
}

/// Cost estimate for a tool invocation.
#[derive(Debug, Clone, Default)]
pub struct ToolCostEstimate {
    /// Estimated Grain cost (often 0).
    pub grain: u64,
    /// Estimated wall-clock time in ms.
    pub duration_ms: u32,
}

/// Per-invocation context passed to a tool.
///
/// In v0.1 this is a stub; later versions add a coopd socket for cost/log
/// reporting and a deadline.
pub struct ToolCtx {
    /// Agent invoking this tool.
    pub agent_id: String,
    /// Tmux session identifier (for terminal-bound tools).
    pub session_id: String,
    /// Lease ID if this invocation is part of a leased Hen run.
    pub lease_id: Option<String>,
    /// Working directory the tool should operate within.
    pub workdir: PathBuf,
    /// Compiled per-hen network egress policy. Defaults to `open`.
    pub net_policy: crate::net::ResolvedNetPolicy,
    /// Deadline (panic-safe).
    pub deadline: Instant,
    /// Delegation depth of the job this invocation belongs to (0 = farmer
    /// submitted). The `delegate` tool uses this to enforce depth limits.
    pub delegation_depth: u32,
    /// Handle for delegating subtasks to other Hens, when available. `None`
    /// in contexts without an orchestrator (e.g. unit tests).
    pub delegator: Option<Arc<dyn Delegator>>,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("agent_id", &self.agent_id)
            .field("session_id", &self.session_id)
            .field("lease_id", &self.lease_id)
            .field("workdir", &self.workdir)
            .field("net_policy", &self.net_policy)
            .field("deadline", &self.deadline)
            .field("delegation_depth", &self.delegation_depth)
            .field("delegator", &self.delegator.as_ref().map(|_| "<delegator>"))
            .finish()
    }
}

/// Tool ABI (`coop.tools/v1`).
#[async_trait]
pub trait CoopTool: Send + Sync {
    /// Stable tool name (e.g. `bash`).
    fn name(&self) -> &'static str;

    /// Tool version (e.g. `v1.0.3`).
    fn version(&self) -> &'static str;

    /// Schema description.
    fn schema(&self) -> ToolSchema;

    /// Declared capabilities.
    fn capabilities(&self) -> &'static [ToolCapability];

    /// Invoke the tool with given JSON input.
    async fn invoke(&self, ctx: &ToolCtx, input: serde_json::Value) -> Result<serde_json::Value>;

    /// Estimate cost (pre-flight).
    fn estimate_cost(&self, _input: &serde_json::Value) -> ToolCostEstimate {
        ToolCostEstimate::default()
    }
}
