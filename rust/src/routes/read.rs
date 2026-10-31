//! Read-only browser route handlers.
//!
//! GET  /browser/text            page body text
//! GET  /browser/html            page markup
//! GET  /browser/snapshot        interactive accessibility snapshot (\[ref\] handles)
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
use crate::routes::advanced::base64_decode;
use crate::state::{SharedState, TabEntry};
use axum::{
    extract::{Json, Query, State},
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

/// 503 for "dependency down": the supervised browser is relaunching or
/// unreachable — retryable, not a bridge bug (audit 41 LOW-10 sweep).
fn service_unavailable(msg: impl Into<String>) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
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
///
/// Uses the vendored `screenshot_base64` and decodes in `spawn_blocking`:
/// Base64-decoding ~300KB is CPU work that does not belong on the async
/// executor (audit 42 follow-up; audit 40 MED-6).
pub(crate) async fn capture_png(page: &chromiumoxide::Page) -> anyhow::Result<Vec<u8>> {
    let params = chromiumoxide::page::ScreenshotParams::builder().build();
    let b64 = tokio::time::timeout(Duration::from_secs(30), page.screenshot_base64(params))
        .await
        .map_err(|_| anyhow::anyhow!("screenshot timed out"))??;
    let png = tokio::task::spawn_blocking(move || {
        base64_decode(&b64).map_err(|e| anyhow::anyhow!("screenshot base64: {e}"))
    })
    .await
    .map_err(|e| anyhow::anyhow!("screenshot decode task: {e}"))??;
    Ok(png)
}

/// Capture the viewport as JPEG bytes at the given quality (30s cap).
/// Chrome encodes the JPEG natively via CDP `Page.captureScreenshot`
/// (`format: jpeg`); no PNG round-trip. Used for the opt-in
/// `?format=jpeg` screenshot path (bandwidth-sensitive SoM/MCP
/// consumers). Default quality 80.
pub(crate) async fn capture_jpeg(
    page: &chromiumoxide::Page,
    quality: i64,
) -> anyhow::Result<Vec<u8>> {
    use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
    let params = chromiumoxide::page::ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Jpeg)
        .quality(quality)
        .build();
    let b64 = tokio::time::timeout(Duration::from_secs(30), page.screenshot_base64(params))
        .await
        .map_err(|_| anyhow::anyhow!("screenshot timed out"))??;
    // Same off-executor decode discipline as `capture_png`.
    let jpeg = tokio::task::spawn_blocking(move || {
        base64_decode(&b64).map_err(|e| anyhow::anyhow!("screenshot base64: {e}"))
    })
    .await
    .map_err(|e| anyhow::anyhow!("screenshot decode task: {e}"))??;
    Ok(jpeg)
}

/// 64-bit perceptual hash (dHash) of PNG bytes.
///
/// 64-bit perceptual hash (dHash) of PNG bytes.
///
/// Single source of truth: `browser::verify::dhash` (audit 41 MED-3). The
/// hand-rolled copy that used to live here (MSB-first bit order, Triangle
/// resample) produced different values than `verify::dhash` (LSB bit
/// order, Nearest resample), so baseline hashes captured from
/// `/browser/screenshot/hash` could never match the comparisons — the
/// documented regression-detection workflow was broken. A 0 return means
/// "undecodable"; the verify pipeline forbids 0 from ever matching, so it
/// surfaces as an error here instead of a hash.
fn dhash_png(png: &[u8]) -> anyhow::Result<u64> {
    let h = crate::browser::verify::dhash(png);
    if h == 0 {
        return Err(anyhow::anyhow!("decode png: undecodable screenshot"));
    }
    Ok(h)
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
        Err(e) => return bad_gateway(format!("no active page: {e}")),
    };
    // Single CDP round trip for url + title + text (was: page.url(),
    // get_title(), and innerText as three separate calls). The in-page
    // slice carries a 256-char lookahead margin and `.toWellFormed()`:
    // JS.slice() indexes UTF-16 code units and can split a surrogate pair
    // at the boundary; toWellFormed() replaces lone surrogates with U+FFFD
    // so CDP never ships broken UTF-16. Redaction runs in Rust over the
    // FULL lookahead buffer BEFORE the final truncation — a secret
    // straddling the 12k boundary is still caught (audit 42 High-5
    // follow-up: truncating first leaked the split half).
    const TEXT_LIMIT: usize = 12_000;
    const LOOKAHEAD: usize = 256;
    let js = format!(
        "(() => {{ const t = document.body ? document.body.innerText : ''; \
           return {{url: location.href, title: document.title, \
                   text: t.slice(0, {}).toWellFormed()}}; }})()",
        TEXT_LIMIT + LOOKAHEAD
    );
    let v = match eval_json(&page, &js).await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("evaluate text: {e}")),
    };
    let url = v
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or_default()
        .to_string();
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let raw = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
    // Audit 42 High-5: PII/secret redaction on the way out. READ-scope
    // callers must not receive raw credential/token values scraped from
    // the page. Runs over the lookahead buffer, then truncated to the
    // exact char limit.
    let (redacted, redactions) = crate::privy::redact(raw);
    let text: String = redacted.chars().take(TEXT_LIMIT).collect();
    (
        StatusCode::OK,
        Json(
            json!({"ok": true, "url": url, "title": title, "text": text, "redactions": redactions}),
        ),
    )
        .into_response()
}

/// GET /browser/html — document markup (truncated for sanity).
pub async fn html(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {e}")),
    };
    // Single CDP round trip (was: page.url() + outerHTML as two calls),
    // with the same 256-char lookahead + toWellFormed() + redact-before-
    // truncate discipline as `text()` above.
    const HTML_LIMIT: usize = 20_000;
    const LOOKAHEAD: usize = 256;
    let js = format!(
        "(() => {{ const h = document.documentElement.outerHTML; \
           return {{url: location.href, \
                   html: h.slice(0, {}).toWellFormed()}}; }})()",
        HTML_LIMIT + LOOKAHEAD
    );
    let v = match eval_json(&page, &js).await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("evaluate html: {e}")),
    };
    let url = v
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or_default()
        .to_string();
    let raw = v.get("html").and_then(|h| h.as_str()).unwrap_or("");
    // Audit 42 High-5: redact secrets from markup too (inline scripts and
    // hidden inputs often carry tokens). Runs over the lookahead buffer,
    // then truncated to the exact char limit.
    let (redacted, redactions) = crate::privy::redact(raw);
    let html: String = redacted.chars().take(HTML_LIMIT).collect();
    (
        StatusCode::OK,
        Json(json!({"ok": true, "url": url, "html": html, "redactions": redactions})),
    )
        .into_response()
}

/// GET /browser/snapshot — interactive elements with stable `[ref]` handles.
///
/// `?source=axtree` selects the opt-in CDP `Accessibility.getFullAXTree`
/// source instead of the default DOM walker. The walker stays the
/// default: getFullAXTree is unreliable on attached browsers (see the
/// walker-path comment), and the AXTree node order/shape differs. Both
/// sources emit the SAME response shape
/// (`{ref, role, name, backendNodeId}`) and register refs in the
/// per-tab `AxMapper`, so downstream consumers (`click_node`, MCP
/// tools) work unchanged.
#[derive(Deserialize)]
pub struct SnapshotQuery {
    #[serde(default)]
    pub source: Option<String>,
}

/// Minimal `Accessibility.getFullAXTree` CDP command — not present in
/// chromiumoxide_cdp 0.7.0's generated bindings, so it is defined here.
/// Used ONLY by the opt-in `?source=axtree` snapshot path.
#[derive(Debug, serde::Serialize)]
struct GetFullAXTree {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_depth: Option<i64>,
}

impl chromiumoxide::types::Method for GetFullAXTree {
    fn identifier(&self) -> chromiumoxide::types::MethodId {
        "Accessibility.getFullAXTree".into()
    }
}

impl chromiumoxide::types::Command for GetFullAXTree {
    type Response = FullAXTreeResponse;
}

#[derive(Debug, serde::Deserialize)]
struct FullAXTreeResponse {
    nodes: Vec<FullAXNode>,
}

#[derive(Debug, serde::Deserialize)]
struct FullAXNode {
    #[serde(rename = "backendDOMNodeId", default)]
    backend_dom_node_id: Option<i64>,
    #[serde(default)]
    ignored: bool,
    #[serde(default)]
    role: Option<FullAXValue>,
    #[serde(default)]
    name: Option<FullAXValue>,
}

#[derive(Debug, serde::Deserialize)]
struct FullAXValue {
    value: Option<serde_json::Value>,
}

/// AXTree-sourced snapshot (opt-in). Maps AX nodes onto the standard
/// `{ref, role, name, backendNodeId}` shape: ignored nodes and nodes
/// without a `backendDOMNodeId` are skipped (click_node needs the
/// backend id), refs are dense from 1 in tree order.
async fn snapshot_axtree(
    state: &SharedState,
    page: &Arc<chromiumoxide::Page>,
    url: &str,
    title: &str,
) -> Response {
    let cmd = GetFullAXTree { max_depth: None };
    let resp = match tokio::time::timeout(Duration::from_secs(15), page.execute(cmd)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return bad_gateway(format!("axtree snapshot failed: {e}")),
        Err(_) => return bad_gateway("axtree snapshot timed out"),
    };
    // Tab id for the per-tab ref mapper (same as the walker path).
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
        let mut seen = 0usize;
        for node in &resp.result.nodes {
            if node.ignored {
                continue;
            }
            let backend_id = match node.backend_dom_node_id {
                Some(id) => id,
                None => continue,
            };
            let role = node
                .role
                .as_ref()
                .and_then(|r| r.value.as_ref())
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = node
                .name
                .as_ref()
                .and_then(|n| n.value.as_ref())
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if seen >= AX_SNAPSHOT_CAP {
                continue;
            }
            seen += 1;
            let r = mapper.insert(backend_id);
            nodes_out.push(json!({
                "ref": r,
                "role": role,
                "name": name,
                "backendNodeId": backend_id,
            }));
        }
        truncated = resp.result.nodes.len() > seen;
    }
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "url": url,
            "title": title,
            "nodes": nodes_out,
            "truncated": truncated,
            "source": "axtree",
        })),
    )
        .into_response()
}

pub async fn snapshot(
    State(state): State<SharedState>,
    Query(q): Query<SnapshotQuery>,
) -> impl IntoResponse {
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
            // Audit 42 High-5: NEVER surface secret-classified field values.
            // The old code folded el.value into the name for every input —
            // passwords included — so credential values reached READ-scope
            // callers in the snapshot. Password and hidden inputs contribute
            // no value here at all.
            const t = (el.getAttribute('type') || '').toLowerCase();
            const secretField = tag === 'input' && (t === 'password' || t === 'hidden');
            const val = secretField ? '' : (el.value || '');
            const name = (el.getAttribute('aria-label') || el.innerText || val || '').trim().slice(0, 80);
            return [role, name];
        });
    })()"#;
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {e}")),
    };
    let (url, title) = page_url_title(&page).await;

    // Opt-in AXTree source (default stays the DOM walker).
    if q.source
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("axtree"))
    {
        return snapshot_axtree(&state, &page, &url, &title).await;
    }

    // Two-call snapshot: (1) JS walker returns [role, name] pairs in document
    // order; (2) find_elements returns Elements with backend_node_id in the
    // same document order. Zipping gives us ref/role/name/backendNodeId.
    // (CDP Accessibility.getFullAXTree is unreliable on attached browsers.)

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

/// GET /browser/screenshot — raw image bytes of the viewport (not JSON).
///
/// `?format=jpeg` opts into a JPEG-80 capture (Chrome-native encode,
/// ~5-10x smaller than PNG) for bandwidth-sensitive consumers; the
/// default stays PNG so existing pixel-pipeline consumers (compare,
/// delta, dHash baselines) are unaffected.
#[derive(Deserialize)]
pub struct ScreenshotQuery {
    #[serde(default)]
    pub format: Option<String>,
}

pub async fn screenshot(
    State(state): State<SharedState>,
    Query(q): Query<ScreenshotQuery>,
) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {e}")),
    };
    if q.format
        .as_deref()
        .is_some_and(|f| f.eq_ignore_ascii_case("jpeg"))
    {
        let jpeg = match capture_jpeg(&page, 80).await {
            Ok(j) => j,
            Err(e) => return bad_gateway(format!("screenshot: {e}")),
        };
        return (StatusCode::OK, [(header::CONTENT_TYPE, "image/jpeg")], jpeg).into_response();
    }
    let png = match capture_png(&page).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("screenshot: {e}")),
    };
    (StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], png).into_response()
}

/// GET /browser/screenshot/hash — perceptual hash of the viewport.
pub async fn screenshot_hash(State(state): State<SharedState>) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("no active page: {e}")),
    };
    let png = match capture_png(&page).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("screenshot: {e}")),
    };
    // dhash decodes + resamples the full frame: blocking pool, not the
    // async runtime (audit 40 MED-6).
    let hash = match tokio::task::spawn_blocking(move || dhash_png(&png)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return internal_error(format!("hash: {e}")),
        Err(e) => return internal_error(format!("hash task: {e}")),
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
        Err(e) => return bad_gateway(format!("no active page: {e}")),
    };
    let (url, title) = page_url_title(&page).await;
    let png = match capture_png(&page).await {
        Ok(p) => p,
        Err(e) => return bad_gateway(format!("state screenshot: {e}")),
    };
    // dhash decodes + resamples the full frame: blocking pool, not the
    // async runtime (audit 40 MED-6).
    let current_hash = match tokio::task::spawn_blocking(move || dhash_png(&png)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return bad_gateway(format!("state hash: {e}")),
        Err(e) => return bad_gateway(format!("state hash task: {e}")),
    };

    // Read-only compare against the stored baseline. The baseline is
    // captured when the tab is created / navigation settles (audit 41
    // MED-4); None means "not yet captured" and reports false — honest,
    // never a false match.
    let hash_match = {
        let tabs = state.tabs.read().await;
        tabs.active_ref()
            .and_then(|e| e.baseline_hash)
            .is_some_and(|b| b == current_hash)
    };

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "url": url,
            "title": title,
            "hash_match": hash_match,
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
    // 503: the browser (our dependency) is down/relaunching —
    // retryable, not a bridge bug (audit 41 LOW-10 sweep).
    let Some(browser) = browser else {
        return service_unavailable("browser not connected");
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
    let validated_url = match crate::privy::validated_url_or_400(&req.url).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let page = match tokio::time::timeout(
        Duration::from_secs(30),
        browser.new_page(validated_url.as_str()),
    )
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return bad_gateway(format!("new_page: {e}")),
        Err(_) => return bad_gateway("new_page timed out"),
    };
    // Defense in depth: literal-IP block patterns on the new target
    // (audit 42 High-1). Best-effort; the Rust firewall is the primary gate.
    crate::browser::actions::install_blocked_url_patterns(
        &page,
        &state.config.blocked_resource_patterns,
    )
    .await;
    let (url, title) = page_url_title(&page).await;
    // Re-run the SSRF firewall on the LANDED url (audit 42 High-1):
    // target creation navigates, and redirects can land where the
    // firewall would have refused. Fail closed: close the page and
    // refuse, never register a tab on a blocked landing.
    if let Err(e) = crate::privy::validate_navigate_url(&url, &[]).await {
        tracing::warn!(url, error = %e, "tab_new landed URL failed SSRF re-check; closing tab");
        let _ = page.close().await;
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": format!("landed URL blocked: {e}"), "verified": false})),
        )
            .into_response();
    }
    let id = uuid::Uuid::new_v4().to_string();
    let page = Arc::new(page);
    let evicted = {
        let mut tabs = state.tabs.write().await;
        tabs.insert(TabEntry {
            id: id.clone(),
            page: page.clone(),
            baseline_hash: None,
            url: url.clone(),
            title: title.clone(),
        })
    };
    // Capture the verification baseline once the tab settles (audit 41
    // MED-4). Best-effort; a failed capture leaves baseline None.
    crate::routes::browser::capture_baseline(&state, &id, &page).await;
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
        Err(e) => return bad_gateway(format!("no active page: {e}")),
    };
    let geo = match eval_json(&page, GEOMETRY_JS).await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("geometry evaluate: {e}")),
    };
    (StatusCode::OK, Json(json!({"ok": true, "geometry": geo}))).into_response()
}
