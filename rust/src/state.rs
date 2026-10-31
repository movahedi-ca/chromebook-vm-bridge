//! Central shared state for vm-bridge.
//!
//! Every mutable structure lives behind a `tokio::sync` lock. The borrow
//! checker plus these locks make the Python audit's "non-atomic
//! read-modify-write across await boundaries" (BUG-2) a compile error.
//!
//! Lock ordering (to avoid deadlock): browser1 -> tabs -> leases -> metrics.
//! The X11 connection uses `std::sync::Mutex` (never held across `.await`).

use crate::browser::Browser1;
use crate::config::Config;
use crate::leases::LeaseTable;
use crate::pool::PoolState;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

/// Per-tab state: page handle, verification baseline, last URL.
pub struct TabEntry {
    pub id: String,
    /// chromiumoxide page handle (Arc for sharing across tasks)
    pub page: Arc<chromiumoxide::Page>,
    /// Baseline screenshot hash for the verify gate (None = not yet captured)
    pub baseline_hash: Option<u64>,
    pub url: String,
    pub title: String,
}

/// LRU tab registry. `IndexMap` preserves insertion order; `move_to_end`
/// on access gives true LRU (fixes Python's FIFO `next(iter(_tabs))` bug).
pub struct TabRegistry {
    tabs: IndexMap<String, TabEntry>,
    active_id: Option<String>,
    max_tabs: usize,
}

impl TabRegistry {
    pub fn new(max_tabs: usize) -> Self {
        Self {
            tabs: IndexMap::new(),
            active_id: None,
            max_tabs,
        }
    }

    pub fn insert(&mut self, entry: TabEntry) {
        // Evict least-recently-used if at capacity
        while self.tabs.len() >= self.max_tabs {
            if let Some((evicted_id, _)) = self.tabs.shift_remove_index(0) {
                tracing::info!(tab_id = %evicted_id, "evicted LRU tab");
                if self.active_id.as_deref() == Some(&evicted_id) {
                    self.active_id = None;
                }
            } else {
                break;
            }
        }
        self.active_id = Some(entry.id.clone());
        self.tabs.insert(entry.id.clone(), entry);
    }

    pub fn get(&mut self, id: &str) -> Option<&TabEntry> {
        // LRU touch: move to end on access
        if let Some(idx) = self.tabs.get_index_of(id) {
            self.tabs.move_index(idx, self.tabs.len() - 1);
        }
        self.tabs.get(id)
    }

    pub fn switch(&mut self, id: &str) -> Option<&TabEntry> {
        if self.tabs.get_index_of(id).is_none() {
            return None;
        }
        self.active_id = Some(id.to_string());
        // LRU touch
        if let Some(idx) = self.tabs.get_index_of(id) {
            let len = self.tabs.len();
            self.tabs.move_index(idx, len - 1);
        }
        self.tabs.get(id)
    }

    pub fn active(&mut self) -> Option<&TabEntry> {
        let id = self.active_id.clone()?;
        self.get(&id)
    }

    pub fn remove(&mut self, id: &str) -> Option<TabEntry> {
        let entry = self.tabs.shift_remove(id);
        if self.active_id.as_deref() == Some(id) {
            self.active_id = self.tabs.keys().last().cloned();
        }
        entry
    }

    pub fn list(&self) -> Vec<TabSummary> {
        self.tabs
            .iter()
            .map(|(id, e)| TabSummary {
                id: id.clone(),
                url: e.url.clone(),
                title: e.title.clone(),
                active: self.active_id.as_deref() == Some(id),
            })
            .collect()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TabSummary {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// Rate-limiter buckets keyed by (client_ip, route_template) — NOT raw path,
/// so 404 dictionary scans can't grow the map unboundedly (audit §5.3).
pub type RateBuckets = HashMap<
    (String, String),
    governor::RateLimiter<
        governor::state::NotKeyed,
        governor::state::InMemoryState,
        governor::clock::DefaultClock,
    >,
>;

/// Operational metrics (replaces the Python `_metrics` dict).
#[derive(Debug, Default)]
pub struct Metrics {
    pub requests_total: u64,
    pub requests_4xx: u64,
    pub requests_5xx: u64,
    pub relaunches: u64, // incremented ONLY on real recovery, not boot
    pub rejections_401: u64,
    pub rejections_429: u64,
    pub rejections_423: u64,
}

/// The single shared state object, passed to every handler via axum `State`.
pub struct BridgeState {
    pub config: Config,
    pub browser1: Mutex<Browser1>,
    pub pool: Mutex<PoolState>,
    pub tabs: RwLock<TabRegistry>,
    pub leases: Mutex<LeaseTable>,
    pub metrics: Mutex<Metrics>,
    pub rate_buckets: Mutex<RateBuckets>,
    /// X11 connection: std Mutex, NEVER held across .await (it's blocking X11 I/O)
    pub x11: StdMutex<Option<x11rb::rust_connection::RustConnection>>,
    /// Shutdown flag for graceful teardown (audit BUG-4)
    pub shutting_down: std::sync::atomic::AtomicBool,
    /// Lease ID for internal bypass (empty = no bypass)
    pub internal_token: String,
}

impl BridgeState {
    pub fn new(config: Config) -> Self {
        Self {
            browser1: Mutex::new(Browser1::new()),
            pool: Mutex::new(PoolState::new(3)),
            tabs: RwLock::new(TabRegistry::new(10)),
            leases: Mutex::new(LeaseTable::new()),
            metrics: Mutex::new(Metrics::default()),
            rate_buckets: Mutex::new(HashMap::new()),
            x11: StdMutex::new(None),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            internal_token: Uuid::new_v4().to_string(),
            config,
        }
    }
}

pub type SharedState = Arc<BridgeState>;
