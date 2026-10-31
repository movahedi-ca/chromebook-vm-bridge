//! Chrome implementation of [`BrowserBackend`](super::backend::BrowserBackend).
//!
//! Spawns `google-chrome-stable` headed via `std::process::Command` and
//! reuses the normal CDP connect path against the child's debug port.
//! chromiumoxide's `Browser::launch` is deliberately NOT used: its
//! websocket handshake deserializes wrong against Chrome 156.
//!
//! Launch flags are the locked-down set: the sandbox stays ON (no
//! `--no-sandbox`) and site isolation stays ON (no
//! `--disable-features=IsolateOrigins,site-per-process`) — both were
//! audit findings (SEC-3) in the previous generation. `--enable-automation`
//! needs no stripping: a directly-launched Chrome never adds it, so
//! `navigator.webdriver` is natively false.

use super::backend::{BackendConfig, BrowserBackend};
use std::future::Future;
use std::pin::Pin;
use std::process::{Child, Stdio};
use std::time::Duration;

/// How long to wait for the debug port after spawning.
const PORT_WAIT_SECS: u64 = 30;

const CHROME_FLAGS: &[&str] = &[
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-blink-features=AutomationControlled",
    "--window-position=0,0",
    "--window-size=1920,1080",
    "--disable-dev-shm-usage",
    "--disable-gpu",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-sync",
    "--disable-translate",
    "--disable-extensions",
    "--disable-default-apps",
    "--disable-ipc-flooding-protection",
    "--metrics-recording-only",
    "--mute-audio",
    // Avoids the ~25s D-Bus Secret Service stall on first profile use.
    "--password-store=basic",
];

pub struct ChromeBackend {
    child: Option<Child>,
}

impl ChromeBackend {
    pub fn new() -> Self {
        Self { child: None }
    }

    async fn port_answers(port: u16) -> bool {
        let url = format!("http://127.0.0.1:{}/json/version", port);
        match tokio::time::timeout(Duration::from_secs(2), reqwest::get(&url)).await {
            Ok(Ok(resp)) => resp.status().is_success(),
            _ => false,
        }
    }

    async fn wait_for_port(&self, port: u16) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(PORT_WAIT_SECS);
        while tokio::time::Instant::now() < deadline {
            if Self::port_answers(port).await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        anyhow::bail!("chrome debug port {} did not answer within {}s", port, PORT_WAIT_SECS)
    }

    /// Reap a dead child (or take back a live one). Returns true when the
    /// child is currently running.
    fn poll_child(&mut self) -> bool {
        match self.child.take() {
            Some(mut child) => match child.try_wait() {
                Ok(None) => {
                    self.child = Some(child);
                    true
                }
                _ => {
                    // Exited (or wait failed): reap to avoid a zombie.
                    let _ = child.wait();
                    false
                }
            },
            None => false,
        }
    }

    fn spawn_child(&mut self, cfg: &BackendConfig) -> anyhow::Result<()> {
        std::fs::create_dir_all(&cfg.profile_dir)?;
        let mut cmd = std::process::Command::new(&cfg.bin);
        cmd.arg(format!("--user-data-dir={}", cfg.profile_dir.display()))
            .arg(format!("--remote-debugging-port={}", cfg.debug_port))
            .args(CHROME_FLAGS)
            .arg("about:blank")
            .env("DISPLAY", &cfg.display)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawning {}: {}", cfg.bin, e))?;
        tracing::info!(pid = child.id(), profile = %cfg.profile_dir.display(), "chrome child spawned");
        self.child = Some(child);
        Ok(())
    }

    /// Best-effort removal of a stray browser holding our debug port (e.g.
    /// orphaned by a bridge crash). Scoped to our exact remote-debugging
    /// port flag so nothing else is touched.
    async fn clear_stray_listener(&self, port: u16) {
        let pattern = format!("chrome.*--remote-debugging-port={}", port);
        let status = tokio::process::Command::new("pkill")
            .arg("-f")
            .arg(&pattern)
            .status()
            .await;
        match status {
            Ok(st) if st.success() => {
                tracing::warn!(port, "killed stray browser holding the debug port");
            }
            _ => {
                tracing::info!(port, "no stray browser on the debug port (or pkill unavailable)");
            }
        }
        // Give the port a moment to go quiet.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

impl Default for ChromeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserBackend for ChromeBackend {
    fn name(&self) -> &'static str {
        "chrome"
    }

    fn ensure_running<'a>(
        &'a mut self,
        cfg: &'a BackendConfig,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if self.poll_child() {
                return Ok(());
            }
            // No live child: the bridge owns exactly one browser. If a
            // stray browser still holds our debug port (e.g. orphaned by a
            // bridge crash), clear it first so the fresh spawn gets a clean,
            // known state (no unknown tabs, no wedged CDP).
            if Self::port_answers(cfg.debug_port).await {
                self.clear_stray_listener(cfg.debug_port).await;
            }
            self.spawn_child(cfg)?;
            self.wait_for_port(cfg.debug_port).await?;
            tracing::info!(port = cfg.debug_port, "chrome debug port live");
            Ok(())
        })
    }

    fn is_up<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { self.poll_child() })
    }

    fn kill<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
                tracing::info!("chrome child killed");
            }
        })
    }

    fn cdp_ws_url<'a>(
        &'a self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let body = reqwest::get(format!("http://127.0.0.1:{}/json/version", port))
                .await
                .map_err(|e| anyhow::anyhow!("debugging port {} unreachable: {}", port, e))?
                .text()
                .await
                .map_err(|e| anyhow::anyhow!("reading /json/version: {}", e))?;
            let v: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| anyhow::anyhow!("parsing /json/version: {}", e))?;
            v.get("webSocketDebuggerUrl")
                .and_then(|u| u.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow::anyhow!("no webSocketDebuggerUrl in /json/version"))
        })
    }
}
