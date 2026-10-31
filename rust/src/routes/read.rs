//! Read-only browser route handlers.
//!
//! GET  /browser/text            page body text
//! GET  /browser/html            page markup
//! GET  /browser/snapshot        interactive accessibility snapshot ([ref] handles)
//! GET  /browser/screenshot      raw PNG of the viewport
//! GET  /browser/screenshot/hash perceptual hash of the viewport
//! GET  /browser/state           url / title / hash-vs-baseline / last action
//! GET  /browser/tabs            tab registry listing
//! POST /browser/tab/new         open a tab and register it
//! POST /browser/tab/switch      switch the active tab by id
//! POST /browser/geometry/refresh re-evaluate the cached window geometry
//!
//! Conventions: no unwraps on browser-touching paths; browser failures map
//! to 502/500 JSON `{"ok": false, "error": ...}`; slow CDP operations run
//! under `tokio::time::timeout`.

use crate::browser::actions::{active_page, close_page, eval_json};
use crate::browser::geometry::GEOMETRY_JS;
use crate::perception::{AxMapper, AX_SNAPSHOT_CAP};
use crate::state::{SharedState, TabEntry};
use axum::{
    extract::{Json, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn bad_gateway(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"ok": false, "error": msg.into()})),
    )
        .into_response()
}

fn internal_error(msg: impl Into<String>) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"ok": false, "error": msg.into()})),
    )
        .into_response()
}

/// Capture the viewport as PNG bytes (30s cap).
async fn capture_png(page: &chromiumoxide::Page) -> anyhow::Result<Vec<u8>> {
    let params = chromiumoxide::page::ScreenshotParams::builder().build();
    let png = tokio::time::timeout(Duration::from_secs(30), page.screenshot(params))
        .await
        .map_err(|_| anyhow::anyhow!("screenshot timed out"))??;
    Ok(png)
}

/// 64-bit perceptual hash (dHash) of PNG bytes.
///
/// Bit-identical to the previous implementation's algorithm: shrink to
/// 9x8 grayscale, then one bit per row comparing each pixel with its
/// right neighbor, MSB first. (img_hash 3.x pins image <0.24 while this
/// crate uses image 0.25, so its `Image` trait cannot be satisfied here
/// without a dependency change; the hand-rolled dHash keeps the exact
/// same bit layout instead.)
fn dhash_png(png: &[u8]) -> anyhow::Result<u64> {
    let img = image::load_from_memory(png).map_err(|e| anyhow::anyhow!("decode png: {}", e))?;
    let gray = img.to_luma8();
    let small = image::imageops::resize(&gray, 9, 8, image::imageops::FilterType::Triangle);
    let mut bits: u64 = 0;
    for y in 0..8 {
        for x in 0..8 {
            let left = small.get_pixel(x, y)[0];
            let right = small.get_pixel(x + 1, y)[0];
            bits = (bits << 1) | u64::from(left > right);
        }
    }
    Ok(bits)
}

pub async fn page_url_title(page: &chromiumoxide::Page) -> (String, String) {
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();
    (url, title)
}

/// GET /browser/text — document body text (truncated for sanity).
pub async fn text(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {}", e)),
    };
    let (url, title) = page_url_title(&page).await;
    let v = match eval_json(&page, "document.body ? document.body.innerText : ''").await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("evaluate text: {}", e)),
    };
    let text: String = v.as_str().unwrap_or("").chars().take(12000).collect();
    (
        StatusCode::OK,
        Json(json!({"ok": true, "url": url, "title": title, "text": text})),
    )
        .into_response()
}

/// GET /browser/html — document markup (truncated for sanity).
pub async fn html(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {}", e)),
    };
    let (url, _) = page_url_title(&page).await;
    let v = match eval_json(&page, "document.documentElement.outerHTML").await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("evaluate html: {}", e)),
    };
    let html: String = v.as_str().unwrap_or("").chars().take(20000).collect();
    (
        StatusCode::OK,
        Json(json!({"ok": true, "url": url, "html": html})),
    )
        .into_response()
}

/// GET /browser/snapshot — interactive elements with stable `[ref]` handles.
///
/// Uses CDP Accessibility.getFullAXTree (no pixels): each returned node
/// carries `ref` (dense from 1 in tree order), `role`, `name`, and the
/// `backendNodeId` that the click_node endpoint resolves. Refs are
/// re-mapped into the per-tab `AxMapper` on every snapshot so a stale
/// ref can never address a node from a previous document.
pub async fn snapshot(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {}", e)),
    };
    let (url, title) = page_url_title(&page).await;

    // Two-call snapshot: (1) JS walker returns [role, name] pairs in document
    // order; (2) find_elements returns Elements with backend_node_id in the
    // same document order. Zipping gives us ref/role/name/backendNodeId.
    // (CDP Accessibility.getFullAXTree is unreliable on attached browsers.)
    const SELECTOR: &str = "button, a[href], input, select, textarea, [role='button'], [role='link'], [role='textbox'], [role='searchbox'], [role='combobox'], [role='checkbox'], [role='radio'], [role='option'], [role='menuitem'], [role='tab'], [role='slider'], [role='spinbutton']";
    const WALKER_JS: &str = r#"(() => {
        const els = Array.from(document.querySelectorAll("button, a[href], input, select, textarea, [role='button'], [role='link'], [role='textbox'], [role='searchbox'], [role='combobox'], [role='checkbox'], [role='radio'], [role='option'], [role='menuitem'], [role='tab'], [role='slider'], [role='spinbutton']"));
        return els.slice(0, 200).map(el => {
            const tag = el.tagName.toLowerCase();
            const explicit = (el.getAttribute('role') || '').trim().toLowerCase().split(/\s+/)[0];
            let role = explicit || tag;
            if (tag === 'a') role = 'link';
            if (tag === 'button') role = 'button';
            if (tag === 'input') {
                const t = (el.getAttribute('type') || 'text').toLowerCase();
                role = t === 'checkbox' ? 'checkbox' : t === 'radio' ? 'radio' : t === 'range' ? 'slider' : 'textbox';
            }
            if (tag === 'textarea' || tag === 'select') role = 'textbox';
            const name = (el.getAttribute('aria-label') || el.innerText || el.value || '').trim().slice(0, 80);
            return [role, name];
        });
    })()"#;

    // Two-call snapshot: (1) JS walker returns [role, name] pairs in document
    // order; (2) find_elements returns Elements with backend_node_id in the
    // same document order. Zipping gives us ref/role/name/backendNodeId.
    // (CDP Accessibility.getFullAXTree is unreliable on attached browsers.)
    //
    // The two evaluations are NOT atomic: if the DOM mutates between them
    // (re-render, lazy hydration), meta[i] and elements[i] describe different
    // nodes and ref=N would bind to the wrong backendNodeId (audit 40 MED-5).
    // Retry on length mismatch; fail loudly instead of silently min-ing.
    let (meta, elements): (Vec<(String, String)>, Vec<chromiumoxide::Element>) = {
        let mut attempt = 0;
        loop {
            let meta: Vec<(String, String)> =
                match tokio::time::timeout(Duration::from_secs(15), eval_json(&page, WALKER_JS))
                    .await
                {
                    Ok(Ok(v)) => serde_json::from_value(v).unwrap_or_default(),
                    _ => Vec::new(),
                };
            let elements =
                match tokio::time::timeout(Duration::from_secs(15), page.find_elements(SELECTOR))
                    .await
                {
                    Ok(Ok(els)) => els,
                    _ => Vec::new(),
                };
            attempt += 1;
            if meta.len() == elements.len() || attempt >= 3 {
                break (meta, elements);
            }
            tracing::warn!(
                meta_len = meta.len(),
                el_len = elements.len(),
                attempt,
                "snapshot DOM mutated between evaluations; retrying"
            );
        }
    };
    if meta.len() != elements.len() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "snapshot DOM unstable: ref metadata and node handles misaligned after 3 attempts"})),
        )
            .into_response();
    }

    // Tab id for the per-tab ref mapper.
    let tab_id = {
        let mut tabs = state.tabs.write().await;
        tabs.active().map(|e| e.id.clone()).unwrap_or_default()
    };

    let mut nodes_out = Vec::new();
    let truncated: bool;
    {
        let mut mappers = state.ax_mappers.lock().await;
        let mapper = mappers.entry(tab_id).or_insert_with(AxMapper::new);
        mapper.clear();
        // meta.len() == elements.len() is guaranteed by the retry guard above.
        let n = meta.len().min(AX_SNAPSHOT_CAP);
        truncated = meta.len() > AX_SNAPSHOT_CAP;
        for i in 0..n {
            let (role, name) = &meta[i];
            let backend_id = elements[i].backend_node_id.inner();
            let r = mapper.insert(*backend_id);
            nodes_out.push(json!({
                "ref": r,
                "role": role,
                "name": name,
                "backendNodeId": backend_id,
            }));
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "url": url,
            "title": title,
            "nodes": nodes_out,
            "truncated": truncated,
        })),
    )
        .into_response()
}

/// GET /browser/screenshot — raw PNG bytes of the viewport (not JSON).
pub async fn screenshot(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {}", e)),
    };
    let png = match capture_png(&page).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("screenshot: {}", e)),
    };
    (StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], png).into_response()
}

/// GET /browser/screenshot/hash — perceptual hash of the viewport.
pub async fn screenshot_hash(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {}", e)),
    };
    let png = match capture_png(&page).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("screenshot: {}", e)),
    };
    // dhash decodes + resamples the full frame: blocking pool, not the
    // async runtime (audit 40 MED-6).
    let hash = match tokio::task::spawn_blocking(move || dhash_png(&png)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return internal_error(format!("hash: {}", e)),
        Err(e) => return internal_error(format!("hash task: {}", e)),
    };
    let (url, _) = page_url_title(&page).await;
    (
        StatusCode::OK,
        Json(json!({"ok": true, "hash": hash, "url": url})),
    )
        .into_response()
}

/// GET /browser/state — what the bridge thinks is happening.
///
/// Compares the current viewport hash against the tab's stored baseline.
/// READ-ONLY by the verify.rs review gate (audit 40 MED-7): a GET whose name
/// starts with "state" must never mutate the baseline. (The baseline is
/// captured at tab creation / navigation settle, not here, so consecutive
/// reads report real drift instead of forcing `hash_match: true`.)
pub async fn state(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {}", e)),
    };
    let (url, title) = page_url_title(&page).await;
    let png = match capture_png(&page).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("state screenshot: {}", e)),
    };
    // dhash decodes + resamples the full frame: blocking pool, not the
    // async runtime (audit 40 MED-6).
    let current_hash = match tokio::task::spawn_blocking(move || dhash_png(&png)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return bad_gateway(format!("state hash: {}", e)),
        Err(e) => return bad_gateway(format!("state hash task: {}", e)),
    };

    // Read-only compare against the stored baseline (None = not yet
    // captured, so the first reads report `hash_match: false`).
    let hash_match = {
        let tabs = state.tabs.read().await;
        tabs.active_ref()
            .and_then(|e| e.baseline_hash)
            .map(|b| b == current_hash)
            .unwrap_or(false)
    };

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "url": url,
            "title": title,
            "hash_match": hash_match,
            // Hud tracks the last completed action, but it is not wired
            // into BridgeState; report empty until the parent wires it.
            "last_action": "",
        })),
    )
        .into_response()
}

/// GET /browser/tabs — registry listing.
pub async fn tabs(State(state): State<SharedState>) -> impl IntoResponse {
    let tabs = state.tabs.read().await;
    (
        StatusCode::OK,
        Json(json!({"ok": true, "tabs": tabs.list()})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct TabNewReq {
    pub url: String,
}

/// POST /browser/tab/new — open a tab at `url`, register it, make it active.
pub async fn tab_new(
    State(state): State<SharedState>,
    Json(req): Json<TabNewReq>,
) -> impl IntoResponse {
    let browser = {
        let b = state.browser1.lock().await;
        b.browser()
    };
    let browser = match browser {
        Some(b) => b,
        None => return bad_gateway("browser not connected"),
    };
    if req.url.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "url is required"})),
        )
            .into_response();
    }
    // SSRF guard: same validation as /browser/navigate (audit 40 CRIT-1).
    // Blocks file://, loopback, link-local, private and CGNAT ranges.
    let validated_url = match crate::privy::validate_navigate_url(&req.url, &[]).await {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };
    let page = match tokio::time::timeout(
        Duration::from_secs(30),
        browser.new_page(validated_url.as_str()),
    )
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return bad_gateway(format!("new_page: {}", e)),
        Err(_) => return bad_gateway("new_page timed out"),
    };
    let (url, title) = page_url_title(&page).await;
    let id = uuid::Uuid::new_v4().to_string();
    let evicted = {
        let mut tabs = state.tabs.write().await;
        tabs.insert(TabEntry {
            id: id.clone(),
            page: Arc::new(page),
            baseline_hash: None,
            url: url.clone(),
            title: title.clone(),
        })
    };
    for p in evicted {
        close_page(&p).await;
    }
    (
        StatusCode::OK,
        Json(json!({"ok": true, "tab_id": id, "url": url, "title": title})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct TabSwitchReq {
    pub selector: String,
}

/// POST /browser/tab/switch — switch the active tab; `selector` is the tab id.
pub async fn tab_switch(
    State(state): State<SharedState>,
    Json(req): Json<TabSwitchReq>,
) -> impl IntoResponse {
    let (id, page) = {
        let mut tabs = state.tabs.write().await;
        match tabs.switch(req.selector.as_str()) {
            Some(entry) => (entry.id.clone(), entry.page.clone()),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"ok": false, "error": "unknown tab"})),
                )
                    .into_response()
            }
        }
    };
    // Best effort: bring the switched tab's target to the front.
    let _ = page.bring_to_front().await;
    (StatusCode::OK, Json(json!({"ok": true, "tab_id": id}))).into_response()
}

/// POST /browser/geometry/refresh — re-evaluate the window geometry script.
pub async fn geometry_refresh(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {}", e)),
    };
    let geo = match eval_json(&page, GEOMETRY_JS).await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("geometry evaluate: {}", e)),
    };
    (StatusCode::OK, Json(json!({"ok": true, "geometry": geo}))).into_response()
}
