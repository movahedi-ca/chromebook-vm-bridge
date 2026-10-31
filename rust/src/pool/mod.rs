//! Context pool for background work.
//!
//! Max 3 contexts (LRU, idle-expired by TTL). Guarded by the single
//! `BridgeState::pool` mutex: acquire/release/lookup all serialize on it.
//!
//! The pool uses the main (headed) browser's pages for isolated contexts.
//! A separate headless browser was attempted but chromiumoxide 0.7 has a
//! WS deserialization bug with Chrome 156 that kills the handler on
//! browser launch. Using the main browser provides identical API parity
//! without a competing browser process.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chromiumoxide::{Browser, Page};

/// A single pooled context (isolated page on the main browser).
pub struct PoolContext {
    #[allow(dead_code)] // surfaced by /pool/status in a future revision
    pub id: String,
    pub page: Option<Arc<Page>>,
    pub last_used: Instant,
    pub ttl: Duration,
    #[allow(dead_code)] // surfaced by /pool/status in a future revision
    pub trace: bool,
}

pub struct PoolState {
    max_contexts: usize,
    contexts: HashMap<String, PoolContext>,
}

impl PoolState {
    pub fn new(max_contexts: usize) -> Self {
        Self {
            max_contexts,
            contexts: HashMap::new(),
        }
    }

    pub fn context_count(&self) -> usize {
        self.contexts.len()
    }

    pub fn max_contexts(&self) -> usize {
        self.max_contexts
    }

    pub fn is_browser_connected(&self) -> bool {
        // Pool uses the main browser; always "connected" if contexts exist.
        // The actual browser liveness is managed by Browser1.
        true
    }

    /// Close a page target when we are its last holder. In-flight handlers
    /// hold their own clone; in that case the target stays open until the
    /// browser dies, which is bounded by the pool cap.
    async fn evict(&mut self, id: &str) {
        if self.contexts.remove(id).is_some() {
            tracing::debug!(context_id = %id, "pool context evicted");
        }
    }

    /// Prune TTL-expired contexts.
    pub async fn prune(&mut self) {
        let now = Instant::now();
        let expired: Vec<String> = self
            .contexts
            .iter()
            .filter(|(_, c)| now.duration_since(c.last_used) > c.ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.evict(&id).await;
        }
    }

    /// Acquire a context: prune expired, LRU-evict past the cap, then open
    /// a fresh blank page on the main browser. Returns the context id.
    pub async fn acquire(
        &mut self,
        browser: &Arc<Browser>,
        ttl_s: u64,
        trace: bool,
    ) -> anyhow::Result<String> {
        self.prune().await;
        while self.contexts.len() >= self.max_contexts {
            let oldest = self
                .contexts
                .iter()
                .min_by_key(|(_, c)| c.last_used)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => self.evict(&id).await,
                None => break,
            }
        }
        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| anyhow::anyhow!("pool new_page: {}", e))?;
        let id: String = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let ttl = Duration::from_secs(ttl_s.clamp(60, 3600));
        self.contexts.insert(
            id.clone(),
            PoolContext {
                id: id.clone(),
                page: Some(Arc::new(page)),
                last_used: Instant::now(),
                ttl,
                trace,
            },
        );
        tracing::info!(context_id = %id, trace, "pool context acquired");
        Ok(id)
    }

    /// Look up a context's page, touching it for LRU/TTL purposes.
    pub fn get(&mut self, id: &str) -> Option<Arc<Page>> {
        if let Some(ctx) = self.contexts.get_mut(id) {
            ctx.last_used = Instant::now();
            ctx.page.clone()
        } else {
            None
        }
    }

    /// Touch a context (update last_used) without returning the page.
    #[allow(dead_code)] // part of the pool API; no caller yet (audit LOW-1)
    pub fn touch(&mut self, id: &str) -> bool {
        match self.contexts.get_mut(id) {
            Some(ctx) => {
                ctx.last_used = Instant::now();
                true
            }
            None => false,
        }
    }

    /// Release a context, closing its page. Returns false if unknown.
    pub async fn release(&mut self, id: &str) -> bool {
        if self.contexts.contains_key(id) {
            self.evict(id).await;
            true
        } else {
            false
        }
    }
}
