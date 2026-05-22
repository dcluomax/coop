//! Routing brain: holds one inner adapter per tier (haiku/sonnet/opus) and
//! dispatches each request to the cheapest model that can plausibly handle it.
//!
//! See [`crate::router`] for the classification heuristics.

use std::sync::Arc;

use async_trait::async_trait;
use coopd_core::{
    BrainAdapter, BrainCaps, ReasonRequest, ReasonResponse, Result, Tier,
    brain::{CostEstimate, ReasonChunk},
};
use futures::stream::BoxStream;

use crate::router::{Difficulty, classify};

/// Brain decorator that routes requests across three inner adapters.
#[derive(Clone)]
pub struct RoutingBrain {
    haiku: Arc<dyn BrainAdapter>,
    sonnet: Arc<dyn BrainAdapter>,
    opus: Arc<dyn BrainAdapter>,
}

impl std::fmt::Debug for RoutingBrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingBrain").finish()
    }
}

impl RoutingBrain {
    /// Construct from three adapters (one per difficulty bucket).
    #[must_use]
    pub fn new(
        haiku: Arc<dyn BrainAdapter>,
        sonnet: Arc<dyn BrainAdapter>,
        opus: Arc<dyn BrainAdapter>,
    ) -> Self {
        Self {
            haiku,
            sonnet,
            opus,
        }
    }

    fn pick(&self, req: &ReasonRequest) -> &Arc<dyn BrainAdapter> {
        match classify(req) {
            Difficulty::Trivial => &self.haiku,
            Difficulty::Standard => &self.sonnet,
            Difficulty::Hard => &self.opus,
        }
    }
}

#[async_trait]
impl BrainAdapter for RoutingBrain {
    fn name(&self) -> &str {
        "routing"
    }
    fn tier(&self) -> Tier {
        self.sonnet.tier()
    }
    fn capabilities(&self) -> BrainCaps {
        self.opus.capabilities()
    }
    async fn reason(&self, req: ReasonRequest) -> Result<ReasonResponse> {
        let pick = self.pick(&req).clone();
        pick.reason(req).await
    }
    async fn stream(&self, req: ReasonRequest) -> Result<BoxStream<'static, Result<ReasonChunk>>> {
        let pick = self.pick(&req).clone();
        pick.stream(req).await
    }
    fn estimate_cost(&self, req: &ReasonRequest) -> CostEstimate {
        self.pick(req).estimate_cost(req)
    }
    async fn health_check(&self) -> Result<()> {
        self.sonnet.health_check().await
    }
}
