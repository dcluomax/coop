//! Auto model router: picks the cheapest model that can plausibly handle a
//! given [`ReasonRequest`] without breaking quality.
//!
//! Pure heuristics — no LLM round-trip. Designed for the common case where a
//! farmer says "use Claude" and we route between haiku / sonnet / opus
//! transparently based on prompt complexity.

use coopd_core::ReasonRequest;

/// Default Anthropic model identifiers used by the router.
pub mod models {
    /// Cheap, fast — good for trivial single-turn prompts.
    pub const HAIKU: &str = "claude-haiku-4-5";
    /// Workhorse — coding, multi-turn, tool use.
    pub const SONNET: &str = "claude-sonnet-4-5-20250929";
    /// Heaviest — long context, deep reasoning, architecture.
    pub const OPUS: &str = "claude-opus-4-1";
}

/// Difficulty buckets the router maps prompts into.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Difficulty {
    /// Short, single-shot, no tools.
    Trivial,
    /// Medium length, tool use, or code edit.
    Standard,
    /// Long context, planning, debugging, architecture.
    Hard,
}

/// Score a request's difficulty using length + keyword + tool heuristics.
#[must_use]
pub fn classify(req: &ReasonRequest) -> Difficulty {
    let total_chars: usize = req.system.len()
        + req
            .messages
            .iter()
            .map(|m| m.content.text_len())
            .sum::<usize>();
    let has_tools = !req.tools.is_empty();
    let needs_big_output = req.max_tokens >= 8_000;

    let last_user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_text().to_lowercase())
        .unwrap_or_default();

    let hard_words = [
        "architect",
        "design a system",
        "debug",
        "refactor",
        "optimize",
        "prove",
        "trace through",
        "plan a",
        "step by step",
        "long-form",
        "novel",
    ];
    let easy_words = [
        "summarize one",
        "say hi",
        "what is the",
        "yes or no",
        "rate",
        "classify",
        "translate",
    ];

    if hard_words.iter().any(|w| last_user.contains(w)) || total_chars > 20_000 || needs_big_output
    {
        return Difficulty::Hard;
    }
    if !has_tools && total_chars < 600 && easy_words.iter().any(|w| last_user.contains(w)) {
        return Difficulty::Trivial;
    }
    if !has_tools && total_chars < 300 {
        return Difficulty::Trivial;
    }
    Difficulty::Standard
}

/// Pick a model id for the given request.
#[must_use]
pub fn pick_model(req: &ReasonRequest) -> &'static str {
    match classify(req) {
        Difficulty::Trivial => models::HAIKU,
        Difficulty::Standard => models::SONNET,
        Difficulty::Hard => models::OPUS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::brain::Message;

    fn req(prompt: &str, max: u32, tools: bool) -> ReasonRequest {
        ReasonRequest {
            system: String::new(),
            messages: vec![Message {
                role: "user".into(),
                content: prompt.into(),
            }],
            tools: if tools {
                vec![serde_json::json!({})]
            } else {
                vec![]
            },
            temperature: 0.7,
            max_tokens: max,
            stop_seq: vec![],
            stream: false,
            metadata: Default::default(),
        }
    }

    #[test]
    fn trivial_routes_to_haiku() {
        assert_eq!(pick_model(&req("hi", 256, false)), models::HAIKU);
        assert_eq!(
            pick_model(&req("translate hello to french", 256, false)),
            models::HAIKU
        );
    }

    #[test]
    fn hard_routes_to_opus() {
        assert_eq!(
            pick_model(&req("please refactor the orchestrator", 1024, true)),
            models::OPUS
        );
        // Big output ⇒ opus.
        assert_eq!(
            pick_model(&req("write me an essay", 16_000, false)),
            models::OPUS
        );
    }

    #[test]
    fn default_is_sonnet() {
        let prompt = "write a function that fetches a URL and returns json";
        assert_eq!(pick_model(&req(prompt, 2_048, true)), models::SONNET);
    }
}
