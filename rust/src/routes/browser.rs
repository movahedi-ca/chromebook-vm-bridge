//! Browser route handlers — core integration.
//!
//! Every handler follows the pattern:
//! 1. `ensure_browser()` — launch if needed (holds lock, no duplicate launch)
//! 2. Get/create the active tab
//! 3. Run the CDP/JS operation with timeout
//! 4. Return JSON with token-efficient shapes

use crate::browser::tabs::ensure_browser;
use crate::privy::validate_navigate_url;
use crate::routes::not_implemented;
use crate::routes::read::page_url_title;
use crate::state::SharedState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct NavigateRequest {
    pub url: String,
}

/// POST /browser/navigate — SSRF-filtered navigation (audit SEC-6).
pub async fn navigate(
    State(state): State<SharedState>,
    Json(req): Json<NavigateRequest>,
) -> impl IntoResponse {
    let url = match validate_navigate_url(&req.url, &[]).await {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };
    let browser = match ensure_browser(&state).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"ok": false, "error": format!("browser launch failed: {}", e)})),
            )
                .into_response()
        }
    };
    let page = {
        let mut tabs = state.tabs.write().await;
        if let Some(active) = tabs.active() {
            active.page.clone()
        } else {
            match browser.new_page(url.as_str()).await {
                Ok(p) => {
                    let page = std::sync::Arc::new(p);
                    let id = uuid::Uuid::new_v4().to_string();
                    tabs.insert(crate::state::TabEntry {
                        id: id.clone(),
                        page: page.clone(),
                        baseline_hash: None,
                        url: url.to_string(),
                        title: String::new(),
                    });
                    page
                }
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"ok": false, "error": format!("new page: {}", e)})),
                    )
                        .into_response()
                }
            }
        }
    };
    let nav_result = tokio::time::timeout(Duration::from_secs(30), async {
        page.goto(url.as_str()).await
    })
    .await;
    match nav_result {
        Ok(Ok(_)) => {
            let (final_url, title) = page_url_title(&page).await;
            (
                StatusCode::OK,
                Json(json!({"title": title, "url": final_url, "verified": true})),
            )
                .into_response()
        }
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "error": format!("navigation failed: {}", e)})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({"ok": false, "error": "navigation timeout"})),
        )
            .into_response(),
    }
}

/// Stubs (501) for endpoints whose handlers are not built yet. Each is
/// registered in main.rs and returns the standard not_implemented shape
/// so contract tests can distinguish "not built" from "broken".
pub async fn fill_react(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("fill_react").await
}
pub async fn fill_date(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("fill_date").await
}
pub async fn paste_atomic(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("paste_atomic").await
}
pub async fn restore_checkpoint(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("restore_checkpoint").await
}
pub async fn som_screenshot(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("som_screenshot").await
}
pub async fn diff_state(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("diff_state").await
}
pub async fn vault_login(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("vault_login").await
}
