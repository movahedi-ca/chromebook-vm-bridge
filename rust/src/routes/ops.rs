//! Ops route handlers: health, metrics, render_package.

use crate::state::SharedState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

/// GET /health — exempt from auth, but REDACTED (audit SEC-8).
/// Never leaks `last_action`, tab URLs, or lease IDs.
pub async fn health(State(state): State<SharedState>) -> impl IntoResponse {
    let browser_alive = state.browser1.lock().await.is_alive();
    let tabs = state.tabs.read().await;
    let metrics = state.metrics.lock().await;

    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "browser_alive": browser_alive,
        "tab_count": tabs.list().len(),
        "uptime_s": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        // Redacted: no last_action, no URLs, no lease IDs (audit SEC-8)
        "requests_total": metrics.requests_total,
        "requests_4xx": metrics.requests_4xx,
        "requests_5xx": metrics.requests_5xx,
    }))
}

/// GET /metrics — operational counters (replaces Python _metrics dict).
pub async fn metrics(State(state): State<SharedState>) -> impl IntoResponse {
    let m = state.metrics.lock().await;
    Json(json!({
        "ok": true,
        "requests_total": m.requests_total,
        "requests_4xx": m.requests_4xx,
        "requests_5xx": m.requests_5xx,
        "relaunches": m.relaunches,
        "rejections_401": m.rejections_401,
        "rejections_429": m.rejections_429,
        "rejections_423": m.rejections_423,
    }))
}

/// POST /ops/render_package — returns the Co-Pilot HUD render package
/// (playbook §10: /ops/render_package). ADMIN scope required (see auth.rs).
pub async fn render_package(State(state): State<SharedState>) -> impl IntoResponse {
    // The HUD state is managed separately; this returns the current
    // render-ready package for external dashboards.
    let tabs = state.tabs.read().await;
    let browser_alive = state.browser1.lock().await.is_alive();

    Json(json!({
        "ok": true,
        "package": {
            "browser_alive": browser_alive,
            "tabs": tabs.list(),
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }
    }))
}
