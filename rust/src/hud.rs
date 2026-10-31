//! Co-Pilot HUD: always-on-top status panel on display :1 (playbook §11.1).
//!
//! A tiny X11 window pinned top-right showing bridge status:
//! green WORKING / idle, current task, last action. The human watching
//! the Chromebook display always knows what the agent is doing.
//!
//! Implemented as a minimal X11 window (no toolkit dependency).

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// HUD state shared between the HTTP handlers and the HUD thread.
#[derive(Debug, Clone, Default)]
pub struct HudState {
    pub status: String,      // "WORKING" | "IDLE" | "ERROR"
    pub task: String,        // current task description
    pub last_action: String, // last completed action
}

pub struct Hud {
    state: Arc<Mutex<HudState>>,
    /// Shutdown signal
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Hud {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(HudState {
                status: "IDLE".to_string(),
                ..Default::default()
            })),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn state(&self) -> Arc<Mutex<HudState>> {
        self.state.clone()
    }

    /// Set the current task (called by route handlers).
    pub fn set_task(&self, task: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.status = "WORKING".to_string();
            s.task = task.to_string();
        }
    }

    /// Mark idle.
    pub fn set_idle(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.status = "IDLE".to_string();
            s.task.clear();
        }
    }

    /// Record a completed action.
    pub fn set_action(&self, action: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.last_action = action.to_string();
        }
    }

    /// Spawn the HUD window thread. Runs until `stop()` is called.
    /// Currently a stub — the X11 window implementation lands in Phase 6.
    /// The state API above is already wired into routes.
    pub fn spawn(self: Arc<Self>) {
        let state = self.state.clone();
        let stop = self.stop.clone();
        std::thread::spawn(move || {
            tracing::info!("HUD thread started (stub — window impl pending)");
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                // TODO Phase 6: create X11 override-redirect window,
                // render status via Xft, update on state change.
                std::thread::sleep(Duration::from_secs(1));
                // Touch state to keep the lock alive (placeholder)
                let _guard = state.lock();
                drop(_guard);
            }
            tracing::info!("HUD thread stopped");
        });
    }

    pub fn stop(&self) {
        self.stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Default for Hud {
    fn default() -> Self {
        Self::new()
    }
}
