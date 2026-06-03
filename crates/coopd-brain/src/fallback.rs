//! Fallback brain: wraps a primary adapter and an ordered list of fallbacks.
//!
//! On a failed [`BrainAdapter::reason`] / [`BrainAdapter::stream`] call the
//! decorator transparently retries the next adapter in order, returning the
//! first success. This enables resilient manifests such as *primary =
//! Anthropic, fallback = a local OpenAI-compatible model* so a Hen keeps
//! working through an upstream outage or rate-limit. The last error is
//! propagated if every adapter fails.
//!
//! Wiring: see `coopd::brain_factory`, which builds the primary plus one
//! adapter per `manifest.brain.fallbacks` entry.

use std::sync::Arc;

use async_trait::async_trait;
use coopd_core::{
    BrainAdapter, BrainCaps, CoreError, ReasonRequest, ReasonResponse, Result, Tier,
    brain::{CostEstimate, ReasonChunk},
};
use futures::stream::BoxStream;
use tracing::warn;

/// Brain decorator that fails over across an ordered chain of adapters.
#[derive(Clone)]
pub struct FallbackBrain {
    primary: Arc<dyn BrainAdapter>,
    fallbacks: Vec<Arc<dyn BrainAdapter>>,
}

impl std::fmt::Debug for FallbackBrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackBrain")
            .field("primary", &self.primary.name())
            .field("fallbacks", &self.fallbacks.len())
            .finish()
    }
}

impl FallbackBrain {
    /// Construct from a primary adapter and a non-empty fallback chain.
    ///
    /// If `fallbacks` is empty this still works (it simply behaves like the
    /// primary), but callers should prefer to skip the wrapper entirely in that
    /// case.
    #[must_use]
    pub fn new(primary: Arc<dyn BrainAdapter>, fallbacks: Vec<Arc<dyn BrainAdapter>>) -> Self {
        Self { primary, fallbacks }
    }

    /// The adapter chain in attempt order (primary first).
    fn chain(&self) -> impl Iterator<Item = &Arc<dyn BrainAdapter>> {
        std::iter::once(&self.primary).chain(self.fallbacks.iter())
    }
}

#[async_trait]
impl BrainAdapter for FallbackBrain {
    fn name(&self) -> &str {
        self.primary.name()
    }
    fn tier(&self) -> Tier {
        self.primary.tier()
    }
    fn capabilities(&self) -> BrainCaps {
        self.primary.capabilities()
    }

    async fn reason(&self, req: ReasonRequest) -> Result<ReasonResponse> {
        let mut last_err: Option<CoreError> = None;
        for (idx, adapter) in self.chain().enumerate() {
            match adapter.reason(req.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    warn!(
                        attempt = idx,
                        adapter = adapter.name(),
                        error = %e,
                        "brain attempt failed; trying next fallback"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| CoreError::Other("fallback chain exhausted (empty)".into())))
    }

    async fn stream(&self, req: ReasonRequest) -> Result<BoxStream<'static, Result<ReasonChunk>>> {
        let mut last_err: Option<CoreError> = None;
        for (idx, adapter) in self.chain().enumerate() {
            match adapter.stream(req.clone()).await {
                Ok(s) => return Ok(s),
                Err(e) => {
                    warn!(
                        attempt = idx,
                        adapter = adapter.name(),
                        error = %e,
                        "brain stream attempt failed; trying next fallback"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| CoreError::Other("fallback chain exhausted (empty)".into())))
    }

    fn estimate_cost(&self, req: &ReasonRequest) -> CostEstimate {
        self.primary.estimate_cost(req)
    }

    async fn health_check(&self) -> Result<()> {
        // Healthy if *any* link in the chain is reachable.
        let mut last_err: Option<CoreError> = None;
        for adapter in self.chain() {
            match adapter.health_check().await {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| CoreError::Other("fallback chain exhausted (empty)".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::brain::{ContentBlock, Cost, Usage};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test adapter that succeeds or fails deterministically and counts calls.
    struct Stub {
        name: &'static str,
        fail: bool,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BrainAdapter for Stub {
        fn name(&self) -> &str {
            self.name
        }
        fn tier(&self) -> Tier {
            Tier::Byok
        }
        fn capabilities(&self) -> BrainCaps {
            BrainCaps::default()
        }
        async fn reason(&self, _req: ReasonRequest) -> Result<ReasonResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(CoreError::Other(format!("{} down", self.name)));
            }
            Ok(ReasonResponse {
                content: vec![ContentBlock::Text {
                    text: self.name.to_string(),
                }],
                usage: Usage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
                latency_ms: 0,
            })
        }
        async fn stream(
            &self,
            _req: ReasonRequest,
        ) -> Result<BoxStream<'static, Result<ReasonChunk>>> {
            if self.fail {
                return Err(CoreError::Other(format!("{} down", self.name)));
            }
            Ok(Box::pin(futures::stream::empty()))
        }
        fn estimate_cost(&self, _req: &ReasonRequest) -> CostEstimate {
            CostEstimate::default()
        }
        async fn health_check(&self) -> Result<()> {
            if self.fail {
                Err(CoreError::Other("down".into()))
            } else {
                Ok(())
            }
        }
    }

    fn req() -> ReasonRequest {
        ReasonRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            temperature: 0.0,
            max_tokens: 16,
            stop_seq: vec![],
            stream: false,
            metadata: Default::default(),
        }
    }

    fn first_text(resp: &ReasonResponse) -> &str {
        match &resp.content[0] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected text block"),
        }
    }

    fn stub(name: &'static str, fail: bool) -> (Arc<dyn BrainAdapter>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let s: Arc<dyn BrainAdapter> = Arc::new(Stub {
            name,
            fail,
            calls: calls.clone(),
        });
        (s, calls)
    }

    #[tokio::test]
    async fn primary_success_skips_fallbacks() {
        let (primary, pc) = stub("primary", false);
        let (fb, fc) = stub("fb", false);
        let brain = FallbackBrain::new(primary, vec![fb]);
        let resp = brain.reason(req()).await.unwrap();
        assert_eq!(first_text(&resp), "primary");
        assert_eq!(pc.load(Ordering::SeqCst), 1);
        assert_eq!(fc.load(Ordering::SeqCst), 0, "fallback must not be called");
    }

    #[tokio::test]
    async fn falls_through_to_first_healthy() {
        let (primary, pc) = stub("primary", true);
        let (fb1, c1) = stub("fb1", true);
        let (fb2, c2) = stub("fb2", false);
        let brain = FallbackBrain::new(primary, vec![fb1, fb2]);
        let resp = brain.reason(req()).await.unwrap();
        assert_eq!(first_text(&resp), "fb2");
        assert_eq!(pc.load(Ordering::SeqCst), 1);
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert_eq!(c2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn all_fail_returns_last_error() {
        let (primary, _) = stub("primary", true);
        let (fb, _) = stub("fb", true);
        let brain = FallbackBrain::new(primary, vec![fb]);
        let err = brain.reason(req()).await.unwrap_err();
        assert!(err.to_string().contains("fb down"));
    }
}
