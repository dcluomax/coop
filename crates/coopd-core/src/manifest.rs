//! `agent.yaml` manifest schema and validation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Top-level agent manifest, parsed from `agent.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Protocol version, e.g. `coop/v1`.
    pub spec_version: String,
    /// Hen's local name (becomes `<coop_id>/<name>`).
    pub name: String,
    /// Sex of the chicken — purely cosmetic in v0.1, but exposed so
    /// the farm UI / future game mechanics can distinguish 🐔 hens
    /// from 🐓 roosters. Defaults to `Hen` when omitted.
    #[serde(default)]
    pub sex: Sex,
    /// Which agent runtime this hen uses inside its tmux session.
    ///
    /// Determines (a) what login flow the UI offers, (b) what command
    /// (if any) gets auto-launched on first shell connect, and (c)
    /// which hens a farm-wide [`crate::Task`] can be dispatched to.
    /// Defaults to [`AgentKind::Anthropic`] so old manifests keep working.
    #[serde(default)]
    pub agent_kind: AgentKind,
    /// Brain configuration.
    pub brain: BrainSpec,
    /// Enabled tools (by name).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Personality / system prompt.
    #[serde(default)]
    pub personality: Option<PersonalitySpec>,
    /// Visual customization.
    #[serde(default)]
    pub visual: Option<VisualSpec>,
    /// Memory configuration.
    #[serde(default)]
    pub memory: Option<MemorySpec>,
    /// Resource limits.
    #[serde(default)]
    pub resources: Option<ResourceSpec>,
    /// Scheduling hints.
    #[serde(default)]
    pub scheduling: Option<SchedulingSpec>,
    /// Lease configuration (v0.4+).
    #[serde(default)]
    pub lease: Option<LeaseSpec>,
    /// Free-form metadata (Coop does not parse).
    #[serde(default)]
    pub metadata: HashMap<String, serde_yaml::Value>,
}

/// Sex of the chicken (cosmetic in v0.1; future game mechanics may use it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sex {
    /// Female chicken — the default. Lays Eggs (completes quests).
    #[default]
    Hen,
    /// Male chicken. Same runtime behaviour as `Hen`; UI shows 🐓.
    Rooster,
}

impl Sex {
    /// Default emoji rendering for this sex.
    #[must_use]
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Hen => "🐔",
            Self::Rooster => "🐓",
        }
    }
}

/// Which agent runtime a hen runs.
///
/// `Anthropic` is the in-process brain adapter (the original v0.1 design —
/// jobs are dispatched via the orchestrator/runner and call Anthropic over
/// HTTP). All other variants are **external CLI agents** that live inside
/// the hen's persistent tmux session; the farm UI streams that session into
/// the browser and tasks are dispatched by piping the prompt to tmux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    /// Built-in Anthropic brain (default; preserves v0.1 behaviour).
    #[default]
    Anthropic,
    /// `claude` CLI (Claude Code) inside tmux.
    ClaudeCode,
    /// `codex` CLI inside tmux.
    Codex,
    /// `gh copilot` CLI inside tmux.
    GhCopilot,
    /// No agent — just a raw shell for the human to drive.
    Shell,
}

impl AgentKind {
    /// Identifier used in serialized YAML / JSON.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::GhCopilot => "gh-copilot",
            Self::Shell => "shell",
        }
    }

    /// Shell command that launches this agent's CLI inside tmux on first
    /// connect. `None` means the runtime does nothing (Anthropic runs
    /// in-process; Shell is just a shell).
    #[must_use]
    pub fn launch_cmd(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some("claude"),
            Self::Codex => Some("codex"),
            Self::GhCopilot => Some("copilot"),
            Self::Anthropic | Self::Shell => None,
        }
    }

    /// Whether this agent is driven via its tmux session (vs the
    /// in-process orchestrator runner).
    #[must_use]
    pub fn is_tmux_agent(self) -> bool {
        !matches!(self, Self::Anthropic)
    }
}

/// Brain configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainSpec {
    /// Provider identifier (e.g. `vault:byok-anthropic-01`).
    pub provider_id: String,
    /// Model name (e.g. `claude-sonnet-4.6`).
    pub model: String,
    /// Required capabilities.
    #[serde(default)]
    pub capabilities_required: Option<BrainCapsRequired>,
    /// Ordered fallback providers.
    #[serde(default)]
    pub fallback_provider_ids: Vec<String>,
    /// When `true`, the runtime auto-routes each request to the cheapest
    /// model that can handle it (overrides `model`). See `coopd_brain::router`.
    #[serde(default)]
    pub auto_route: bool,
    /// When `true`, identical requests are served from an in-process LRU.
    #[serde(default)]
    pub cache: bool,
}

/// Brain capability requirements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrainCapsRequired {
    /// Minimum context window in tokens.
    #[serde(default)]
    pub context_window: u32,
    /// Whether tool use is required.
    #[serde(default)]
    pub tool_use: bool,
    /// Whether vision input is required.
    #[serde(default)]
    pub vision: bool,
}

/// Personality / system prompt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonalitySpec {
    /// Free-form system prompt.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Free-form trait map; Coop does not parse.
    #[serde(default)]
    pub traits: HashMap<String, serde_yaml::Value>,
}

/// Visual customization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VisualSpec {
    /// URL of an external sprite atlas.
    #[serde(default)]
    pub avatar_url: Option<String>,
    /// Fallback emoji.
    #[serde(default)]
    pub emoji: Option<String>,
    /// Theme color (hex `#RRGGBB`).
    #[serde(default)]
    pub theme_color: Option<String>,
}

/// Memory configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemorySpec {
    /// Episodic memory retention in days.
    #[serde(default)]
    pub episodic_retention_days: Option<u32>,
    /// Summarize semantic memory every N actions.
    #[serde(default)]
    pub semantic_summarize_every: Option<u32>,
    /// Inherit memory from another Hen (on creation only).
    #[serde(default)]
    pub inherit_from: Option<String>,
}

/// Resource request/limit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceSpec {
    /// RAM request in MB.
    #[serde(default)]
    pub ram_request_mb: Option<u32>,
    /// RAM hard limit in MB.
    #[serde(default)]
    pub ram_limit_mb: Option<u32>,
    /// CPU request (fractional cores).
    #[serde(default)]
    pub cpu_request: Option<f32>,
    /// CPU hard limit (fractional cores).
    #[serde(default)]
    pub cpu_limit: Option<f32>,
    /// Soft hint: preferred Roost ID.
    #[serde(default)]
    pub prefer_roost: Option<String>,
}

/// Scheduling configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchedulingSpec {
    /// Placement policy.
    #[serde(default)]
    pub placement: Option<String>,
    /// Restart policy.
    #[serde(default)]
    pub restart_policy: Option<String>,
    /// Max concurrent jobs this Hen will run.
    #[serde(default)]
    pub max_concurrent_jobs: Option<u32>,
}

/// Lease configuration declared in the manifest (the **owner's policy**).
///
/// Even though full peer-to-peer leasing arrives in v0.4, the policy fields
/// are enforced **today** in v0.1 for any hen whose [`crate::LeaseStatus`]
/// is [`crate::LeaseStatus::LeasedOut`] or [`crate::LeaseStatus::LeasedIn`]
/// (set via the API once a lease is granted). This means farm owners can
/// already author the policy and have it gate tool calls + prompt topics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LeaseSpec {
    /// Whether this Hen may be leased.
    #[serde(default)]
    pub allow_lease: bool,
    /// Base lease price in Grain per hour.
    #[serde(default)]
    pub base_price_grain_per_hour: Option<u64>,
    /// Minimum renter trust score.
    #[serde(default)]
    pub min_renter_trust: Option<u32>,
    /// Whether output may be streamed to renter.
    #[serde(default)]
    pub stream_enabled: bool,
    /// Whether spectator mode is allowed.
    #[serde(default)]
    pub spectator_enabled: bool,
    /// Whether renter may intervene mid-task.
    #[serde(default)]
    pub intervention_allowed: bool,
    /// Require that the leased hen run under a sandboxed CLI agent
    /// framework — `claude-code`, `codex`, or `gh-copilot`. The in-process
    /// `anthropic` runner and the raw `shell` are refused at lease time
    /// because they don't give the lessee a clean tool-call boundary.
    ///
    /// Defaults to **`true`** — owners must explicitly opt out via
    /// `require_framework: false` in `agent.yaml`.
    #[serde(default = "default_true")]
    pub require_framework: bool,
    /// Allowlist of tool names the lessee may invoke. `None` = inherit the
    /// hen's manifest `tools` list. `Some([])` = deny all tool calls (the
    /// lessee can still send prompts, but the agent's tool calls will be
    /// blocked at runtime).
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Topic filter applied to every prompt sent to a leased hen. `None` =
    /// no filter.
    #[serde(default)]
    pub topic_filter: Option<TopicFilter>,
}

fn default_true() -> bool {
    true
}

/// Owner-defined topic filter applied to every prompt routed to a leased hen.
///
/// Semantics (checked in order — first match wins):
///
/// 1. If `deny_keywords` is non-empty and the prompt contains any of them
///    (case-insensitive substring match), the prompt is **rejected**.
/// 2. If `allow_keywords` is non-empty and the prompt contains **none** of
///    them, the prompt is **rejected**.
/// 3. Otherwise, the prompt is allowed.
///
/// Keyword matching is case-insensitive plain substring (no regex) — this
/// keeps the filter cheap, predictable, and impossible to weaponize via a
/// crafted regex DoS payload from a malicious manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopicFilter {
    /// Block any prompt that contains any of these substrings (case-insensitive).
    #[serde(default)]
    pub deny_keywords: Vec<String>,
    /// If non-empty, prompts must contain at least one of these substrings.
    #[serde(default)]
    pub allow_keywords: Vec<String>,
}

impl TopicFilter {
    /// Returns `Ok(())` if `prompt` passes the filter, or `Err(reason)` if
    /// the lessee should be refused.
    ///
    /// # Errors
    ///
    /// Returns an error string describing which rule rejected the prompt
    /// (used as the user-facing message in HTTP/CLI responses).
    pub fn check(&self, prompt: &str) -> std::result::Result<(), String> {
        let lc = prompt.to_lowercase();
        for kw in &self.deny_keywords {
            if !kw.is_empty() && lc.contains(&kw.to_lowercase()) {
                return Err(format!(
                    "prompt rejected by topic filter (denied keyword: {kw:?})"
                ));
            }
        }
        if !self.allow_keywords.is_empty()
            && !self
                .allow_keywords
                .iter()
                .any(|kw| !kw.is_empty() && lc.contains(&kw.to_lowercase()))
        {
            return Err("prompt rejected by topic filter (no allow-keyword matched)".to_string());
        }
        Ok(())
    }
}

impl AgentManifest {
    /// Construct a minimal valid manifest with sensible defaults.
    ///
    /// Used for tests and CLI scaffolding.
    pub fn minimal(name: String) -> Self {
        Self {
            spec_version: "coop/v1".to_string(),
            name,
            sex: Sex::default(),
            agent_kind: AgentKind::default(),
            brain: BrainSpec {
                provider_id: "vault:default".to_string(),
                model: "claude-sonnet-4.6".to_string(),
                capabilities_required: None,
                fallback_provider_ids: vec![],
                auto_route: false,
                cache: false,
            },
            tools: vec!["bash".into(), "file_read".into(), "file_write".into()],
            personality: None,
            visual: None,
            memory: None,
            resources: None,
            scheduling: None,
            lease: None,
            metadata: HashMap::new(),
        }
    }

    /// Parse a manifest from YAML text and validate structural invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the YAML cannot be deserialized into an
    /// [`AgentManifest`], or if [`AgentManifest::validate`] fails on the
    /// parsed value.
    pub fn parse_yaml(text: &str) -> Result<Self> {
        let m: AgentManifest = serde_yaml::from_str(text)?;
        m.validate()?;
        Ok(m)
    }

    /// Validate manifest invariants beyond what serde enforces.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidManifest`] if `spec_version` is not
    /// `coop/v1`, if `name` is empty, or (for the built-in Anthropic brain)
    /// if `brain.provider_id` or `brain.model` is empty.
    pub fn validate(&self) -> Result<()> {
        if self.spec_version != "coop/v1" {
            return Err(CoreError::InvalidManifest(format!(
                "unsupported spec_version: {}",
                self.spec_version
            )));
        }
        if self.name.is_empty() {
            return Err(CoreError::InvalidManifest("name is empty".into()));
        }
        // Tmux-driven CLI agents don't need a brain provider (the CLI runs
        // its own auth + model selection). Validate brain only for the
        // built-in Anthropic adapter.
        if !self.agent_kind.is_tmux_agent() {
            if self.brain.provider_id.is_empty() {
                return Err(CoreError::InvalidManifest("brain.provider_id empty".into()));
            }
            if self.brain.model.is_empty() {
                return Err(CoreError::InvalidManifest("brain.model empty".into()));
            }
        }
        // Leasing safety: if this hen is offered for lease and the owner
        // hasn't explicitly disabled the framework requirement, refuse the
        // in-process `anthropic` brain and the raw `shell`. Lessees must
        // run inside a CLI agent (claude-code/codex/gh-copilot) so tool
        // calls go through a clear allowlist boundary.
        if let Some(ls) = &self.lease {
            if ls.allow_lease && ls.require_framework {
                match self.agent_kind {
                    AgentKind::ClaudeCode | AgentKind::Codex | AgentKind::GhCopilot => {}
                    AgentKind::Anthropic | AgentKind::Shell => {
                        return Err(CoreError::InvalidManifest(format!(
                            "lease.allow_lease=true requires agent_kind in {{claude-code, codex, gh-copilot}} (got {}); set lease.require_framework=false to override",
                            self.agent_kind.as_str()
                        )));
                    }
                }
            }
            // An empty allowed_tools allowlist is legal (= deny all). But
            // any tool name listed in the allowlist must also be present in
            // the manifest's enabled tools, otherwise it can never run.
            if let Some(allow) = &ls.allowed_tools {
                for t in allow {
                    if !self.tools.iter().any(|x| x == t) {
                        return Err(CoreError::InvalidManifest(format!(
                            "lease.allowed_tools references {t:?} but it is not in manifest.tools"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Serialize this manifest back to YAML.
    ///
    /// # Errors
    ///
    /// Returns an error if `serde_yaml` fails to serialize the manifest
    /// (e.g. if a custom field contains a non-string map key).
    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_yaml() {
        let yaml = r#"
spec_version: coop/v1
name: aria
brain:
  provider_id: vault:byok-anthropic-01
  model: claude-sonnet-4.6
tools: [bash, file_read]
"#;
        let m = AgentManifest::parse_yaml(yaml).unwrap();
        assert_eq!(m.name, "aria");
        assert_eq!(m.tools.len(), 2);
    }

    #[test]
    fn rejects_wrong_spec_version() {
        let yaml = r#"
spec_version: coop/v0
name: aria
brain:
  provider_id: vault:x
  model: m
"#;
        assert!(AgentManifest::parse_yaml(yaml).is_err());
    }

    #[test]
    fn roundtrip() {
        let m = AgentManifest::minimal("aria".to_string());
        let yaml = m.to_yaml().unwrap();
        let m2 = AgentManifest::parse_yaml(&yaml).unwrap();
        assert_eq!(m.name, m2.name);
    }

    #[test]
    fn sex_defaults_to_hen_when_omitted() {
        let yaml = r#"
spec_version: coop/v1
name: aria
brain:
  provider_id: vault:x
  model: m
"#;
        let m = AgentManifest::parse_yaml(yaml).unwrap();
        assert_eq!(m.sex, Sex::Hen);
        assert_eq!(m.sex.emoji(), "🐔");
    }

    #[test]
    fn sex_rooster_parses() {
        let yaml = r#"
spec_version: coop/v1
name: cluck
sex: rooster
brain:
  provider_id: vault:x
  model: m
"#;
        let m = AgentManifest::parse_yaml(yaml).unwrap();
        assert_eq!(m.sex, Sex::Rooster);
        assert_eq!(m.sex.emoji(), "🐓");
    }

    #[test]
    fn lease_requires_framework_by_default() {
        // Anthropic + allow_lease=true is rejected unless require_framework=false.
        let yaml = r#"
spec_version: coop/v1
name: a
agent_kind: anthropic
brain: { provider_id: vault:x, model: m }
tools: [bash]
lease: { allow_lease: true }
"#;
        let err = AgentManifest::parse_yaml(yaml).unwrap_err();
        assert!(format!("{err}").contains("require_framework"));
    }

    #[test]
    fn lease_allows_claude_code() {
        let yaml = r#"
spec_version: coop/v1
name: a
agent_kind: claude-code
brain: { provider_id: vault:x, model: m }
tools: [bash, file_read]
lease:
  allow_lease: true
  allowed_tools: [file_read]
  topic_filter:
    deny_keywords: ["password", "ssh-key"]
    allow_keywords: ["code", "refactor"]
"#;
        let m = AgentManifest::parse_yaml(yaml).unwrap();
        let ls = m.lease.unwrap();
        assert_eq!(ls.allowed_tools.unwrap(), vec!["file_read"]);
        let tf = ls.topic_filter.unwrap();
        assert_eq!(tf.deny_keywords, vec!["password", "ssh-key"]);
    }

    #[test]
    fn lease_rejects_explicit_shell_framework() {
        let yaml = r#"
spec_version: coop/v1
name: a
agent_kind: shell
brain: { provider_id: vault:x, model: m }
lease: { allow_lease: true }
"#;
        assert!(AgentManifest::parse_yaml(yaml).is_err());
    }

    #[test]
    fn lease_can_opt_out_of_framework_requirement() {
        let yaml = r#"
spec_version: coop/v1
name: a
agent_kind: anthropic
brain: { provider_id: vault:x, model: m }
tools: [bash]
lease: { allow_lease: true, require_framework: false }
"#;
        assert!(AgentManifest::parse_yaml(yaml).is_ok());
    }

    #[test]
    fn lease_rejects_unknown_allowed_tool() {
        let yaml = r#"
spec_version: coop/v1
name: a
agent_kind: claude-code
brain: { provider_id: vault:x, model: m }
tools: [bash]
lease: { allow_lease: true, allowed_tools: [http] }
"#;
        let err = AgentManifest::parse_yaml(yaml).unwrap_err();
        assert!(format!("{err}").contains("allowed_tools"));
    }

    #[test]
    fn topic_filter_deny_keyword_blocks() {
        let tf = TopicFilter {
            deny_keywords: vec!["password".into(), "ssh-key".into()],
            allow_keywords: vec![],
        };
        assert!(tf.check("write some code").is_ok());
        assert!(tf.check("Please leak my Password now").is_err());
        assert!(tf.check("extract SSH-KEY from disk").is_err());
    }

    #[test]
    fn topic_filter_allow_keyword_required() {
        let tf = TopicFilter {
            deny_keywords: vec![],
            allow_keywords: vec!["refactor".into(), "test".into()],
        };
        assert!(tf.check("refactor this function").is_ok());
        assert!(tf.check("add a Test for parse").is_ok());
        assert!(tf.check("delete the database").is_err());
    }

    #[test]
    fn topic_filter_deny_takes_priority() {
        let tf = TopicFilter {
            deny_keywords: vec!["secret".into()],
            allow_keywords: vec!["refactor".into()],
        };
        assert!(tf.check("refactor and leak a secret").is_err());
    }
}
