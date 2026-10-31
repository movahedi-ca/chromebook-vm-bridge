//! Browser #1: persistent headed Chrome with the dedicated automation profile.
//!
//! Lifecycle: launched once at startup (after X11 readiness check), supervised
//! by watchdog.rs. Disconnects observed via chromiumoxide's event stream
//! (audit BUG-4: no more try/except around `_ctx.browser` which is always None).

pub mod geometry;
pub mod tabs;
pub mod verify;

use std::sync::Arc;

/// Handle to the persistent browser. All fields private; access via methods
/// that take `&mut self` under the `BridgeState::browser1` mutex.
pub struct Browser1 {
    browser: Option<Arc<chromiumoxide::Browser>>,
    /// Whether the browser is currently considered alive
    alive: bool,
    /// Consecutive failed health probes (watchdog)
    probe_failures: u32,
}

impl Browser1 {
    pub fn new() -> Self {
        Self {
            browser: None,
            alive: false,
            probe_failures: 0,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.alive
    }

    pub fn set_browser(&mut self, browser: Arc<chromiumoxide::Browser>) {
        self.browser = Some(browser);
        self.alive = true;
        self.probe_failures = 0;
    }

    pub fn mark_dead(&mut self) {
        self.alive = false;
        self.browser = None;
    }

    pub fn browser(&self) -> Option<Arc<chromiumoxide::Browser>> {
        self.browser.clone()
    }

    pub fn record_probe_failure(&mut self) -> u32 {
        self.probe_failures += 1;
        self.probe_failures
    }

    pub fn reset_probe_failures(&mut self) {
        self.probe_failures = 0;
    }
}

impl Default for Browser1 {
    fn default() -> Self {
        Self::new()
    }
}
