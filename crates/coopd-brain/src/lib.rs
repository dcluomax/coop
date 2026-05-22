//! # coopd-brain
//!
//! Brain adapters (LLM provider integrations) for the Coop runtime.
//!
//! v0.1 ships a single Anthropic Messages API adapter (BYOK tier).

#![warn(missing_docs)]

pub mod anthropic;
pub mod cached;
pub mod router;
pub mod routing;

pub use anthropic::Anthropic;
pub use cached::{CacheStats, CachingBrain};
pub use routing::RoutingBrain;
