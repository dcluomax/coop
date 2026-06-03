//! # coopd-brain
//!
//! Brain adapters (LLM provider integrations) for the Coop runtime.
//!
//! v0.1 ships a single Anthropic Messages API adapter (BYOK tier).
//! v0.2 adds an OpenAI Chat Completions adapter that also drives any
//! OpenAI-compatible endpoint (llama.cpp / Ollama / vLLM) via a base-URL
//! override (the `openai-compat` provider).

#![warn(missing_docs)]

pub mod anthropic;
pub mod cached;
pub mod openai;
pub mod router;
pub mod routing;

pub use anthropic::Anthropic;
pub use cached::{CacheStats, CachingBrain};
pub use openai::OpenAi;
pub use routing::RoutingBrain;
