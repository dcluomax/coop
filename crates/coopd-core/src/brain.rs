//! Brain adapter trait — the abstraction over LLM providers (T1-T4).

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Brain provider tier classification.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Foundation-managed inference (reseller, OAuth).
    Managed,
    /// Bring-Your-Own-Key (farmer's vendor key).
    Byok,
    /// Bring-Your-Own-Model (local llama/vllm/ollama HTTP).
    Byom,
    /// CLI channel (Claude Code, Codex, Copilot CLI subprocess).
    Cli,
}

/// Capabilities reported by a brain provider.
#[derive(Debug, Clone, Default)]
pub struct BrainCaps {
    /// Maximum context window in tokens.
    pub context_window: u32,
    /// Supports tool use.
    pub tool_use: bool,
    /// Supports vision input.
    pub vision: bool,
    /// Supports streaming tool calls.
    pub streaming_tools: bool,
    /// Maximum output tokens per response.
    pub max_output_tokens: u32,
    /// Free-form pricing tier label (for budget matching).
    pub pricing_tier: String,
}

/// A single chat message turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Role: `user`, `assistant`, `system`, `tool`.
    pub role: String,
    /// Message content: either a plain string or a sequence of structured
    /// blocks (carrying `tool_use` / `tool_result` for multi-turn tool calls).
    pub content: MessageContent,
}

/// Body of a [`Message`].
///
/// Serialized **untagged** so a plain-text turn round-trips as a JSON string
/// (`"content": "hi"`) and a structured turn as a JSON array
/// (`"content": [ {...}, {...} ]`). This keeps wire compatibility with the
/// v0.1 string-only representation while enabling structured tool blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Plain text content.
    Text(String),
    /// A sequence of structured content blocks.
    Blocks(Vec<ContentBlock>),
}

impl From<String> for MessageContent {
    fn from(s: String) -> Self {
        MessageContent::Text(s)
    }
}

impl From<&str> for MessageContent {
    fn from(s: &str) -> Self {
        MessageContent::Text(s.to_string())
    }
}

impl From<Vec<ContentBlock>> for MessageContent {
    fn from(b: Vec<ContentBlock>) -> Self {
        MessageContent::Blocks(b)
    }
}

impl MessageContent {
    /// A flat text view of this content, concatenating the textual payload of
    /// every block. Used for length/keyword heuristics and logging — not for
    /// wire serialization (which goes through each provider adapter).
    #[must_use]
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => {
                let mut out = String::new();
                for b in blocks {
                    match b {
                        ContentBlock::Text { text } | ContentBlock::Thinking { text } => {
                            out.push_str(text);
                        }
                        ContentBlock::ToolCall { name, input, .. } => {
                            out.push_str(name);
                            out.push(' ');
                            out.push_str(&input.to_string());
                        }
                        ContentBlock::ToolResult { content, .. } => out.push_str(content),
                    }
                    out.push('\n');
                }
                out
            }
        }
    }

    /// Character length of the flattened textual content.
    #[must_use]
    pub fn text_len(&self) -> usize {
        self.as_text().chars().count()
    }
}

/// One block of generated or replayed content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text output.
    Text {
        /// Body.
        text: String,
    },
    /// A tool invocation request.
    ToolCall {
        /// Provider-assigned tool-use id, threaded back on the matching
        /// [`ContentBlock::ToolResult`] so multi-turn tool conversations
        /// stay correlated. May be empty for providers that omit ids.
        #[serde(default)]
        id: String,
        /// Tool name.
        name: String,
        /// JSON-encoded input.
        input: serde_json::Value,
    },
    /// The result of a previously-requested tool call, fed back to the model.
    ToolResult {
        /// The [`ContentBlock::ToolCall::id`] this result answers.
        tool_use_id: String,
        /// Tool output (serialized JSON or an error string).
        content: String,
        /// Whether `content` represents a tool failure.
        #[serde(default)]
        is_error: bool,
    },
    /// Reasoning trace (not always available).
    Thinking {
        /// Reasoning body.
        text: String,
    },
}

/// Token usage summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens consumed.
    pub input_tokens: u32,
    /// Output tokens produced.
    pub output_tokens: u32,
    /// Tokens served from prompt cache.
    pub cache_tokens: u32,
}

/// Cost of a single LLM call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cost {
    /// Coop-internal Grain cost.
    pub grain: u64,
    /// USD cost (informational).
    pub usd_micros: u64,
}

/// Reasoning request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasonRequest {
    /// System prompt.
    pub system: String,
    /// Chat history.
    pub messages: Vec<Message>,
    /// Tool schemas (advertised to the model).
    #[serde(default)]
    pub tools: Vec<serde_json::Value>,
    /// Sampling temperature.
    pub temperature: f32,
    /// Output token cap.
    pub max_tokens: u32,
    /// Stop sequences.
    #[serde(default)]
    pub stop_seq: Vec<String>,
    /// Stream tokens incrementally.
    #[serde(default)]
    pub stream: bool,
    /// Free-form metadata (hen_id, lease_id, etc.).
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Reasoning response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasonResponse {
    /// Output content blocks.
    pub content: Vec<ContentBlock>,
    /// Token usage.
    pub usage: Usage,
    /// Cost summary.
    pub cost: Cost,
    /// Reason for stop (`end_turn`, `tool_use`, `max_tokens`, etc.).
    pub finish_reason: String,
    /// Round-trip latency in ms.
    pub latency_ms: u32,
}

/// Streaming chunk (for `BrainAdapter::stream`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasonChunk {
    /// A partial text token.
    Text {
        /// Token string.
        delta: String,
    },
    /// A complete final response (sent last).
    Final {
        /// Final response.
        response: ReasonResponse,
    },
    /// An error mid-stream.
    Error {
        /// Error message.
        message: String,
    },
}

/// Cost estimate (pre-flight).
#[derive(Debug, Clone, Default)]
pub struct CostEstimate {
    /// Estimated Grain cost.
    pub grain: u64,
    /// Estimated USD micro-dollars.
    pub usd_micros: u64,
    /// Confidence label.
    pub confidence: String,
}

/// Brain adapter trait — implemented by each LLM provider integration.
#[async_trait]
pub trait BrainAdapter: Send + Sync {
    /// Provider identifier (e.g. `anthropic`, `openai`, `local-llama`).
    fn name(&self) -> &str;

    /// Provider tier.
    fn tier(&self) -> Tier;

    /// Declared capabilities.
    fn capabilities(&self) -> BrainCaps;

    /// Synchronous reasoning call.
    async fn reason(&self, req: ReasonRequest) -> Result<ReasonResponse>;

    /// Streaming reasoning call. Returns a stream of chunks ending in `Final` or `Error`.
    async fn stream(
        &self,
        req: ReasonRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ReasonChunk>>>;

    /// Estimate cost of a request without invoking the model.
    fn estimate_cost(&self, req: &ReasonRequest) -> CostEstimate;

    /// Health probe (e.g. ping API endpoint).
    async fn health_check(&self) -> Result<()>;
}
