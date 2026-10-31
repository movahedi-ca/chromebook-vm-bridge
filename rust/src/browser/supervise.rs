//! Browser supervisor: the bridge owns exactly one browser daemon.
//!
//! This is the inner supervision ring (systemd is the outer one). The
//! supervisor spawns the browser at startup, connects the CDP driver,
//! watches the child process, and relaunches with capped backoff when it
//! dies. It talks only to [`BrowserBackend`](super::backend::BrowserBackend),
//! never to an engine directly.
//!
//! Health is split in two on purpose:
//! - the supervisor watches the *process* (did the child exit?),
//! - the watchdog watches the *CDP session* (does evaluate answer?).
//!   Either one marking the browser dead funnels into the same respawn path
//!   here. `metrics.relaunches` increments only on real recovery, never on
//!   boot.

use crate::browser::attach::spawn_handler;
use crate::browser::backend::{BackendConfig, BrowserBackend};
use crate::browser::tabs::wait_for_x11;
use crate::state::SharedState;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// Seconds between process-liveness polls while the browser is healthy.
const POLL_SECS: u64 = 5;
/// Hard timeout for the CDP connect handshake.
const CONNECT_TIMEOUT_SECS: u64 = 15;
/// Hard timeout for the post-connect smoke test.
const SMOKE_TIMEOUT_SECS: u64 = 10;
/// Relaunch backoff: 2^failures seconds, capped here.
const MAX_BACKOFF_SECS: u64 = 30;

/// Connect the CDP driver to the backend's debug port and smoke-test it.
///
/// SEAM: this is the one function that assumes a CDP-speaking engine. A
/// future non-CDP backend would replace this function with its own driver
/// handshake; the trait, the supervisor loop, and all routes stay untouched.
async fn connect_driver(
    state: &SharedState,
    backend: &mut dyn BrowserBackend,
    cfg: &BackendConfig,
) -> anyhow::Result<()> {
    let ws_url = backend.cdp_ws_url(cfg.debug_port).await?;
    let (browser, handler) = tokio::time::timeout(
        Duration::from_secs(CONNECT_TIMEOUT_SECS),
        chromiumoxide::Browser::connect(ws_url),
    )
    .await
    .map_err(|_| anyhow::anyhow!("CDP connect timed out"))?
    .map_err(|e| anyhow::anyhow!("CDP connect failed: {}", e))?;
    // The handler MUST be polled or the connection silently dies.
    spawn_handler(handler);
    let browser = Arc::new(browser);
    // Smoke test: a wedged browser can complete the handshake but never
    // answer. Kill it so the next loop iteration spawns a clean one.
    let smoke = tokio::time::timeout(Duration::from_secs(SMOKE_TIMEOUT_SECS), async {
        let page = browser.new_page("about:blank").await?;
        page.close().await?;
        Ok::<(), anyhow::Error>(())
    })
    .await;
    match smoke {
        Ok(Ok(())) => {
            state.browser1.lock().await.set_browser(browser);
            // A new browser instance invalidates every tab handle and ref
            // mapping from the previous one (audit 40 HIGH-2). Without this,
            // active_page() serves stale Arc<Page> handles and all /browser/*
            // routes 502 until manual tab surgery. On first boot the
            // registry is empty, so this is a no-op there.
            {
                let mut tabs = state.tabs.write().await;
                tabs.clear();
            }
            state.ax_mappers.lock().await.clear();
            Ok(())
        }
        _ => {
            tracing::warn!("browser failed CDP smoke test; killing for a clean respawn");
            backend.kill().await;
            anyhow::bail!("browser wedged (smoke test failed)")
        }
    }
}

/// Run the supervision loop forever (until shutdown). Owns `backend`.
pub async fn run_supervisor(mut backend: Box<dyn BrowserBackend>, state: SharedState) {
    tracing::info!(backend = backend.name(), "browser supervisor starting");
    let cfg = BackendConfig {
        bin: state.config.chrome_bin.clone(),
        profile_dir: state.config.profile_dir.clone(),
        display: state.config.display.clone(),
        debug_port: 9222,
    };
    // Chrome must never race the X server.
    if let Err(e) = wait_for_x11(&cfg.display).await {
        tracing::error!("X11 not ready ({}); will keep retrying bring-up", e);
    }

    let mut booted = false;
    let mut failures: u32 = 0;

    loop {
        if state.shutting_down.load(Ordering::SeqCst) {
            tracing::info!("supervisor shutting down; killing browser");
            backend.kill().await;
            break;
        }

        let alive = state.browser1.lock().await.is_alive();
        if !alive {
            let bring_up = async {
                backend.ensure_running(&cfg).await?;
                connect_driver(&state, &mut *backend, &cfg).await
            };
            match bring_up.await {
                Ok(()) => {
                    failures = 0;
                    if booted {
                        state.metrics.lock().await.relaunches += 1;
                        tracing::warn!("browser relaunched and healthy");
                    } else {
                        tracing::info!("browser online");
                    }
                    booted = true;
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    let backoff = std::cmp::min(MAX_BACKOFF_SECS, 1u64 << failures.min(5));
                    tracing::error!(error = %e, "browser bring-up failed; retry in {}s", backoff);
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    continue;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(POLL_SECS)).await;

        // Process-level monitor. If our child exited, mark dead so the next
        // iteration respawns it. (CDP-level wedges are the watchdog's job;
        // it marks dead and lands on the same path.)
        if !backend.is_up().await {
            let was_alive = {
                let mut b = state.browser1.lock().await;
                let was = b.is_alive();
                if was {
                    b.mark_dead();
                }
                was
            };
            if was_alive {
                tracing::warn!("browser process exited; will respawn");
            }
        }
    }
}
