//! CDP handler pump helper.
//!
//! The chromiumoxide `Handler` MUST be polled for the connection to live:
//! dropping it kills the websocket. Every connect path spawns the handler
//! pump as a background task. (Browser spawning itself lives behind the
//! [`BrowserBackend`](super::backend::BrowserBackend) trait; the CDP
//! connect step is in `supervise.rs`.)

use chromiumoxide::Handler;
use futures_util::StreamExt;

/// Drive the handler event stream until it ends. Spawn this; never drop
/// the Handler without polling it.
pub fn spawn_handler(mut handler: Handler) {
    tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if event.is_err() {
                tracing::warn!("browser handler stream ended with error");
                break;
            }
        }
        tracing::warn!("browser handler stream ended");
    });
}
