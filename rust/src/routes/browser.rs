//! Browser route handlers — core integration.
//!
//! Every handler follows the pattern:
//! 1. `ensure_browser()` — launch if needed (holds lock, no duplicate launch)
//! 2. Get/create the active tab
//! 3. Run the CDP/JS operation with timeout
//! 4. Return JSON with token-efficient shapes

use crate::browser::tabs::ensure_browser;
use crate::privy::validate_navigate_url;
use crate::routes::{not_implemented, require_browser};
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
    let url = match validate_navigate_url(&req.url, &[]) {
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
        Ok(Ok(_)) => (
            StatusCode::OK,
            Json(json!({"ok": true, "url": url.as_str()})),
        )
            .into_response(),
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

pub async fn text(State(state): State<SharedState>) -> impl IntoResponse {
    if let Err(r) = require_browser(&state).await {
        return r;
    }
    not_implemented("text").await.into_response()
}
pub async fn html(State(state): State<SharedState>) -> impl IntoResponse {
    if let Err(r) = require_browser(&state).await {
        return r;
    }
    not_implemented("html").await.into_response()
}
pub async fn snapshot(State(state): State<SharedState>) -> impl IntoResponse {
    if let Err(r) = require_browser(&state).await {
        return r;
    }
    not_implemented("snapshot").await.into_response()
}
pub async fn state(State(state): State<SharedState>) -> impl IntoResponse {
    let tabs = state.tabs.read().await;
    Json(json!({"ok": true, "tabs": tabs.list()})).into_response()
}
pub async fn tabs(State(state): State<SharedState>) -> impl IntoResponse {
    let tabs = state.tabs.read().await;
    Json(json!({"ok": true, "tabs": tabs.list()})).into_response()
}
pub async fn screenshot(State(state): State<SharedState>) -> impl IntoResponse {
    if let Err(r) = require_browser(&state).await {
        return r;
    }
    not_implemented("screenshot").await.into_response()
}
pub async fn screenshot_hash(State(state): State<SharedState>) -> impl IntoResponse {
    if let Err(r) = require_browser(&state).await {
        return r;
    }
    not_implemented("screenshot_hash").await.into_response()
}
pub async fn click(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("click").await }
pub async fn click_at(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("click_at").await }
pub async fn click_node(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("click_node").await }
pub async fn click_screen(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("click_screen").await }
pub async fn double_click(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("double_click").await }
pub async fn drag(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("drag").await }
pub async fn move_to(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("move_to").await }
pub async fn move_to_viewport(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("move_to_viewport").await }
pub async fn scroll(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("scroll").await }
pub async fn fill(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("fill").await }
pub async fn type_text(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("type_text").await }
pub async fn type_verified(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("type_verified").await }
pub async fn press(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("press").await }
pub async fn wait(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("wait").await }
pub async fn eval(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("eval").await }
pub async fn script(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("script").await }
pub async fn order(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("order").await }
pub async fn autocomplete(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("autocomplete").await }
pub async fn geometry_refresh(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("geometry_refresh").await }
pub async fn assert_visual(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("assert_visual").await }
pub async fn screenshot_compare(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("screenshot_compare").await }
pub async fn screenshot_delta(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("screenshot_delta").await }
pub async fn screenshot_region(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("screenshot_region").await }
pub async fn tab_new(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("tab_new").await }
pub async fn tab_switch(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("tab_switch").await }
pub async fn upload(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("upload").await }
pub async fn fill_react(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("fill_react").await }
pub async fn fill_date(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("fill_date").await }
pub async fn paste_atomic(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("paste_atomic").await }
pub async fn restore_checkpoint(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("restore_checkpoint").await }
pub async fn som_screenshot(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("som_screenshot").await }
pub async fn diff_state(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("diff_state").await }
pub async fn vault_login(State(_state): State<SharedState>) -> impl IntoResponse { not_implemented("vault_login").await }
