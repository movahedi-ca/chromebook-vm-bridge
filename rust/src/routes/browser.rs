//! Browser route handlers — core integration.
//!
//! Every handler follows the pattern:
//! 1. `ensure_browser()` — launch if needed (holds lock, no duplicate launch)
//! 2. Get/create the active tab
//! 3. Run the CDP/JS operation with timeout
//! 4. Return JSON with token-efficient shapes

use crate::browser::actions::{install_blocked_url_patterns, navigate_with_mode, NavWaitMode};
use crate::browser::tabs::ensure_browser;
use crate::routes::not_implemented;
use crate::routes::read::{capture_png, page_url_title};
use crate::state::SharedState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct NavigateRequest {
    pub url: String,
    /// Lifecycle wait mode: "networkidle" (default — historical
    /// `page.goto()` behavior), "load", "domcontentloaded", "commit".
    #[serde(default)]
    pub wait_until: String,
}

/// POST /browser/navigate — SSRF-filtered navigation (audit SEC-6).
pub async fn navigate(
    State(state): State<SharedState>,
    Json(req): Json<NavigateRequest>,
) -> impl IntoResponse {
    let url = match crate::privy::validated_url_or_400(&req.url).await {
        Ok(u) => u,
        Err(r) => return r,
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
    let (page, tab_id) = match acquire_nav_tab(&state, &browser, url.as_str()).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    // Defense in depth: literal-IP block patterns on this target, so a
    // literal-IP subresource/redirect inside the renderer is blocked even
    // if a Rust-side check were ever bypassed (audit 42 High-1).
    // Best-effort; the Rust firewall stays the primary gate.
    install_blocked_url_patterns(&page, &state.config.blocked_resource_patterns).await;
    // `wait_until` is honored: "networkidle" (default) keeps the
    // historical `page.goto()` path; early modes register the CDP
    // lifecycle listener before `Page.navigate` fires.
    let mode = NavWaitMode::parse(&req.wait_until);
    let nav_result = navigate_with_mode(&page, url.as_str(), mode, Duration::from_secs(30)).await;
    match nav_result {
        Ok(()) => {
            let (final_url, title) = page_url_title(&page).await;
            finish_navigation(&state, &tab_id, &page, &final_url, &title).await
        }
        Err(e) => {
            if e.to_string().contains("timeout") {
                (
                    StatusCode::GATEWAY_TIMEOUT,
                    Json(json!({"ok": false, "error": "navigation timeout"})),
                )
                    .into_response()
            } else {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"ok": false, "error": format!("navigation failed: {e}")})),
                )
                    .into_response()
            }
        }
    }
}

/// Reuse the active tab under a read lock, else open a new page and
/// register it. The read guard is dropped before the else branch: holding
/// it across `state.tabs.write()` would deadlock (the `if let` scrutinee
/// temporary lives for the whole if/else). No CDP call is ever made while
/// holding the tabs lock (audit 41 MED-1): opening a page is a slow
/// round-trip that used to block every read endpoint (`/browser/tabs`,
/// `/health`'s `tab_count`, ...) behind a write lock.
async fn acquire_nav_tab(
    state: &SharedState,
    browser: &chromiumoxide::Browser,
    url: &str,
) -> Result<(std::sync::Arc<chromiumoxide::Page>, String), Box<axum::response::Response>> {
    let active = {
        let tabs = state.tabs.read().await;
        tabs.active_ref().map(|a| (a.page.clone(), a.id.clone()))
    };
    if let Some((page, id)) = active {
        Ok((page, id))
    } else {
        // 503: the browser (our dependency) failed to open a
        // page — retryable, not a bridge bug (audit 41 LOW-10).
        let p = browser.new_page(url).await.map_err(|e| {
            Box::new(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"ok": false, "error": format!("new page: {}", e)})),
                )
                    .into_response(),
            )
        })?;
        let page = std::sync::Arc::new(p);
        let id = uuid::Uuid::new_v4().to_string();
        // Write lock held only for the registry insert.
        let evicted = state.tabs.write().await.insert(crate::state::TabEntry {
            id: id.clone(),
            page: page.clone(),
            baseline_hash: None,
            url: url.to_string(),
            title: String::new(),
        });
        for p in evicted {
            crate::browser::actions::close_page(&p).await;
        }
        Ok((page, id))
    }
}

/// Post-goto handling for `/browser/navigate`: re-run the SSRF firewall
/// on the LANDED url (audit 42 High-1 — the requested URL passed
/// validation, but redirects and in-page navigations can land where the
/// firewall would have refused) and fail closed. On success, write the
/// final URL/title back into the registry (audit 42 Medium-4) so
/// /browser/tabs never serves the stale requested URL and `verified`
/// reflects the landed URL's SSRF check — not a hardcoded true.
async fn finish_navigation(
    state: &SharedState,
    tab_id: &str,
    page: &std::sync::Arc<chromiumoxide::Page>,
    final_url: &str,
    title: &str,
) -> axum::response::Response {
    match crate::privy::validate_navigate_url(final_url, &[]).await {
        Ok(_) => {
            {
                let mut tabs = state.tabs.write().await;
                tabs.set_url_title(tab_id, final_url, title);
            }
            // Capture the verification baseline once navigation settles so
            // /browser/state's hash_match reflects reality (audit 41 MED-4).
            // Best-effort: a failed capture leaves baseline None (unknown),
            // which can never produce a false match.
            capture_baseline(state, tab_id, page).await;
            (
                StatusCode::OK,
                Json(json!({"title": title, "url": final_url, "verified": true})),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(
                tab_id,
                final_url,
                error = %e,
                "landed URL failed post-navigation SSRF re-check; failing closed"
            );
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": format!("landed URL blocked: {e}"),
                    "url": final_url,
                    "verified": false,
                })),
            )
                .into_response()
        }
    }
}

/// Capture a screenshot dHash as the tab's verification baseline (audit 41
/// MED-4). Shared by `navigate` and `tab_new`. Best-effort: any failure
/// leaves `baseline_hash` None (unknown) rather than a wrong value, and a
/// tab evicted mid-flight is logged, not crashed on.
pub(crate) async fn capture_baseline(
    state: &SharedState,
    tab_id: &str,
    page: &std::sync::Arc<chromiumoxide::Page>,
) {
    let png = match capture_png(page).await {
        Ok(png) => png,
        Err(e) => {
            tracing::warn!(tab_id, error = %e, "baseline capture failed; leaving None");
            return;
        }
    };
    // dHash decodes + resamples the full frame: blocking pool, never the
    // async runtime (audit 40 MED-6).
    let hash = tokio::task::spawn_blocking(move || crate::browser::verify::dhash(&png))
        .await
        .unwrap_or(0);
    if hash == 0 {
        // verify::dhash returns 0 for undecodable input; the verify
        // pipeline forbids 0 from ever matching (audit 41 MED-3).
        tracing::warn!(tab_id, "baseline hash undecodable; leaving None");
        return;
    }
    if !state.tabs.write().await.set_baseline(tab_id, hash) {
        tracing::warn!(tab_id, "tab evicted before baseline could be stored");
    }
}

/// Stubs (501) for endpoints whose handlers are not built yet. Each is
/// registered in main.rs and returns the standard `not_implemented` shape
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
