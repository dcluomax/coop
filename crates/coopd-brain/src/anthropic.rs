//! Anthropic Messages API adapter (BYOK).
//!
//! Maps Coop's [`ReasonRequest`] / [`ReasonResponse`] to the Anthropic
//! `/v1/messages` HTTP API.

use async_trait::async_trait;
use coopd_core::{
    BrainAdapter, BrainCaps, CoreError, ReasonRequest, ReasonResponse, Result, Tier,
    brain::{ContentBlock, Cost, CostEstimate, Message, MessageContent, ReasonChunk, Usage},
};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::debug;
use zeroize::Zeroizing;

/// Anthropic Messages API adapter.
///
/// The BYOK `api_key` is held in [`Zeroizing`] so its heap buffer is wiped
/// when the adapter (and every clone) is dropped (M1), and the [`Debug`] impl
/// redacts it so the key never reaches logs or error messages.
#[derive(Clone)]
pub struct Anthropic {
    api_key: Zeroizing<String>,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Anthropic")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Anthropic {
    /// Construct an Anthropic adapter.
    ///
    /// `api_key` is a `sk-ant-...` key. `model` is e.g. `claude-sonnet-4-5-20250929`.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest::Client` builder fails (e.g. the
    /// platform has no TLS backend available). Treated as unrecoverable at
    /// daemon startup.
    pub fn new(api_key: String, model: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .expect("reqwest client");
        Self {
            api_key: Zeroizing::new(api_key),
            base_url: "https://api.anthropic.com".to_string(),
            model,
            client,
        }
    }

    /// Override the base URL (for tests / proxies).
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "stop_sequences")]
    stop_seq: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

#[derive(Deserialize, Debug)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicContent>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: AnthropicUsage,
}

#[derive(Deserialize, Debug, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },
}

// ---- Streaming (SSE) types -------------------------------------------------

enum AnthropicStreamState {
    Active {
        stream: BoxStream<'static, Result<String>>,
        acc: AnthropicStreamAcc,
    },
    Done,
}

#[derive(Deserialize, Debug)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    message: Option<AnthropicStreamMessage>,
    #[serde(default)]
    content_block: Option<AnthropicStreamBlock>,
    #[serde(default)]
    delta: Option<AnthropicStreamDelta>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    #[serde(default)]
    error: Option<AnthropicStreamError>,
}

#[derive(Deserialize, Debug, Default)]
struct AnthropicStreamMessage {
    #[serde(default)]
    usage: AnthropicUsage,
}

#[derive(Deserialize, Debug)]
struct AnthropicStreamBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Debug, Default)]
struct AnthropicStreamDelta {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct AnthropicStreamError {
    #[serde(default)]
    message: String,
}

/// One in-flight content block during streaming.
enum AnthBlock {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
}

/// Accumulates Anthropic SSE events (`message_start`, `content_block_*`,
/// `message_delta`) into a final [`ReasonResponse`]. `tool_use` blocks arrive
/// as a `content_block_start` then a run of `input_json_delta` fragments whose
/// `partial_json` is concatenated to form the arguments object.
#[derive(Default)]
struct AnthropicStreamAcc {
    started: Option<std::time::Instant>,
    blocks: Vec<AnthBlock>,
    stop_reason: Option<String>,
    input_tokens: u32,
    output_tokens: u32,
    cache_tokens: u32,
}

impl AnthropicStreamAcc {
    fn ensure(&mut self, idx: usize) {
        if idx >= self.blocks.len() {
            self.blocks
                .resize_with(idx + 1, || AnthBlock::Text(String::new()));
        }
    }

    /// Fold one event into the accumulator; returns a newly-arrived text delta
    /// (if any) for surfacing as [`ReasonChunk::Text`].
    fn ingest(&mut self, ev: AnthropicStreamEvent) -> Option<String> {
        if self.started.is_none() {
            self.started = Some(std::time::Instant::now());
        }
        match ev.kind.as_str() {
            "message_start" => {
                if let Some(m) = ev.message {
                    self.input_tokens = m.usage.input_tokens;
                    self.cache_tokens = m.usage.cache_read_input_tokens;
                    if m.usage.output_tokens > 0 {
                        self.output_tokens = m.usage.output_tokens;
                    }
                }
                None
            }
            "content_block_start" => {
                let idx = ev.index.unwrap_or(self.blocks.len());
                self.ensure(idx);
                self.blocks[idx] = match ev.content_block {
                    Some(cb) if cb.kind == "tool_use" => AnthBlock::Tool {
                        id: cb.id,
                        name: cb.name,
                        json: String::new(),
                    },
                    _ => AnthBlock::Text(String::new()),
                };
                None
            }
            "content_block_delta" => {
                let idx = ev.index.unwrap_or(0);
                self.ensure(idx);
                let d = ev.delta?;
                if let Some(t) = d.text {
                    if let AnthBlock::Text(s) = &mut self.blocks[idx] {
                        s.push_str(&t);
                    }
                    return Some(t);
                }
                if let Some(pj) = d.partial_json
                    && let AnthBlock::Tool { json, .. } = &mut self.blocks[idx]
                {
                    json.push_str(&pj);
                }
                None
            }
            "message_delta" => {
                if let Some(d) = ev.delta
                    && let Some(sr) = d.stop_reason
                {
                    self.stop_reason = Some(sr);
                }
                if let Some(u) = ev.usage {
                    self.output_tokens = u.output_tokens;
                }
                None
            }
            _ => None,
        }
    }

    fn finish(self) -> ReasonResponse {
        let mut content: Vec<ContentBlock> = Vec::new();
        for block in self.blocks {
            match block {
                AnthBlock::Text(text) if !text.is_empty() => {
                    content.push(ContentBlock::Text { text });
                }
                AnthBlock::Tool { id, name, json } if !name.is_empty() => {
                    let input: Value = if json.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&json).unwrap_or_else(|_| Value::String(json.clone()))
                    };
                    content.push(ContentBlock::ToolCall { id, name, input });
                }
                AnthBlock::Text(_) | AnthBlock::Tool { .. } => {}
            }
        }
        ReasonResponse {
            content,
            usage: Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_tokens: self.cache_tokens,
            },
            cost: Cost::default(),
            finish_reason: self.stop_reason.unwrap_or_else(|| "end_turn".into()),
            latency_ms: self.started.map_or(0, |s| s.elapsed().as_millis() as u32),
        }
    }
}

#[async_trait]
impl BrainAdapter for Anthropic {
    fn name(&self) -> &str {
        "anthropic"
    }
    fn tier(&self) -> Tier {
        Tier::Byok
    }
    fn capabilities(&self) -> BrainCaps {
        BrainCaps {
            context_window: 200_000,
            tool_use: true,
            vision: true,
            streaming_tools: true,
            max_output_tokens: 16_000,
            pricing_tier: "anthropic-standard".into(),
        }
    }

    async fn reason(&self, req: ReasonRequest) -> Result<ReasonResponse> {
        let start = std::time::Instant::now();
        let body = build_request(&self.model, &req);
        debug!(model = %self.model, msgs = req.messages.len(), "anthropic request");

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.api_key.as_str())
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::Other(format!("anthropic: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| CoreError::Other(format!("anthropic body: {e}")))?;
        if !status.is_success() {
            return Err(CoreError::Other(format!("anthropic {status}: {text}")));
        }
        let ar: AnthropicResponse = serde_json::from_str(&text)
            .map_err(|e| CoreError::Other(format!("anthropic parse: {e} body={text}")))?;

        let content = ar
            .content
            .into_iter()
            .map(|c| match c {
                AnthropicContent::Text { text } => ContentBlock::Text { text },
                AnthropicContent::ToolUse { id, name, input } => {
                    ContentBlock::ToolCall { id, name, input }
                }
                AnthropicContent::Thinking { thinking } => {
                    ContentBlock::Thinking { text: thinking }
                }
            })
            .collect();

        Ok(ReasonResponse {
            content,
            usage: Usage {
                input_tokens: ar.usage.input_tokens,
                output_tokens: ar.usage.output_tokens,
                cache_tokens: ar.usage.cache_read_input_tokens,
            },
            cost: Cost::default(),
            finish_reason: ar.stop_reason.unwrap_or_else(|| "end_turn".into()),
            latency_ms: start.elapsed().as_millis() as u32,
        })
    }

    async fn stream(&self, req: ReasonRequest) -> Result<BoxStream<'static, Result<ReasonChunk>>> {
        let body = build_request_inner(&self.model, &req, true);
        debug!(model = %self.model, "anthropic stream request");

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.api_key.as_str())
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::Other(format!("anthropic: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(CoreError::Other(format!("anthropic {status}: {text}")));
        }

        let events = crate::sse::sse_data_stream(resp.bytes_stream());
        let init = AnthropicStreamState::Active {
            stream: Box::pin(events),
            acc: AnthropicStreamAcc::default(),
        };
        let out = futures::stream::unfold(init, |state| async move {
            let AnthropicStreamState::Active {
                mut stream,
                mut acc,
            } = state
            else {
                return None;
            };
            loop {
                match stream.next().await {
                    Some(Ok(data)) => {
                        let ev: AnthropicStreamEvent = match serde_json::from_str(&data) {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        match ev.kind.as_str() {
                            "error" => {
                                let message = ev
                                    .error
                                    .map_or_else(|| "anthropic stream error".into(), |e| e.message);
                                return Some((
                                    Ok(ReasonChunk::Error { message }),
                                    AnthropicStreamState::Done,
                                ));
                            }
                            "message_stop" => {
                                let resp = acc.finish();
                                return Some((
                                    Ok(ReasonChunk::Final { response: resp }),
                                    AnthropicStreamState::Done,
                                ));
                            }
                            _ => {
                                if let Some(delta) = acc.ingest(ev)
                                    && !delta.is_empty()
                                {
                                    return Some((
                                        Ok(ReasonChunk::Text { delta }),
                                        AnthropicStreamState::Active { stream, acc },
                                    ));
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        return Some((
                            Ok(ReasonChunk::Error {
                                message: e.to_string(),
                            }),
                            AnthropicStreamState::Done,
                        ));
                    }
                    None => {
                        let resp = acc.finish();
                        return Some((
                            Ok(ReasonChunk::Final { response: resp }),
                            AnthropicStreamState::Done,
                        ));
                    }
                }
            }
        });
        Ok(Box::pin(out))
    }

    fn estimate_cost(&self, _req: &ReasonRequest) -> CostEstimate {
        CostEstimate {
            grain: 0,
            usd_micros: 0,
            confidence: "none".into(),
        }
    }

    async fn health_check(&self) -> Result<()> {
        // Light probe: send a 1-token request.
        let probe = ReasonRequest {
            system: String::new(),
            messages: vec![Message {
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

fn build_request<'a>(model: &'a str, req: &'a ReasonRequest) -> AnthropicRequest<'a> {
    build_request_inner(model, req, false)
}

fn build_request_inner<'a>(
    model: &'a str,
    req: &'a ReasonRequest,
    stream: bool,
) -> AnthropicRequest<'a> {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let content = match &m.content {
                MessageContent::Text(s) => json!(s),
                MessageContent::Blocks(blocks) => {
                    Value::Array(blocks.iter().map(block_to_anthropic).collect())
                }
            };
            json!({ "role": m.role, "content": content })
        })
        .collect();
    AnthropicRequest {
        model,
        max_tokens: req.max_tokens,
        system: if req.system.is_empty() {
            None
        } else {
            Some(req.system.as_str())
        },
        messages,
        tools: req.tools.clone(),
        temperature: Some(req.temperature),
        stop_seq: req.stop_seq.clone(),
        stream,
    }
}

/// Render one Coop [`ContentBlock`] into an Anthropic Messages API content
/// block. `tool_use` carries the `id` so a following `tool_result` correlates;
/// `Thinking` degrades to plain text (Anthropic does not accept replayed
/// `thinking` blocks on input).
fn block_to_anthropic(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } | ContentBlock::Thinking { text } => {
            json!({ "type": "text", "text": text })
        }
        ContentBlock::ToolCall { id, name, input } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::ReasonRequest;

    fn req_with(messages: Vec<Message>) -> ReasonRequest {
        ReasonRequest {
            system: "sys".into(),
            messages,
            tools: vec![],
            temperature: 0.5,
            max_tokens: 256,
            stop_seq: vec![],
            stream: false,
            metadata: Default::default(),
        }
    }

    #[test]
    fn text_message_renders_as_string_content() {
        let req = req_with(vec![Message {
            role: "user".into(),
            content: "hi".into(),
        }]);
        let body = build_request("m", &req);
        assert_eq!(body.messages[0]["role"], "user");
        assert_eq!(body.messages[0]["content"], json!("hi"));
    }

    #[test]
    fn structured_tool_use_and_result_round_trip_to_wire() {
        let assistant = Message {
            role: "assistant".into(),
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "let me check".into(),
                },
                ContentBlock::ToolCall {
                    id: "toolu_1".into(),
                    name: "bash".into(),
                    input: json!({ "command": "ls" }),
                },
            ]),
        };
        let user = Message {
            role: "user".into(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "a.txt".into(),
                is_error: false,
            }]),
        };
        let req = req_with(vec![assistant, user]);
        let body = build_request("m", &req);

        // Assistant turn: text block + tool_use block carrying the id.
        let a = &body.messages[0]["content"];
        assert_eq!(a[0]["type"], "text");
        assert_eq!(a[1]["type"], "tool_use");
        assert_eq!(a[1]["id"], "toolu_1");
        assert_eq!(a[1]["name"], "bash");
        assert_eq!(a[1]["input"]["command"], "ls");

        // User turn: tool_result correlated by tool_use_id.
        let u = &body.messages[1]["content"];
        assert_eq!(u[0]["type"], "tool_result");
        assert_eq!(u[0]["tool_use_id"], "toolu_1");
        assert_eq!(u[0]["content"], "a.txt");
        assert_eq!(u[0]["is_error"], false);
    }

    #[test]
    fn message_content_is_untagged_in_json() {
        // Text serializes as a bare string; Blocks as an array — preserving
        // backward-compatible wire shape.
        let text: MessageContent = "hello".into();
        assert_eq!(serde_json::to_value(&text).unwrap(), json!("hello"));
        let blocks = MessageContent::Blocks(vec![ContentBlock::Text { text: "x".into() }]);
        let v = serde_json::to_value(&blocks).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["type"], "text");

        // And both deserialize back through the untagged enum.
        let back: MessageContent = serde_json::from_value(json!("hello")).unwrap();
        assert!(matches!(back, MessageContent::Text(s) if s == "hello"));
        let back: MessageContent = serde_json::from_value(v).unwrap();
        assert!(matches!(back, MessageContent::Blocks(_)));
    }

    fn ev(json: &str) -> AnthropicStreamEvent {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn stream_acc_accumulates_text_blocks() {
        let mut acc = AnthropicStreamAcc::default();
        acc.ingest(ev(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":11,"output_tokens":1}}}"#,
        ));
        acc.ingest(ev(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        ));
        let d = acc.ingest(ev(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        ));
        assert_eq!(d.as_deref(), Some("Hi"));
        acc.ingest(ev(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
        ));
        let resp = acc.finish();
        assert_eq!(resp.finish_reason, "end_turn");
        assert_eq!(resp.usage.input_tokens, 11);
        assert_eq!(resp.usage.output_tokens, 5);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hi"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn stream_acc_assembles_tool_use_from_partial_json() {
        let mut acc = AnthropicStreamAcc::default();
        acc.ingest(ev(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_9","name":"bash"}}"#,
        ));
        acc.ingest(ev(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":"}}"#,
        ));
        acc.ingest(ev(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#,
        ));
        acc.ingest(ev(
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
        ));
        let resp = acc.finish();
        assert_eq!(resp.finish_reason, "tool_use");
        match &resp.content[0] {
            ContentBlock::ToolCall { id, name, input } => {
                assert_eq!(id, "toolu_9");
                assert_eq!(name, "bash");
                assert_eq!(input["cmd"], "ls");
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }
}
