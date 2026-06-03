//! OpenAI Chat Completions API adapter (BYOK / BYOM).
//!
//! Maps Coop's [`ReasonRequest`] / [`ReasonResponse`] onto the OpenAI
//! `/v1/chat/completions` HTTP API. Because the protocol is widely cloned, the
//! same adapter serves any OpenAI-compatible endpoint (llama.cpp, Ollama,
//! vLLM, LM Studio, …) by pointing [`OpenAi::with_base_url`] at the local
//! server — that is the `openai-compat` provider.

use async_trait::async_trait;
use coopd_core::{
    BrainAdapter, BrainCaps, CoreError, ReasonRequest, ReasonResponse, Result, Tier,
    brain::{ContentBlock, Cost, CostEstimate, MessageContent, ReasonChunk, Usage},
};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::debug;
use zeroize::Zeroizing;

/// OpenAI Chat Completions adapter.
///
/// The BYOK `api_key` is held in [`Zeroizing`] so its heap buffer is wiped when
/// the adapter (and every clone) is dropped, and the [`Debug`] impl redacts it
/// so the key never reaches logs or error messages.
#[derive(Clone)]
pub struct OpenAi {
    api_key: Zeroizing<String>,
    base_url: String,
    model: String,
    /// Provider label reported by [`BrainAdapter::name`] (`openai` or
    /// `openai-compat`).
    provider: &'static str,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAi")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl OpenAi {
    /// Construct an OpenAI adapter (`https://api.openai.com/v1` base URL).
    ///
    /// `api_key` is an `sk-...` key. `model` is e.g. `gpt-4o-mini`.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest::Client` builder fails (e.g. the
    /// platform has no TLS backend). Treated as unrecoverable at daemon
    /// startup, matching the Anthropic adapter.
    #[must_use]
    pub fn new(api_key: String, model: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .expect("reqwest client");
        Self {
            api_key: Zeroizing::new(api_key),
            base_url: "https://api.openai.com/v1".to_string(),
            model,
            provider: "openai",
            client,
        }
    }

    /// Override the base URL and report the `openai-compat` provider label.
    /// Use for local OpenAI-compatible servers (llama.cpp/Ollama/vLLM) or a
    /// proxy. A trailing `/v1` is expected, matching the OpenAI convention.
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self.provider = "openai-compat";
        self
    }
}

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct OpenAiResponse {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: OpenAiUsage,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Deserialize, Debug)]
struct OpenAiChoice {
    #[serde(default)]
    message: OpenAiMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCall>,
}

#[derive(Deserialize, Debug)]
struct OpenAiToolCall {
    #[serde(default)]
    id: String,
    #[serde(default)]
    function: OpenAiFunction,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: String,
}

#[async_trait]
impl BrainAdapter for OpenAi {
    fn name(&self) -> &str {
        self.provider
    }
    fn tier(&self) -> Tier {
        if self.provider == "openai-compat" {
            Tier::Byom
        } else {
            Tier::Byok
        }
    }
    fn capabilities(&self) -> BrainCaps {
        BrainCaps {
            context_window: 128_000,
            tool_use: true,
            vision: true,
            streaming_tools: true,
            max_output_tokens: 16_000,
            pricing_tier: format!("{}-standard", self.provider),
        }
    }

    async fn reason(&self, req: ReasonRequest) -> Result<ReasonResponse> {
        let start = std::time::Instant::now();
        let body = build_request(&self.model, &req);
        debug!(provider = self.provider, model = %self.model, msgs = req.messages.len(), "openai request");

        let mut rb = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("content-type", "application/json");
        // OpenAI-compatible local servers often need no auth; only send the
        // bearer header when a key is present.
        if !self.api_key.is_empty() {
            rb = rb.header("authorization", format!("Bearer {}", self.api_key.as_str()));
        }
        let resp = rb
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::Other(format!("{}: {e}", self.provider)))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| CoreError::Other(format!("{} body: {e}", self.provider)))?;
        if !status.is_success() {
            return Err(CoreError::Other(format!(
                "{} {status}: {text}",
                self.provider
            )));
        }
        let or: OpenAiResponse = serde_json::from_str(&text)
            .map_err(|e| CoreError::Other(format!("{} parse: {e} body={text}", self.provider)))?;

        let choice = or
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::Other(format!("{}: empty choices", self.provider)))?;

        let mut content: Vec<ContentBlock> = Vec::new();
        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                content.push(ContentBlock::Text { text });
            }
        }
        for tc in choice.message.tool_calls {
            // OpenAI tool arguments are a JSON *string*; decode to an object so
            // the rest of Coop sees the same shape as the Anthropic adapter.
            let input: Value = if tc.function.arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| Value::String(tc.function.arguments.clone()))
            };
            content.push(ContentBlock::ToolCall {
                id: tc.id,
                name: tc.function.name,
                input,
            });
        }

        Ok(ReasonResponse {
            content,
            usage: Usage {
                input_tokens: or.usage.prompt_tokens,
                output_tokens: or.usage.completion_tokens,
                cache_tokens: 0,
            },
            cost: Cost::default(),
            finish_reason: normalize_finish_reason(choice.finish_reason.as_deref()),
            latency_ms: start.elapsed().as_millis() as u32,
        })
    }

    async fn stream(&self, _req: ReasonRequest) -> Result<BoxStream<'static, Result<ReasonChunk>>> {
        Err(CoreError::Other("streaming not implemented in v0.1".into()))
    }

    fn estimate_cost(&self, _req: &ReasonRequest) -> CostEstimate {
        CostEstimate {
            grain: 0,
            usd_micros: 0,
            confidence: "none".into(),
        }
    }

    async fn health_check(&self) -> Result<()> {
        let probe = ReasonRequest {
            system: String::new(),
            messages: vec![coopd_core::brain::Message {
                role: "user".into(),
                content: "ping".into(),
            }],
            tools: vec![],
            temperature: 0.0,
            max_tokens: 1,
            stop_seq: vec![],
            stream: false,
            metadata: Default::default(),
        };
        self.reason(probe).await.map(|_| ())
    }
}

/// Map Coop's normalized stop reasons onto Anthropic-style strings so the
/// runner's `end_turn` short-circuit works uniformly across providers.
fn normalize_finish_reason(raw: Option<&str>) -> String {
    match raw {
        Some("tool_calls") => "tool_use".into(),
        Some("length") => "max_tokens".into(),
        Some("stop") | None => "end_turn".into(),
        Some(other) => other.to_string(),
    }
}

fn build_request<'a>(model: &'a str, req: &'a ReasonRequest) -> OpenAiRequest<'a> {
    let mut messages: Vec<Value> = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({ "role": "system", "content": req.system }));
    }
    for m in &req.messages {
        push_openai_messages(&mut messages, &m.role, &m.content);
    }

    // Coop advertises tools in Anthropic shape ({name, description,
    // input_schema}); translate to OpenAI's function-tool shape.
    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.get("name").cloned().unwrap_or(Value::Null),
                    "description": t.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({})),
                }
            })
        })
        .collect();

    OpenAiRequest {
        model,
        messages,
        tools,
        temperature: Some(req.temperature),
        max_tokens: Some(req.max_tokens),
        stop: req.stop_seq.clone(),
    }
}

/// Flatten one Coop message into one or more OpenAI messages. A structured
/// assistant turn becomes a single `assistant` message carrying `tool_calls`;
/// a structured user turn yields one `tool` message per `tool_result` block
/// (OpenAI's required shape).
fn push_openai_messages(out: &mut Vec<Value>, role: &str, content: &MessageContent) {
    match content {
        MessageContent::Text(s) => out.push(json!({ "role": role, "content": s })),
        MessageContent::Blocks(blocks) => {
            let mut text = String::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            for b in blocks {
                match b {
                    ContentBlock::Text { text: t } | ContentBlock::Thinking { text: t } => {
                        text.push_str(t);
                    }
                    ContentBlock::ToolCall { id, name, input } => tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": input.to_string() },
                    })),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => out.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": content,
                    })),
                }
            }
            if !tool_calls.is_empty() {
                out.push(json!({
                    "role": role,
                    "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                    "tool_calls": tool_calls,
                }));
            } else if !text.is_empty() {
                out.push(json!({ "role": role, "content": text }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::brain::Message;

    fn req_with(messages: Vec<Message>, tools: Vec<Value>) -> ReasonRequest {
        ReasonRequest {
            system: "sys".into(),
            messages,
            tools,
            temperature: 0.5,
            max_tokens: 256,
            stop_seq: vec![],
            stream: false,
            metadata: Default::default(),
        }
    }

    #[test]
    fn system_prompt_is_prepended() {
        let req = req_with(
            vec![Message {
                role: "user".into(),
                content: "hi".into(),
            }],
            vec![],
        );
        let body = build_request("gpt-4o-mini", &req);
        assert_eq!(body.messages[0]["role"], "system");
        assert_eq!(body.messages[0]["content"], "sys");
        assert_eq!(body.messages[1]["role"], "user");
        assert_eq!(body.messages[1]["content"], "hi");
    }

    #[test]
    fn tools_translate_to_function_shape() {
        let req = req_with(
            vec![],
            vec![json!({
                "name": "bash",
                "description": "run a shell command",
                "input_schema": { "type": "object" }
            })],
        );
        let body = build_request("m", &req);
        assert_eq!(body.tools[0]["type"], "function");
        assert_eq!(body.tools[0]["function"]["name"], "bash");
        assert_eq!(body.tools[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn structured_turns_flatten_to_openai_shape() {
        let assistant = Message {
            role: "assistant".into(),
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "checking".into(),
                },
                ContentBlock::ToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    input: json!({ "command": "ls" }),
                },
            ]),
        };
        let user = Message {
            role: "user".into(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "a.txt".into(),
                is_error: false,
            }]),
        };
        let req = req_with(vec![assistant, user], vec![]);
        let body = build_request("m", &req);

        // [0]=system, [1]=assistant(tool_calls), [2]=tool result
        let a = &body.messages[1];
        assert_eq!(a["role"], "assistant");
        assert_eq!(a["content"], "checking");
        assert_eq!(a["tool_calls"][0]["id"], "call_1");
        assert_eq!(a["tool_calls"][0]["type"], "function");
        assert_eq!(a["tool_calls"][0]["function"]["name"], "bash");
        // arguments must be a JSON string, not an object.
        assert!(a["tool_calls"][0]["function"]["arguments"].is_string());

        let t = &body.messages[2];
        assert_eq!(t["role"], "tool");
        assert_eq!(t["tool_call_id"], "call_1");
        assert_eq!(t["content"], "a.txt");
    }

    #[test]
    fn finish_reason_is_normalized() {
        assert_eq!(normalize_finish_reason(Some("stop")), "end_turn");
        assert_eq!(normalize_finish_reason(Some("tool_calls")), "tool_use");
        assert_eq!(normalize_finish_reason(Some("length")), "max_tokens");
        assert_eq!(normalize_finish_reason(None), "end_turn");
    }

    #[test]
    fn compat_base_url_sets_label_and_tier() {
        let a = OpenAi::new("k".into(), "m".into());
        assert_eq!(a.name(), "openai");
        assert!(matches!(a.tier(), Tier::Byok));
        let c = OpenAi::new("k".into(), "m".into()).with_base_url("http://localhost:11434/v1");
        assert_eq!(c.name(), "openai-compat");
        assert!(matches!(c.tier(), Tier::Byom));
    }

    #[test]
    fn debug_redacts_api_key() {
        let a = OpenAi::new("sk-secret-123".into(), "m".into());
        let dbg = format!("{a:?}");
        assert!(!dbg.contains("sk-secret-123"), "api key leaked: {dbg}");
        assert!(dbg.contains("redacted"));
    }
}
