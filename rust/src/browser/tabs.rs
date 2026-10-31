//! Browser lifecycle accessors.
//!
//! The supervisor task owns spawning and reconnecting (see `supervise.rs`).
//! `ensure_browser` is the read path for routes: fast path when the browser
//! is alive, bounded wait while the supervisor is (re)spawning.
//!
//! - X11 readiness check BEFORE first spawn (Chrome must never race the X server)

use std::sync::Arc;
use std::time::Duration;

/// JS to install on every new document: FormGuard + diff observer.
///
/// NOTE (audit HIGH-2): FormGuard is currently DISABLED. The receiving
/// route (`/internal/checkpoint`) does not exist yet and the injected
/// fetch would fire doomed requests (console noise + CSP risk) on every
/// page. Re-enable by uncommenting below AND registering the internal
/// route with the ACT key injected as a closure variable.
#[allow(dead_code)] // parked scaffolding until HIGH-2 is properly implemented
pub const NEW_DOCUMENT_SCRIPTS: &[&str] = &[
    // crate::vault::FORMGUARD_JS, // disabled until /internal/checkpoint exists
    // Diff observer installed by perception::diff::DIFF_INSTALL_JS
];

/// Check X11 is ready before launching Chrome.
/// Polls for the X socket / display responsiveness, up to 30s.
pub async fn wait_for_x11(dpy: &str) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        // Try to connect via x11rb (blocking — run in spawn_blocking)
        let dpy_owned = dpy.to_string();
        let ok =
            tokio::task::spawn_blocking(move || x11rb::connect(Some(dpy_owned.as_str())).is_ok())
                .await
                .unwrap_or(false);
        if ok {
            tracing::info!("X11 display {} ready", dpy);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    anyhow::bail!("X11 display {} not ready after 30s", dpy)
}

/// Ensure the browser is running. The supervisor task owns the lifecycle
/// (spawn, connect, relaunch); this is the read path for routes: fast path
/// when the browser is alive, bounded wait while the supervisor is
/// (re)spawning. Called at the start of every /browser/* handler.
pub async fn ensure_browser(
    state: &crate::state::SharedState,
) -> anyhow::Result<Arc<chromiumoxide::Browser>> {
    // Fast path: already alive
    {
        let b = state.browser1.lock().await;
        if let Some(browser) = b.browser() {
            if b.is_alive() {
                return Ok(browser);
            }
        }
    }
    // Slow path: the supervisor is (re)spawning. Wait up to 60s rather
    // than failing fast — a respawn is seconds, not minutes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let b = state.browser1.lock().await;
        if let Some(browser) = b.browser() {
            if b.is_alive() {
                return Ok(browser);
            }
        }
    }
    anyhow::bail!("browser unavailable (supervisor still bringing it up)")
}
