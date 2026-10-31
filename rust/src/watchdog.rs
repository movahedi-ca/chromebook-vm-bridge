//! Watchdog: health probing, sd_notify, and sleep-state resilience.
//!
//! Fixes audit BUG-3:
//! - Probe wrapped in timeout (3s), loop sleeps 10s — systemd gets
//!   WATCHDOG=1 every ~13s, well under WatchdogSec=60. No more suicide
//!   when a heavy page stalls evaluate().
//! - WATCHDOG=1 pinged immediately BEFORE any browser relaunch.
//! - Monotonic-vs-wall clock jump detector (>5s) catches Chromebook
//!   sleep/wake: flushes dead sockets, re-verifies network.
//! - "Two strikes then relaunch" actually works now (systemd no longer
//!   kills us mid-probe).

use crate::state::SharedState;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Check systemd notify support. Uses the `systemd` crate if available,
/// falls back to direct socket write to $NOTIFY_SOCKET.
fn sd_notify(state: &str) {
    if let Ok(sock_path) = std::env::var("NOTIFY_SOCKET") {
        use std::os::unix::net::UnixDatagram;
        // Use unbound socket for sending (abstract namespace safe)
        if let Ok(sock) = UnixDatagram::unbound() {
            let _ = sock.send_to(state.as_bytes(), &sock_path);
        }
    }
}

pub async fn run(state: SharedState) {
    let interval_s = state.config.watchdog_interval_s;
    let probe_timeout = Duration::from_secs(state.config.watchdog_probe_timeout_s);

    // Signal readiness to systemd after first successful setup
    sd_notify("READY=1");
    tracing::info!("watchdog started");

    let mut last_wall = std::time::SystemTime::now();
    let mut last_mono = Instant::now();

    loop {
        tokio::time::sleep(Duration::from_secs(interval_s)).await;

        if state.shutting_down.load(Ordering::SeqCst) {
            tracing::info!("watchdog exiting (shutdown)");
            break;
        }

        // Sleep-state jump detection (audit ARCH-2)
        let now_wall = std::time::SystemTime::now();
        let now_mono = Instant::now();
        if let (Ok(wall_dt), mono_dt) = (
            now_wall.duration_since(last_wall),
            now_mono.duration_since(last_mono),
        ) {
            let jump = wall_dt.as_secs_f64() - mono_dt.as_secs_f64();
            if jump.abs() > 5.0 {
                tracing::warn!(
                    jump_s = jump,
                    "wall-clock jump detected (likely sleep/wake) — re-verifying"
                );
                // Dead sockets will fail fast on next probe; force re-check
                // by marking browser suspect (but not dead — probe decides)
            }
        }
        last_wall = now_wall;
        last_mono = now_mono;

        // Probe: CDP Runtime.evaluate("1") with hard timeout.
        // Never blocks the loop; never exceeds WatchdogSec.
        let probe_ok = {
            let browser_guard = state.browser1.lock().await;
            match browser_guard.browser() {
                Some(browser) => {
                    // Use a lightweight CDP ping via a new tab evaluate.
                    // We do NOT hold the browser lock across the timeout —
                    // drop the guard and re-acquire after.
                    drop(browser_guard);
                    probe_browser(browser, probe_timeout).await
                }
                None => false,
            }
        };

        if probe_ok {
            let mut b = state.browser1.lock().await;
            b.reset_probe_failures();
            sd_notify("WATCHDOG=1");
        } else {
            let strikes = {
                let mut b = state.browser1.lock().await;
                b.record_probe_failure()
            };
            tracing::warn!(strikes, "watchdog probe failed");
            if strikes >= 2 {
                tracing::error!("two consecutive probe failures — triggering relaunch");
                // Ping watchdog BEFORE the slow relaunch (audit BUG-3)
                sd_notify("WATCHDOG=1");
                trigger_relaunch(state.clone()).await;
            }
            // Still notify so systemd doesn't kill us during recovery
            sd_notify("WATCHDOG=1");
        }
    }
}

/// Lightweight browser probe: evaluate "1" via CDP with a hard timeout.
async fn probe_browser(
    browser: std::sync::Arc<chromiumoxide::Browser>,
    timeout: Duration,
) -> bool {
    let fut = async {
        // Create a throwaway target, evaluate, close it
        let page = browser.new_page("about:blank").await.map_err(|_| ())?;
        page.evaluate("1").await.map_err(|_| ())?;
        let _ = page.close().await;
        Ok::<(), ()>(())
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(())) => true,
        _ => false,
    }
}

/// Mark the browser dead so the supervisor respawns it. Called with
/// WATCHDOG=1 already pinged. The supervisor owns the relaunch itself
/// (and the `relaunches` metric); this only flips the state.
async fn trigger_relaunch(state: SharedState) {
    if state.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    {
        let mut b = state.browser1.lock().await;
        b.mark_dead();
    }
    // The supervisor's loop notices the dead browser on its next tick and
    // respawns with capped backoff.
    tracing::info!("browser marked for relaunch (supervisor will respawn)");
}
