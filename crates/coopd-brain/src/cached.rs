//! Content-addressed LRU cache for brain reasoning calls.
//!
//! Wraps any [`BrainAdapter`] and returns cached responses when the exact same
//! `(model, system, messages, temperature, tools)` tuple is requested again.
//! Mutating fields like `stream` and `metadata` are ignored for the hash.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use coopd_core::{
    BrainAdapter, BrainCaps, ReasonRequest, ReasonResponse, Result, Tier,
    brain::{CostEstimate, ReasonChunk},
};
use futures::stream::BoxStream;
use serde::Serialize;

#[derive(Serialize)]
struct CacheKeyFields<'a> {
    model_hint: &'a str,
    system: &'a str,
    messages: &'a [coopd_core::brain::Message],
    tools: &'a [serde_json::Value],
    temperature: f32,
    max_tokens: u32,
    stop_seq: &'a [String],
}

fn key_for(model_hint: &str, req: &ReasonRequest) -> u64 {
    let fields = CacheKeyFields {
        model_hint,
        system: &req.system,
        messages: &req.messages,
        tools: &req.tools,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        stop_seq: &req.stop_seq,
    };
    // Hash via stable JSON to avoid struct-layout dependence.
    let bytes = serde_json::to_vec(&fields).unwrap_or_default();
    // FNV-1a (fast, deterministic, no extra dep).
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in &bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Internal LRU map keyed by request hash. Tiny on purpose: 256 entries.
#[derive(Debug)]
struct Lru {
    cap: usize,
    map: HashMap<u64, ReasonResponse>,
    order: VecDeque<u64>,
    hits: u64,
    misses: u64,
}
impl Lru {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            map: HashMap::new(),
            order: VecDeque::new(),
            hits: 0,
            misses: 0,
        }
    }
    fn get(&mut self, k: u64) -> Option<ReasonResponse> {
        if let Some(r) = self.map.get(&k).cloned() {
            self.hits += 1;
            // Move to back (most-recent).
            if let Some(pos) = self.order.iter().position(|x| *x == k) {
                self.order.remove(pos);
            }
            self.order.push_back(k);
            Some(r)
        } else {
            self.misses += 1;
            None
        }
    }
    fn put(&mut self, k: u64, v: ReasonResponse) {
        if self.map.insert(k, v).is_none() {
            self.order.push_back(k);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
}

/// Statistics snapshot for observability.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct CacheStats {
    /// Number of cache hits since process start.
    pub hits: u64,
    /// Number of cache misses since process start.
    pub misses: u64,
    /// Current cache entry count.
    pub size: usize,
}

/// Caching decorator around any [`BrainAdapter`].
///
/// Streaming and health checks pass through unmodified — only `reason()` is
/// cached. Cache is keyed by `(model_hint, request_payload)` so the same
/// prompt sent to a different model is treated as a different key.
#[derive(Clone)]
pub struct CachingBrain<B: BrainAdapter + 'static> {
    inner: Arc<B>,
    lru: Arc<Mutex<Lru>>,
    model_hint: String,
}

impl<B: BrainAdapter + 'static> CachingBrain<B> {
    /// Construct a caching wrapper with default capacity (256 entries).
    pub fn new(inner: B, model_hint: impl Into<String>) -> Self {
        Self::with_capacity(inner, model_hint, 256)
    }
    /// Construct with explicit cache capacity.
    pub fn with_capacity(inner: B, model_hint: impl Into<String>, cap: usize) -> Self {
        Self {
            inner: Arc::new(inner),
            lru: Arc::new(Mutex::new(Lru::new(cap))),
            model_hint: model_hint.into(),
        }
    }
    /// Snapshot the current cache stats.
    ///
    /// # Panics
    ///
    /// Panics if the internal LRU mutex has been poisoned by a previous
    /// panic in another thread holding the lock. This indicates an
    /// unrecoverable bug elsewhere in the cache.
    pub fn stats(&self) -> CacheStats {
        let lru = self.lru.lock().expect("cache lru mutex poisoned");
        CacheStats {
            hits: lru.hits,
            misses: lru.misses,
            size: lru.map.len(),
        }
    }
}

impl<B: BrainAdapter + 'static> std::fmt::Debug for CachingBrain<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachingBrain")
            .field("model_hint", &self.model_hint)
            .field("stats", &self.stats())
            .finish()
    }
}

#[async_trait]
impl<B: BrainAdapter + 'static> BrainAdapter for CachingBrain<B> {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn tier(&self) -> Tier {
        self.inner.tier()
    }
    fn capabilities(&self) -> BrainCaps {
        self.inner.capabilities()
    }

    async fn reason(&self, req: ReasonRequest) -> Result<ReasonResponse> {
        let k = key_for(&self.model_hint, &req);
        if let Some(mut hit) = self.lru.lock().expect("cache lru mutex poisoned").get(k) {
            // Mark as cached: zero out latency + usage so the caller knows.
            hit.latency_ms = 0;
            hit.usage.cache_tokens = hit.usage.input_tokens;
            hit.usage.input_tokens = 0;
            return Ok(hit);
        }
        let resp = self.inner.reason(req).await?;
        self.lru
            .lock()
            .expect("cache lru mutex poisoned")
            .put(k, resp.clone());
        Ok(resp)
    }

    async fn stream(&self, req: ReasonRequest) -> Result<BoxStream<'static, Result<ReasonChunk>>> {
        self.inner.stream(req).await
    }
    fn estimate_cost(&self, req: &ReasonRequest) -> CostEstimate {
        self.inner.estimate_cost(req)
    }
    async fn health_check(&self) -> Result<()> {
        self.inner.health_check().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::brain::{ContentBlock, Cost, Message, Usage};
    use std::sync::atomic::{AtomicU32, Ordering};

    use std::sync::Arc;

    struct CountingBrain {
        calls: Arc<AtomicU32>,
    }
    #[async_trait]
    impl BrainAdapter for CountingBrain {
        fn name(&self) -> &str {
            "count"
        }
        fn tier(&self) -> Tier {
            Tier::Byok
        }
        fn capabilities(&self) -> BrainCaps {
            BrainCaps {
                context_window: 0,
                tool_use: false,
                vision: false,
                streaming_tools: false,
                max_output_tokens: 0,
                pricing_tier: String::new(),
            }
        }
        async fn reason(&self, _req: ReasonRequest) -> Result<ReasonResponse> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(ReasonResponse {
                content: vec![ContentBlock::Text { text: "hi".into() }],
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_tokens: 0,
                },
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
                latency_ms: 12,
            })
        }
        async fn stream(
            &self,
            _req: ReasonRequest,
        ) -> Result<BoxStream<'static, Result<ReasonChunk>>> {
            unimplemented!()
        }
        fn estimate_cost(&self, _req: &ReasonRequest) -> CostEstimate {
            CostEstimate::default()
        }
        async fn health_check(&self) -> Result<()> {
            Ok(())
        }
    }

    fn make_req(prompt: &str) -> ReasonRequest {
        ReasonRequest {
            system: String::new(),
            messages: vec![Message {
                role: "user".into(),
                content: prompt.into(),
            }],
            tools: vec![],
            temperature: 0.0,
            max_tokens: 64,
            stop_seq: vec![],
            stream: false,
            metadata: Default::default(),
        }
    }

    #[tokio::test]
    async fn cache_hits_avoid_inner_call() {
        let calls = Arc::new(AtomicU32::new(0));
        let inner = CountingBrain {
            calls: calls.clone(),
        };
        let cache = CachingBrain::new(inner, "test-model");

        let _ = cache.reason(make_req("hello")).await.unwrap();
        let _ = cache.reason(make_req("hello")).await.unwrap();
        let _ = cache.reason(make_req("hello")).await.unwrap();
        let _ = cache.reason(make_req("world")).await.unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 2, "inner called twice only");
        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 2);
    }
}
