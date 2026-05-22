//! # coopd-tools
//!
//! Built-in tools for the Coop v0.1 ALONE FARMER runtime.
//!
//! Tools live in the same process as `coopd` (in-process ABI). Each tool
//! implements [`CoopTool`] from `coopd-core`. The [`Registry`] type lets the
//! orchestrator advertise/execute tools by name.

#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;

use coopd_core::{CoopTool, ToolCapability};

pub mod bash;
pub mod file_read;
pub mod file_write;
pub mod http;
pub mod safe_net;
pub mod safe_path;

/// Built-in tool registry.
#[derive(Clone, Default)]
pub struct Registry {
    tools: HashMap<String, Arc<dyn CoopTool>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Registry {
    /// New, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-populated with all v0.1 built-ins.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(bash::Bash));
        r.register(Arc::new(file_read::FileRead));
        r.register(Arc::new(file_write::FileWrite));
        r.register(Arc::new(http::Http::new()));
        r
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Arc<dyn CoopTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn CoopTool>> {
        self.tools.get(name).cloned()
    }

    /// Iterate over tool names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// Render schema for a subset of tools (for the brain adapter).
    #[must_use]
    pub fn schemas_for(&self, enabled: &[String]) -> Vec<ToolEntry> {
        enabled
            .iter()
            .filter_map(|n| self.tools.get(n))
            .map(|t| ToolEntry {
                name: t.name().to_string(),
                description: t.schema().description,
                input_schema: t.schema().input_schema,
                capabilities: t.capabilities().to_vec(),
            })
            .collect()
    }
}

/// A tool's advertised metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolEntry {
    /// Tool name.
    pub name: String,
    /// Description.
    pub description: String,
    /// JSON Schema for input.
    pub input_schema: serde_json::Value,
    /// Declared capabilities.
    pub capabilities: Vec<ToolCapability>,
}

#[cfg(test)]
pub(crate) fn test_ctx(workdir: std::path::PathBuf) -> coopd_core::ToolCtx {
    coopd_core::ToolCtx {
        agent_id: "alice.coop/aria".into(),
        session_id: "test".into(),
        lease_id: None,
        workdir,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
    }
}
