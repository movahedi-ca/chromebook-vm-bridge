//! Browser #2: headless context pool for background work.
//!
//! Max 3 contexts (LRU). Guarded by `BridgeState::pool` mutex —
//! fixes the Python duplicate-launch race (audit BUG-2).

use std::collections::HashMap;

/// A single pooled headless context.
pub struct PoolContext {
    pub id: String,
    pub page: Option<std::sync::Arc<chromiumoxide::Page>>,
    pub last_used: chrono::DateTime<chrono::Utc>,
    pub trace: Vec<String>,
}

pub struct PoolState {
    max_contexts: usize,
    contexts: HashMap<String, PoolContext>,
    browser: Option<std::sync::Arc<chromiumoxide::Browser>>,
}

impl PoolState {
    pub fn new(max_contexts: usize) -> Self {
        Self {
            max_contexts,
            contexts: HashMap::new(),
            browser: None,
        }
    }

    pub fn context_count(&self) -> usize {
        self.contexts.len()
    }

    pub fn max_contexts(&self) -> usize {
        self.max_contexts
    }

    pub fn is_browser_connected(&self) -> bool {
        self.browser.is_some()
    }
}
