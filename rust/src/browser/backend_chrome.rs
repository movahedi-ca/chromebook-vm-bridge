//! Chrome implementation of [`super::backend::BrowserBackend`].
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
        let url = format!("http://127.0.0.1:{port}/json/version");
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
        anyhow::bail!("chrome debug port {port} did not answer within {PORT_WAIT_SECS}s")
    }

    /// Reap a dead child (or take back a live one). Returns true when the
    /// child is currently running.
    fn poll_child(&mut self) -> bool {
        match self.child.take() {
            Some(mut child) => {
                if let Ok(None) = child.try_wait() {
                    self.child = Some(child);
                    true
                } else {
                    // Exited (or wait failed): reap to avoid a zombie.
                    let _ = child.wait();
                    false
                }
            }
            None => false,
        }
    }

    fn spawn_child(&mut self, cfg: &BackendConfig) -> anyhow::Result<()> {
        std::fs::create_dir_all(&cfg.profile_dir)?;
        let mut cmd = std::process::Command::new(&cfg.bin);
        cmd.arg(format!("--user-data-dir={}", cfg.profile_dir.display()))
            .arg(format!("--remote-debugging-port={}", cfg.debug_port))
            // Audit 42 High-2: without this flag the DevTools port binds
            // beyond loopback — an unauthenticated CDP control plane for
            // any local process. Pin it to 127.0.0.1.
            .arg("--remote-debugging-address=127.0.0.1")
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
    /// orphaned by a bridge crash). PID-based (audit MED-4): scans /proc
    /// for processes whose exe is a chrome binary AND whose cmdline carries
    /// our exact --remote-debugging-port flag, instead of `pkill -f` on a
    /// regex that could match an unrelated process.
    async fn clear_stray_listener(&self, port: u16) {
        let flag = format!("--remote-debugging-port={port}");
        let mut killed = 0;
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let pid: i32 = match entry.file_name().to_string_lossy().parse() {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let exe_ok = std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|p| {
                    p.file_name().is_some_and(|n| {
                        let n = n.to_string_lossy();
                        n.contains("chrome") || n.contains("chromium")
                    })
                });
                if !exe_ok {
                    continue;
                }
                let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                if cmdline.split('\0').any(|a| a == flag) {
                    // SIGTERM first (lets Chrome clean its profile); the
                    // supervisor's next spawn reaps anything left.
                    let _ = std::process::Command::new("kill")
                        .arg("-TERM")
                        .arg(pid.to_string())
                        .status();
                    killed += 1;
                }
            }
        }
        if killed > 0 {
            tracing::warn!(
                port,
                killed,
                "sent SIGTERM to stray chrome holding the debug port"
            );
        } else {
            tracing::info!(port, "no stray browser on the debug port");
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

    /// OS pid of the supervised Chrome child (audit 42 Critical-1: the
    /// visible-input path matches this against `_NET_WM_PID` on X11).
    fn child_pid(&self) -> Option<u32> {
        self.child.as_ref().map(std::process::Child::id)
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
                // Graceful TERM first (audit MED-4): a SIGKILL'd Chrome never
                // cleans its profile, so the next launch lands on the
                // "didn't shut down correctly" restore dialog and blocks
                // automation. Escalate to KILL only after a 5s grace period.
                #[cfg(unix)]
                {
                    use std::process::Command as ProcCommand;
                    let pid = child.id();
                    let _ = ProcCommand::new("kill")
                        .arg("-TERM")
                        .arg(pid.to_string())
                        .status();
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) if std::time::Instant::now() < deadline => {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                            _ => {
                                let _ = child.kill(); // escalate: SIGKILL
                                let _ = child.wait();
                                break;
                            }
                        }
                    }
                    tracing::info!("chrome child terminated");
                }
                #[cfg(not(unix))]
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::info!("chrome child killed");
                }
            }
        })
    }

    fn cdp_ws_url<'a>(
        &'a self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let body = reqwest::get(format!("http://127.0.0.1:{port}/json/version"))
                .await
                .map_err(|e| anyhow::anyhow!("debugging port {port} unreachable: {e}"))?
                .text()
                .await
                .map_err(|e| anyhow::anyhow!("reading /json/version: {e}"))?;
            let v: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| anyhow::anyhow!("parsing /json/version: {e}"))?;
            v.get("webSocketDebuggerUrl")
                .and_then(|u| u.as_str())
                .map(std::string::ToString::to_string)
                .ok_or_else(|| anyhow::anyhow!("no webSocketDebuggerUrl in /json/version"))
        })
    }
}
