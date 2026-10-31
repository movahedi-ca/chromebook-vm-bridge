//! Route handlers.

pub mod advanced;
pub mod browser;
pub mod input;
pub mod lease;
pub mod ops;
pub mod pool;
pub mod read;

use crate::state::SharedState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

/// Standard "not yet implemented" response for stub handlers.
/// Every stub returns 501 so contract tests can distinguish
/// "not built" from "broken".
pub async fn not_implemented(name: &str) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"ok": false, "error": format!("not_implemented: {}", name)})),
    )
}

/// Helper: require the browser to be alive, else 503.
#[allow(dead_code)] // shared guard for future handlers; no caller yet (audit LOW-1)
#[allow(clippy::result_large_err)] // axum Response is large by design; not worth boxing
pub async fn require_browser(state: &SharedState) -> Result<(), axum::response::Response> {
    if state.browser1.lock().await.is_alive() {
        Ok(())
    } else {
        Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "browser not running"})),
        )
            .into_response())
    }
}

#[allow(unused)]
pub async fn _state(_s: State<SharedState>) -> impl IntoResponse {
    Json(json!({"ok": true}))
}
