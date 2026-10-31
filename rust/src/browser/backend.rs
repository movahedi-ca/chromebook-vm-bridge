//! Browser backend trait: the swappable seam for the browser engine.
//!
//! SEAM: The bridge core (routes, auth, middleware, supervisor) talks only
//! to `BrowserBackend`, never to engine specifics. To swap the engine later
//! (e.g. a faster Rust-based browser), implement this trait for the new
//! engine and construct it in `main.rs` — routes, auth, middleware, and the
//! supervisor loop need no changes. The CDP `connect` step in
//! `supervise.rs` is the one place that assumes a CDP-speaking engine; a
//! non-CDP engine would replace that single function with its own driver
//! handshake.
//!
//! Async methods are written as manually-boxed futures (instead of the
//! `async-trait` crate) to keep the dependency tree exactly as it is.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Launch parameters for a browser backend. Engine-agnostic: headed mode,
/// display, profile dir, and debug port are concepts every desktop browser
/// engine in this role understands.
#[derive(Debug, Clone)]
pub struct BackendConfig {
    /// Browser executable (e.g. "google-chrome-stable")
    pub bin: String,
    /// Dedicated automation profile directory (created if missing)
    pub profile_dir: PathBuf,
    /// X11 display for headed mode (e.g. ":1")
    pub display: String,
    /// Remote-debugging / driver port (e.g. 9222)
    pub debug_port: u16,
}

pub trait BrowserBackend: Send + Sync {
    /// Human name for logs (e.g. "chrome").
    fn name(&self) -> &'static str;

    /// Ensure a browser is running and its debug port answers. Spawns our
    /// own child process when nothing is live. After `Ok`, the backend
    /// tracks that browser until `kill`.
    fn ensure_running<'a>(
        &'a mut self,
        cfg: &'a BackendConfig,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

    /// True if the tracked browser process is still running.
    fn is_up<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    /// Terminate the tracked browser (best effort). Used on shutdown and
    /// when a wedged browser must be replaced with a clean one.
    fn kill<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

    /// Driver websocket URL for the debug port, for the connect step in
    /// `supervise.rs`.
    fn cdp_ws_url<'a>(
        &'a self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + 'a>>;

    /// OS pid of the supervised browser child, if the backend tracks one.
    /// The visible-input path uses it to resolve the Chrome window from
    /// X11 ground truth (`_NET_WM_PID`); page-reported geometry is never
    /// trusted for the input gate (audit 42 Critical-1). Default: unknown.
    fn child_pid(&self) -> Option<u32> {
        None
    }
}
