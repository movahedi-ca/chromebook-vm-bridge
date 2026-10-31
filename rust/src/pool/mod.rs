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

use chromiumoxide::Page;

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

    pub fn is_browser_connected() -> bool {
        // Pool uses the main browser; always "connected" if contexts exist.
        // The actual browser liveness is managed by Browser1.
        true
    }

    /// Remove a context, returning its page. The caller MUST close the page
    /// AFTER dropping the pool lock (audit 40 MED-3/MED-4: the lock is never
    /// held across slow CDP calls). In-flight handlers hold their own clone;
    /// in that case the target stays open until the browser dies, which is
    /// bounded by the pool cap. Returns None for an unknown id (live
    /// contexts always have a page, so None is unambiguous).
    fn evict(&mut self, id: &str) -> Option<Arc<Page>> {
        let ctx = self.contexts.remove(id)?;
        tracing::debug!(context_id = %id, "pool context evicted");
        ctx.page
    }

    /// Prune TTL-expired contexts. No CDP calls; returns pages for the
    /// caller to close after dropping the lock.
    pub fn prune(&mut self) -> Vec<Arc<Page>> {
        let now = Instant::now();
        let expired: Vec<String> = self
            .contexts
            .iter()
            .filter(|(_, c)| now.duration_since(c.last_used) > c.ttl)
            .map(|(id, _)| id.clone())
            .collect();
        let mut pages = Vec::new();
        for id in expired {
            if let Some(p) = self.evict(&id) {
                pages.push(p);
            }
        }
        pages
    }

    /// Bookkeeping half of acquire: prune expired, LRU-evict past the cap.
    /// No CDP calls. Returns pages the caller must close after dropping the
    /// lock. Call BEFORE opening the new page (audit 40 MED-4).
    pub fn prepare_acquire(&mut self) -> Vec<Arc<Page>> {
        let mut to_close = self.prune();
        while self.contexts.len() >= self.max_contexts {
            let oldest = self
                .contexts
                .iter()
                .min_by_key(|(_, c)| c.last_used)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => {
                    if let Some(p) = self.evict(&id) {
                        to_close.push(p);
                    }
                }
                None => break,
            }
        }
        to_close
    }

    /// Register an already-opened page. No CDP calls. Returns the context id.
    ///
    /// Note: check-then-insert is no longer atomic with page creation (the
    /// old atomicity required holding the lock across `new_page`, which
    /// MED-4 forbids), so two concurrent acquires may transiently exceed the
    /// cap by one; the next `prepare_acquire` corrects it.
    pub fn register(&mut self, page: Arc<Page>, ttl_s: u64, trace: bool) -> String {
        let id: String = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let ttl = Duration::from_secs(ttl_s.clamp(60, 3600));
        self.contexts.insert(
            id.clone(),
            PoolContext {
                id: id.clone(),
                page: Some(page),
                last_used: Instant::now(),
                ttl,
                trace,
            },
        );
        tracing::info!(context_id = %id, trace, "pool context acquired");
        id
    }

    /// Release a context, returning its page for the caller to close after
    /// dropping the lock. Returns None for an unknown id. (Live contexts
    /// always have a page, so a None return is unambiguous here.)
    pub fn release(&mut self, id: &str) -> Option<Arc<Page>> {
        if !self.contexts.contains_key(id) {
            return None;
        }
        tracing::debug!(context_id = %id, "pool context released");
        self.evict(id)
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

    /// Touch a context (update `last_used`) without returning the page.
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
}
