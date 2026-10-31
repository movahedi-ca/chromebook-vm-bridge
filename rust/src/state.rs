//! Central shared state for vm-bridge.
//!
//! Every mutable structure lives behind a `tokio::sync` lock. The borrow
//! checker plus these locks make the the legacy audit's "non-atomic
//! read-modify-write across await boundaries" (BUG-2) a compile error.
//!
//! Lock ordering (to avoid deadlock): browser1 -> tabs -> leases -> metrics.
//! The X11 connection uses `std::sync::Mutex` (never held across `.await`).

use crate::browser::Browser1;
use crate::config::Config;
use crate::leases::LeaseTable;
use crate::perception::AxMapper;
use crate::pool::PoolState;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
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
/// on access gives true LRU (fixes the legacy FIFO `next(iter(_tabs))` bug).
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

    /// Insert a tab, evicting LRU entries past capacity. Returns the evicted
    /// pages: the caller must `close()` them AFTER dropping the tabs write
    /// lock (audit 40 MED-3 — eviction must not leak Chrome targets, and the
    /// lock is never held across the CDP close call).
    pub fn insert(&mut self, entry: TabEntry) -> Vec<Arc<chromiumoxide::Page>> {
        let mut evicted_pages = Vec::new();
        // Evict least-recently-used if at capacity
        while self.tabs.len() >= self.max_tabs {
            if let Some((evicted_id, evicted)) = self.tabs.shift_remove_index(0) {
                tracing::info!(tab_id = %evicted_id, "evicted LRU tab");
                if self.active_id.as_deref() == Some(&evicted_id) {
                    self.active_id = None;
                }
                evicted_pages.push(evicted.page);
            } else {
                break;
            }
        }
        self.active_id = Some(entry.id.clone());
        self.tabs.insert(entry.id.clone(), entry);
        evicted_pages
    }

    pub fn get(&mut self, id: &str) -> Option<&TabEntry> {
        // LRU touch: move to end on access
        if let Some(idx) = self.tabs.get_index_of(id) {
            self.tabs.move_index(idx, self.tabs.len() - 1);
        }
        self.tabs.get(id)
    }

    pub fn switch(&mut self, id: &str) -> Option<&TabEntry> {
        self.tabs.get_index_of(id)?;
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

    /// Read-only active lookup without the LRU touch, for read-only
    /// endpoints (audit 40 MED-7): `active()` needs `&mut` for the touch.
    pub fn active_ref(&self) -> Option<&TabEntry> {
        let id = self.active_id.as_ref()?;
        self.tabs.get(id)
    }

    /// Drop every tab entry (audit 40 HIGH-2). A browser relaunch invalidates
    /// all page handles from the previous instance; serving them would 502
    /// every tab-scoped endpoint until manual tab surgery.
    pub fn clear(&mut self) {
        self.tabs.clear();
        self.active_id = None;
    }

    /// Record a verification baseline hash for a tab (audit 41 MED-4).
    /// Called once navigation settles; a failed/uncapturable baseline
    /// leaves None (unknown), which is honest — never a false match.
    /// Returns false when the tab id is unknown (evicted mid-flight).
    pub fn set_baseline(&mut self, id: &str, hash: u64) -> bool {
        match self.tabs.get_mut(id) {
            Some(e) => {
                e.baseline_hash = Some(hash);
                true
            }
            None => false,
        }
    }

    /// Write the LANDED url/title back into a tab entry (audit 42
    /// Medium-4). `/browser/tabs` must show where the tab actually is,
    /// not the URL that was requested before redirects. Returns false
    /// when the tab id is unknown (evicted mid-flight).
    pub fn set_url_title(&mut self, id: &str, url: &str, title: &str) -> bool {
        match self.tabs.get_mut(id) {
            Some(e) => {
                e.url = url.to_string();
                e.title = title.to_string();
                true
            }
            None => false,
        }
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

/// Rate-limiter buckets keyed by (`client_identity`, `raw_path`).
///
/// NOTE: the rate-limit middleware runs as a `Router::layer`, i.e. BEFORE
/// route matching, so `MatchedPath` is never populated there and the key
/// always uses the raw request path (audit HIGH-4). Unbounded growth from
/// 404-scan path variants is contained by idle-bucket eviction in the
/// middleware (cap 10k entries, 120s idle TTL).
///
/// `client_identity` is the client socket IP (see
/// `middleware::client_identity)`: the presented `x-api-key` is UNVERIFIED at
/// that layer (auth runs after), so keying on it would let a brute-forcer
/// rotate keys for a fresh bucket per request. Key rotation does not reset
/// buckets. (Audit 41 LOW-3: an older comment here claimed key-hash
/// identity; the code has used socket IP since audit 40 HIGH-3.)
pub type RateBuckets = HashMap<
    (String, String),
    (
        governor::RateLimiter<
            governor::state::NotKeyed,
            governor::state::InMemoryState,
            governor::clock::DefaultClock,
        >,
        // Last-seen instant, for idle eviction.
        std::time::Instant,
    ),
>;

/// Operational metrics (replaces the legacy `_metrics` dict).
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
    /// Per-tab [ref=N] -> backendDOMNodeId mappers (perception plane)
    pub ax_mappers: Mutex<HashMap<String, AxMapper>>,
    /// Parked persistent X11 connection: (display, connection, screen).
    /// A visible click takes it out, issues FRESH queries on the reused
    /// socket, and parks it back on success; on error the socket is
    /// dropped so the next click reconnects. Std Mutex, NEVER held
    /// across .await (blocking X11 I/O — take/park inside spawn_blocking).
    pub x11: StdMutex<Option<(String, x11rb::rust_connection::RustConnection, usize)>>,
    /// Geometry fast-path memo: (window id, cached_at, (viewport_w, viewport_h) as i32).
    /// Memoizes ONLY the window id — the rect is re-queried FRESH from X11
    /// on every click via `refresh_window_rect`, so a moved window can
    /// never pass the gate on a stale rect. TTL 3s; invalidated when the
    /// page reports different viewport dimensions. Std Mutex, only
    /// touched inside spawn_blocking.
    pub geom_cache: StdMutex<GeomCache>,
    /// Shutdown flag for graceful teardown (audit BUG-4)
    pub shutting_down: std::sync::atomic::AtomicBool,
    /// OS pid of the supervised Chrome child, published by the supervisor
    /// after every successful bring-up (0 = unknown / no child). The
    /// visible-input path matches it against `_NET_WM_PID` on X11 so the
    /// SEC-1 gate rect comes from the window manager, never from page JS
    /// (audit 42 Critical-1).
    pub chrome_pid: AtomicU32,
    /// Lease generation counter (audit 42 High-4): bumped on every lease
    /// acquire and release. Long mutation handlers capture it at admission
    /// and abort before any irreversible step when it changed mid-flight.
    pub lease_gen: AtomicU64,
    /// Mutating HTTP requests currently inside their handler (audit 42
    /// High-4). `/lease/acquire` is rejected while this is non-zero so a
    /// new lease can never be granted over someone's in-flight mutation.
    pub inflight_mutations: AtomicU64,
    /// Process start time, for honest /health uptime reporting (audit 41
    /// MED-2 — `uptime_s` used to report wall-clock epoch seconds).
    pub started_at: std::time::Instant,
    /// Lease ID for internal bypass (empty = no bypass)
    #[allow(dead_code)] // parked: internal bypass not yet enforced anywhere (audit LOW-1)
    pub internal_token: String,
}

impl BridgeState {
    /// True when the lease generation changed since `gen0` was captured.
    /// Long mutation handlers call this before each irreversible step and
    /// abort when it returns true (audit 42 High-4): a lease hand-over
    /// mid-flight must not let the previous holder's input land.
    pub fn lease_gen_changed(&self, gen0: u64) -> bool {
        self.lease_gen.load(std::sync::atomic::Ordering::SeqCst) != gen0
    }

    /// Click timing for the configured `interaction_profile` ("normal" =
    /// historical 30/40 ms sleeps; "fast" = 5/10 ms).
    pub fn click_timing(&self) -> crate::browser::actions::ClickTiming {
        crate::browser::actions::ClickTiming::from_profile(&self.config.interaction_profile)
    }

    pub fn new(config: Config) -> Self {
        Self {
            browser1: Mutex::new(Browser1::new()),
            pool: Mutex::new(PoolState::new(3)),
            tabs: RwLock::new(TabRegistry::new(10)),
            leases: Mutex::new(LeaseTable::new()),
            metrics: Mutex::new(Metrics::default()),
            rate_buckets: Mutex::new(HashMap::new()),
            ax_mappers: Mutex::new(HashMap::new()),
            x11: StdMutex::new(None),
            geom_cache: StdMutex::new(None),
            shutting_down: AtomicBool::new(false),
            chrome_pid: AtomicU32::new(0),
            lease_gen: AtomicU64::new(0),
            inflight_mutations: AtomicU64::new(0),
            started_at: std::time::Instant::now(),
            internal_token: Uuid::new_v4().to_string(),
            config,
        }
    }
}

pub type SharedState = Arc<BridgeState>;

/// Geometry fast-path memo: `Some((window_id, cached_at, (viewport_w, viewport_h)))`.
/// Memoizes only the window id; the rect is always re-queried fresh.
pub type GeomCache = Option<(u32, std::time::Instant, (i32, i32))>;
