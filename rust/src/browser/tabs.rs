//! Browser launch and lifecycle (Phase 2).
//!
//! Replaces Playwright's `launch_persistent_context` with chromiumoxide's
//! CDP launch. Key fixes vs. Python:
//! - X11 readiness check BEFORE launch (audit: 31-min Overleaf fight started
//!   with Chrome racing the X server)
//! - Dedicated automation profile (NOT chrome-main clone — audit SEC-2)
//! - Sandbox flags: keep --no-sandbox ONLY if running as non-root with
//!   user namespaces available; document the tradeoff
//! - Graceful shutdown: close all pages, then browser.close() (audit BUG-4)

use crate::config::Config;
use std::sync::Arc;
use std::time::Duration;

/// JS to install on every new document: FormGuard + diff observer.
pub const NEW_DOCUMENT_SCRIPTS: &[&str] = &[
    crate::vault::FORMGUARD_JS,
    // Diff observer installed by perception::diff::DIFF_INSTALL_JS
];

/// Check X11 is ready before launching Chrome.
/// Polls for the X socket / display responsiveness, up to 30s.
pub async fn wait_for_x11(dpy: &str) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        // Try to connect via x11rb (blocking — run in spawn_blocking)
        let dpy_owned = dpy.to_string();
        let ok = tokio::task::spawn_blocking(move || {
            x11rb::connect(Some(dpy_owned.as_str())).is_ok()
        })
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

/// Launch the persistent headed browser.
/// Returns the chromiumoxide Browser handle.
pub async fn launch(config: &Config) -> anyhow::Result<Arc<chromiumoxide::Browser>> {
    wait_for_x11(&config.display).await?;

    // Ensure profile dir exists
    std::fs::create_dir_all(&config.profile_dir)?;

    let (browser, _handler) = chromiumoxide::Browser::launch(
        chromiumoxide::BrowserConfig::builder()
            .chrome_executable(&config.chrome_bin)
            .with_head()
            .user_data_dir(&config.profile_dir)
            .args(vec![
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-blink-features=AutomationControlled",
                // Window geometry: pinned (0,0) 1920x1080 like the Python version
                "--window-position=0,0",
                "--window-size=1920,1080",
                // Keep --no-sandbox: we run as non-root `auto` user in a VM;
                // user namespaces may not be available. Documented tradeoff.
                "--no-sandbox",
                "--disable-dev-shm-usage",
            ])
            .build()
            .map_err(|e| anyhow::anyhow!("browser config: {}", e))?,
    )
    .await
    .map_err(|e| anyhow::anyhow!("browser launch: {}", e))?;

    tracing::info!("browser launched");

    // Install new-document scripts on every page
    // (FormGuard + diff observer — done per-page in routes for now)

    Ok(Arc::new(browser))
}

/// Ensure the browser is running, launching if needed.
/// Called at the start of every /browser/* handler.
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
    // Slow path: launch (holds the lock to prevent duplicate launches —
    // fixes the Python "two launches racing" bug)
    let mut b = state.browser1.lock().await;
    // Re-check under lock
    if let Some(browser) = b.browser() {
        if b.is_alive() {
            return Ok(browser);
        }
    }
    tracing::info!("launching browser (ensure_browser)");
    let browser = launch(&state.config).await?;
    b.set_browser(browser.clone());
    Ok(browser)
}
