//! Advanced browser route handlers.
//!
//! This module owns the higher-level browser operations: deterministic
//! element resolution without screenshots (`order`), scripted multi-step
//! batches (`script`), stubborn autocomplete widgets (`autocomplete`),
//! file uploads (`upload`), and VM-side visual comparison
//! (`assert_visual`, `screenshot_compare`, `screenshot_delta`,
//! `screenshot_region`).
//!
//! Conventions (shared with the other route modules):
//! - All input goes through [`crate::browser::actions`] (CDP-native real
//!   input; coordinates are CSS viewport pixels).
//! - Responses are `(StatusCode::OK, Json(...))` on success;
//!   request/operational failures are 4xx/5xx `{"ok":false,"error":...}`.
//!   Resolution misses inside `order`/`autocomplete`/`script` return
//!   `200` with `ok:false`, matching the documented per-endpoint contract.
//! - Slow operations are wrapped in `tokio::time::timeout`.
//! - Base64 is decoded/encoded by the small codec below: the `base64`
//!   crate is not a dependency, so no external codec is used.

use crate::browser::actions::{
    active_page, element_center, eval_json, insert_text, mouse_click, press_key,
};
use crate::interact::autocomplete::{exact_match, AUTOCOMPLETE_COMMIT_JS, AUTOCOMPLETE_JS};
use crate::interact::react::FILL_REACT_JS;
use crate::interact::upload::{validate_upload_path, UPLOAD_STAGE1_JS};
use crate::privy::validate_navigate_url;
use crate::routes::input::{visible_click, window_geometry};
use crate::state::SharedState;
use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use chromiumoxide::cdp::browser_protocol::dom::{
    GetDocumentParams, QuerySelectorParams, SetFileInputFilesParams,
};
use chromiumoxide::cdp::browser_protocol::page::{NavigateParams, Viewport};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------------

fn err(status: StatusCode, msg: impl Into<String>) -> axum::response::Response {
    (status, Json(json!({"ok": false, "error": msg.into()}))).into_response()
}

fn ok_json(value: serde_json::Value) -> axum::response::Response {
    (StatusCode::OK, Json(value)).into_response()
}

fn default_timeout_ms() -> u64 {
    10_000
}

fn default_key() -> String {
    "Enter".to_string()
}

/// Clamp a caller-supplied timeout to a sane range (0.5s .. 120s).
fn clamp_timeout(ms: u64) -> u64 {
    ms.clamp(500, 120_000)
}

// ---------------------------------------------------------------------------
// Minimal base64 codec (standard alphabet). The `base64` crate is not a
// dependency, so this file carries its own small implementation.
// ---------------------------------------------------------------------------

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !bytes.len().is_multiple_of(4) {
        return Err("bad base64: length is not a multiple of 4".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut i = 0;
    while i < bytes.len() {
        let last = i + 4 == bytes.len();
        let mut sext = [0u8; 4];
        let mut pad = 0usize;
        for j in 0..4 {
            let c = bytes[i + j];
            if c == b'=' {
                pad += 1;
            } else {
                if pad > 0 {
                    return Err(format!("bad base64: data after padding at {i}"));
                }
                sext[j] =
                    b64_val(c).ok_or_else(|| format!("bad base64: invalid char at {}", i + j))?;
            }
        }
        if pad > 2 {
            return Err("bad base64: too much padding".to_string());
        }
        if pad > 0 && !last {
            return Err("bad base64: padding before the final quantum".to_string());
        }
        out.push((sext[0] << 2) | (sext[1] >> 4));
        if pad < 2 {
            out.push(((sext[1] & 0x0f) << 4) | (sext[2] >> 2));
        }
        if pad < 1 {
            out.push(((sext[2] & 0x03) << 6) | sext[3]);
        }
        i += 4;
    }
    Ok(out)
}

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(B64_ALPHABET[(b0 >> 2) as usize] as char);
        out.push(B64_ALPHABET[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[(((b1 & 15) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[(b2 & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Screenshot capture
// ---------------------------------------------------------------------------

/// Capture a PNG screenshot, optionally clipped to a viewport rectangle
/// (CSS pixels). One capture pipeline: CDP `Page.captureScreenshot`.
async fn capture(page: &Page, clip: Option<Viewport>) -> anyhow::Result<Vec<u8>> {
    let mut builder = ScreenshotParams::builder();
    if let Some(vp) = clip {
        builder = builder.clip(vp);
    }
    let png = tokio::time::timeout(Duration::from_secs(30), page.screenshot(builder.build()))
        .await
        .map_err(|_| anyhow::anyhow!("screenshot timed out"))?
        .map_err(|e| anyhow::anyhow!("screenshot failed: {e}"))?;
    Ok(png)
}

fn clip_viewport(x: f64, y: f64, width: f64, height: f64) -> anyhow::Result<Viewport> {
    for (name, v) in [("x", x), ("y", y), ("width", width), ("height", height)] {
        if !v.is_finite() {
            anyhow::bail!("region {name} is not finite");
        }
    }
    if width <= 0.0 || height <= 0.0 {
        anyhow::bail!("region width/height must be positive");
    }
    Viewport::builder()
        .x(x)
        .y(y)
        .width(width)
        .height(height)
        .scale(1.0)
        .build()
        .map_err(|e| anyhow::anyhow!("clip viewport: {e}"))
}

// ---------------------------------------------------------------------------
// Pixel comparison
// ---------------------------------------------------------------------------

struct DiffResult {
    /// Fraction of pixels within tolerance: 1.0 = identical.
    score: f64,
    changed: u64,
    #[allow(dead_code)] // reported by a future diff endpoint; score+changed suffice today
    total: u64,
    /// Bounding box of changed pixels (x0, y0, x1, y1, inclusive-exclusive).
    bbox: Option<(u32, u32, u32, u32)>,
}

/// Compare two PNGs pixel-wise. The reference is resized to the current
/// frame when sizes differ. A pixel counts as changed when its maximum
/// per-channel absolute difference exceeds `threshold`.
#[allow(clippy::cast_precision_loss)] // u64 pixel counts -> f64 ratio; sub-pixel precision irrelevant
#[allow(clippy::many_single_char_names)] // pixel loops: x/y/w/h/a/b are conventional
fn diff_images(
    current_png: &[u8],
    reference_png: &[u8],
    threshold: u8,
) -> anyhow::Result<DiffResult> {
    let cur = image::load_from_memory(current_png)
        .map_err(|e| anyhow::anyhow!("cannot decode current screenshot: {e}"))?
        .to_rgb8();
    let mut rf = image::load_from_memory(reference_png)
        .map_err(|e| anyhow::anyhow!("cannot decode reference image: {e}"))?
        .to_rgb8();
    if rf.dimensions() != cur.dimensions() {
        rf = image::imageops::resize(
            &rf,
            cur.width(),
            cur.height(),
            image::imageops::FilterType::Triangle,
        );
    }
    let (w, h) = cur.dimensions();
    let total = u64::from(w) * u64::from(h);
    let mut changed = 0u64;
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for y in 0..h {
        for x in 0..w {
            let a = cur.get_pixel(x, y);
            let b = rf.get_pixel(x, y);
            let d = a[0]
                .abs_diff(b[0])
                .max(a[1].abs_diff(b[1]))
                .max(a[2].abs_diff(b[2]));
            if d > threshold {
                changed += 1;
                bbox = Some(match bbox {
                    None => (x, y, x + 1, y + 1),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
                });
            }
        }
    }
    let score = if total == 0 {
        1.0
    } else {
        1.0 - changed as f64 / total as f64
    };
    Ok(DiffResult {
        score,
        changed,
        total,
        bbox,
    })
}

fn png_bytes(img: &image::RgbImage) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgb8(img.clone())
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| anyhow::anyhow!("PNG encode failed: {e}"))?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// 1. order — deterministic semantic resolution, no screenshots
// ---------------------------------------------------------------------------

/// Deterministic in-page resolver, mirroring the parity strategy:
/// ARIA role + accessible name first, then case-insensitive text match,
/// then label/placeholder (fill only), then the title attribute.
/// Returns the winning element's center `{x, y}` or null.
/// The most specific (shortest-text) visible match wins, so containers
/// like \<body\> never shadow the real control.
const ORDER_RESOLVE_JS: &str = r#"(text, role, isFill) => {
    const vis = (el) => {
        if (!el || !el.isConnected) return false;
        const r = el.getBoundingClientRect();
        if (r.width <= 0 || r.height <= 0) return false;
        const s = window.getComputedStyle(el);
        return s.visibility !== 'hidden' && s.display !== 'none'
            && parseFloat(s.opacity || '1') > 0;
    };
    const center = (el) => {
        const r = el.getBoundingClientRect();
        return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
    };
    const low = (s) => (s || '').toLowerCase();
    const nameOf = (el) =>
        (el.getAttribute('aria-label') || '').trim()
        || ((el.innerText || '').trim().split('\n')[0] || '')
        || (el.getAttribute('value') || '')
        || (el.textContent || '').trim();
    const t = low((text || '').trim());
    if (!t) return null;
    const all = Array.from(document.querySelectorAll('*'));
    const skip = new Set(['HTML', 'HEAD', 'SCRIPT', 'STYLE', 'NOSCRIPT', 'TITLE', 'META', 'LINK']);
    // 1. ARIA role + accessible name (exact, then substring).
    if (role) {
        const els = Array.from(
            document.querySelectorAll('[role="' + CSS.escape(role) + '"]'));
        for (const pass of [0, 1]) {
            for (const el of els) {
                const n = low(nameOf(el).trim());
                if (!n) continue;
                if ((pass === 0 && n === t) || (pass === 1 && n.includes(t))) {
                    if (vis(el)) return center(el);
                }
            }
        }
    }
    // 2. Case-insensitive text match; shortest visible text wins.
    let best = null, bestLen = Infinity;
    for (const el of all) {
        if (skip.has(el.tagName)) continue;
        const raw = (el.innerText || '').trim();
        if (!raw || !low(raw).includes(t)) continue;
        if (!vis(el)) continue;
        if (raw.length < bestLen) { best = el; bestLen = raw.length; }
    }
    if (best) return center(best);
    // 3. Label / placeholder association (fill only).
    if (isFill) {
        for (const el of all) {
            const lbl = el.getAttribute('aria-label') || el.getAttribute('placeholder') || '';
            if (lbl && low(lbl).includes(t) && vis(el)) return center(el);
        }
        for (const lab of document.querySelectorAll('label')) {
            if (low(lab.innerText || '').includes(t)) {
                const forId = lab.getAttribute('for');
                const ctl = forId ? document.getElementById(forId)
                                  : lab.querySelector('input, textarea, select');
                if (ctl && vis(ctl)) return center(ctl);
            }
        }
    }
    // 4. Title attribute fallback.
    for (const el of all) {
        const ti = el.getAttribute('title') || '';
        if (ti && low(ti).includes(t) && vis(el)) return center(el);
    }
    return null;
}"#;

/// Shell for the MutationObserver-driven order resolver. `__ORDER_FIND__` is
/// spliced with [`ORDER_RESOLVE_JS`] at runtime, so the finder's four passes
/// stay byte-identical and the existing unit test keeps covering them.
///
/// Takes `(text, role, isFill, timeoutMs)`; returns a Promise resolving to
/// `{x, y}` or `null`. The observer re-runs the finder when the DOM
/// mutates, debounced so scans never happen more often than the old 150ms
/// poll cadence (a continuously-mutating page costs no more scans than
/// before); the in-page timer enforces the timeout.
const ORDER_PROMISE_SHELL: &str = r"((text, role, isFill, timeoutMs) => new Promise((resolve) => {
    const find = __ORDER_FIND__;
    let done = false;
    let obs = null;
    let deferTimer = null;
    let limitTimer = null;
    const finish = (v) => {
        if (done) return;
        done = true;
        try { if (obs) obs.disconnect(); } catch (e) {}
        if (deferTimer !== null) { clearTimeout(deferTimer); deferTimer = null; }
        if (limitTimer !== null) { clearTimeout(limitTimer); limitTimer = null; }
        resolve(v);
    };
    const h0 = find(text, role, isFill);
    if (h0) { finish(h0); return; }
    let lastScan = Date.now();
    limitTimer = setTimeout(() => finish(null), timeoutMs);
    obs = new MutationObserver(() => {
        if (done) return;
        const now = Date.now();
        const wait = 150 - (now - lastScan);
        if (wait <= 0) {
            lastScan = now;
            const h = find(text, role, isFill);
            if (h) finish(h);
        } else if (deferTimer === null) {
            deferTimer = setTimeout(() => {
                deferTimer = null;
                if (done) return;
                lastScan = Date.now();
                const h2 = find(text, role, isFill);
                if (h2) finish(h2);
            }, wait);
        }
    });
    try {
        obs.observe(document.documentElement || document,
            { childList: true, subtree: true, attributes: true, characterData: true });
    } catch (e) { /* observe failed: limitTimer still enforces the timeout */ }
}))";

/// The promise shell with the 4-pass finder spliced in. Built at runtime so
/// the finder text has a single source of truth.
fn order_resolve_promise_js() -> String {
    ORDER_PROMISE_SHELL.replacen("__ORDER_FIND__", ORDER_RESOLVE_JS, 1)
}

/// Evaluate the order-resolver promise, awaiting it via CDP `awaitPromise`.
///
/// The JS resolves with `{x, y}` when the target appears, or `null` when its
/// internal timer (the remaining order budget) expires. The Rust-side timeout
/// is a backstop only, for a hung CDP call.
async fn eval_order_promise(
    page: &Page,
    js: &str,
    timeout_ms: u64,
) -> anyhow::Result<serde_json::Value> {
    use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
    let params = EvaluateParams::builder()
        .expression(js)
        .await_promise(true)
        .build()
        .map_err(|e| anyhow::anyhow!("order eval params: {e}"))?;
    let result = tokio::time::timeout(
        Duration::from_millis(timeout_ms.saturating_add(5_000)),
        page.evaluate(params),
    )
    .await
    .map_err(|_| anyhow::anyhow!("order resolve timed out"))??;
    let v: serde_json::Value = result
        .into_value()
        .map_err(|e| anyhow::anyhow!("deserialize order result: {e}"))?;
    Ok(v)
}

/// Resolve the order target, waiting efficiently for it to appear.
///
/// The 4-pass matcher is unchanged; instead of re-running the full DOM scan
/// every 150ms from Rust, a single CDP evaluation awaits a promise that a
/// `MutationObserver` resolves the moment a match appears. The in-page timer
/// enforces the clamped timeout, so the miss path still returns `None`
/// exactly at the deadline and the hit path returns the same `(x, y)`.
/// A transient CDP failure (e.g. a navigation destroying the execution
/// context mid-wait) retries with a fresh observer until the deadline,
/// exactly like the old poll loop did.
async fn resolve_order_center(
    page: &Page,
    text: &str,
    role: &str,
    is_fill: bool,
    timeout_ms: u64,
) -> Option<(f64, f64)> {
    let timeout = clamp_timeout(timeout_ms);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout);
    let shell = order_resolve_promise_js();
    let text_js = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    let role_js = serde_json::to_string(role).unwrap_or_else(|_| "\"\"".to_string());
    let fill_js = if is_fill { "true" } else { "false" };
    loop {
        let remaining_ms = u64::try_from(
            deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        if remaining_ms == 0 {
            return None;
        }
        // Rebuild per attempt with the remaining budget so a retry after a
        // transient failure can never overrun the original deadline.
        let js = format!("({shell})({text_js},{role_js},{fill_js},{remaining_ms})");
        // A transient CDP failure (navigation tore down the context, target
        // crashed, ...) falls through and retries until the deadline.
        if let Ok(v) = eval_order_promise(page, &js, remaining_ms).await {
            if v.is_null() {
                return None;
            }
            if let (Some(x), Some(y)) = (
                v.get("x").and_then(serde_json::Value::as_f64),
                v.get("y").and_then(serde_json::Value::as_f64),
            ) {
                return Some((x, y));
            }
            // Non-null but malformed: keep waiting until the deadline.
        }
    }
}

/// Apply the native-prototype value setter to the focused element.
async fn fill_focused(page: &Page, value: &str) -> anyhow::Result<()> {
    let js = String::from(
        "(() => { const el = document.activeElement; if (!el) return null; const set = ",
    ) + FILL_REACT_JS
        + "; return set(el, "
        + &serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
        + "); })()";
    let v = eval_json(page, &js).await?;
    if v.is_null() {
        anyhow::bail!("no focused element to fill");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct OrderRequest {
    pub action: String, // click | fill
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub value: String, // text to fill (for fill action)
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Accepted for contract compatibility. When true, the click is driven
    /// through the visible X11/XTest cursor (same hardened path as
    /// /browser/click visible=true); otherwise input is CDP-native.
    #[serde(default)]
    pub visible: bool,
}

/// POST /browser/order — click or fill by visible text / ARIA role.
///
/// Resolves the target deterministically in-page (no screenshot
/// round-trip), then dispatches real input at its center: CDP-native by
/// default, or the visible X11 cursor when `visible=true` (audit 42
/// Medium — the flag was accepted but silently ignored).
pub async fn order(
    State(state): State<SharedState>,
    Json(req): Json<OrderRequest>,
) -> impl IntoResponse {
    let action = req.action.trim().to_lowercase();
    if action != "click" && action != "fill" {
        return err(
            StatusCode::BAD_REQUEST,
            format!("unknown action '{}'", req.action),
        );
    }
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("no active page: {e}")),
    };
    let target = resolve_order_center(
        &page,
        &req.text,
        &req.role,
        action == "fill",
        req.timeout_ms,
    )
    .await;
    let Some((x, y)) = target else {
        return ok_json(json!({"ok": false, "error": format!("could not resolve '{}'", req.text)}));
    };
    // Lease generation at admission: a hand-over mid-flight aborts the
    // visible input before any XTest event (audit 42 High-4).
    let gen0 = state.lease_gen.load(std::sync::atomic::Ordering::SeqCst);
    // Dispatch one real click at the resolved center, natively or via the
    // visible cursor.
    let click_at = |visible: bool| {
        let state = &state;
        let page = &page;
        async move {
            if visible {
                let geom = window_geometry(page).await?;
                tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    visible_click(state, x, y, &geom, gen0),
                )
                .await
                .map_err(|_| anyhow::anyhow!("visible click timed out"))??;
                Ok::<_, anyhow::Error>("visible")
            } else {
                mouse_click(page, x, y).await?;
                Ok("native")
            }
        }
    };
    if action == "click" {
        let mode = match click_at(req.visible).await {
            Ok(m) => m,
            Err(e) => return err(StatusCode::BAD_GATEWAY, format!("click failed: {e}")),
        };
        ok_json(json!({"ok": true, "at": {"x": x.round(), "y": y.round()}, "mode": mode}))
    } else {
        // Focus with a real click so frameworks move focus correctly,
        // then set the value through the native prototype setter.
        let mode = match click_at(req.visible).await {
            Ok(m) => m,
            Err(e) => return err(StatusCode::BAD_GATEWAY, format!("focus click failed: {e}")),
        };
        if let Err(e) = fill_focused(&page, &req.value).await {
            return err(StatusCode::BAD_GATEWAY, format!("fill failed: {e}"));
        }
        ok_json(json!({"ok": true, "mode": mode}))
    }
}

// ---------------------------------------------------------------------------
// 2. autocomplete — stubborn-widget autocomplete sequence
// ---------------------------------------------------------------------------

/// Center of the visible `[role="option"]` at `idx` (index into the
/// visible-option list produced by `AUTOCOMPLETE_JS`), or null.
const OPTION_CENTER_JS: &str = r#"(idx) => {
    const listbox = document.querySelector('[role="listbox"]') || document;
    const opts = Array.from(listbox.querySelectorAll('[role="option"]'))
        .filter((o) => {
            const r = o.getBoundingClientRect();
            return r.width > 0 && r.height > 0;
        });
    const el = opts[idx];
    if (!el) return null;
    el.scrollIntoView({ block: 'nearest' });
    const r = el.getBoundingClientRect();
    return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
}"#;

#[derive(Debug, Deserialize)]
struct AutocompleteOption {
    index: usize,
    text: String,
}

#[derive(Debug, Deserialize)]
pub struct AutocompleteRequest {
    pub selector: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub option_text: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

/// POST /browser/autocomplete — drive a stubborn autocomplete widget.
///
/// Sequence (wired from `crate::interact::autocomplete`): real click to
/// focus, React-safe clear, `insert_text`, Enter to trigger the backend
/// query, wait for `[role="option"]` items, click the case-insensitive
/// EXACT text match (no substring, no first-option fallback), then commit
/// with a neutral outside-click + blur.
pub async fn autocomplete(
    State(state): State<SharedState>,
    Json(req): Json<AutocompleteRequest>,
) -> impl IntoResponse {
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("no active page: {e}")),
    };
    // 1. Real click to focus.
    let (x, y) = match element_center(&page, &req.selector).await {
        Ok(pt) => pt,
        Err(e) => {
            return err(
                StatusCode::NOT_FOUND,
                format!("autocomplete target not found: {e}"),
            )
        }
    };
    if let Err(e) = mouse_click(&page, x, y).await {
        return err(StatusCode::BAD_GATEWAY, format!("focus click failed: {e}"));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 2. React-safe clear of any existing text.
    if let Err(e) = fill_focused(&page, "").await {
        return err(StatusCode::BAD_GATEWAY, format!("clear failed: {e}"));
    }
    // 3. Type the query.
    if let Err(e) = insert_text(&page, &req.text).await {
        return err(StatusCode::BAD_GATEWAY, format!("type failed: {e}"));
    }
    // 4. Enter triggers the backend query on enterprise widgets.
    if let Err(e) = press_key(&page, "Enter").await {
        return err(StatusCode::BAD_GATEWAY, format!("enter failed: {e}"));
    }
    // 5. Wait for suggestion options to render.
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(clamp_timeout(req.timeout_ms));
    let mut options: Vec<AutocompleteOption> = Vec::new();
    loop {
        if let Ok(v) = eval_json(&page, AUTOCOMPLETE_JS).await {
            if let Ok(list) = serde_json::from_value::<Vec<AutocompleteOption>>(v) {
                if !list.is_empty() {
                    options = list;
                    break;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if options.is_empty() {
        return ok_json(json!({
            "ok": false,
            "error": "no-options",
            "detail": "suggestion list did not appear",
        }));
    }
    // 6. Case-insensitive EXACT match on the Rust side (never in the page).
    let texts: Vec<&str> = options.iter().map(|o| o.text.as_str()).collect();
    let matched = exact_match(&req.option_text, &texts);
    let winner = match matched {
        Some(text) => match options.iter().find(|o| o.text == text) {
            Some(o) => o,
            None => {
                return ok_json(json!({"ok": false, "error": "internal option mismatch"}));
            }
        },
        None => {
            return ok_json(json!({
                "ok": false,
                "error": format!("no exact option match for '{}'", req.option_text),
                "options": texts,
            }));
        }
    };
    // 7. Real click on the winning option.
    let center_js = format!("({})({})", OPTION_CENTER_JS, winner.index);
    let (ox, oy) = match eval_json(&page, &center_js).await {
        Ok(v) => match (
            v.get("x").and_then(serde_json::Value::as_f64),
            v.get("y").and_then(serde_json::Value::as_f64),
        ) {
            (Some(ox), Some(oy)) => (ox, oy),
            _ => return ok_json(json!({"ok": false, "error": "option has no box"})),
        },
        Err(e) => {
            return err(
                StatusCode::BAD_GATEWAY,
                format!("option lookup failed: {e}"),
            )
        }
    };
    if let Err(e) = mouse_click(&page, ox, oy).await {
        return err(StatusCode::BAD_GATEWAY, format!("option click failed: {e}"));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    // 8. Commit: neutral outside-click + blur so the selection finalizes.
    let commit_js = String::from("(() => { const commit = ")
        + AUTOCOMPLETE_COMMIT_JS
        + "; return commit(document.querySelector("
        + &serde_json::to_string(&req.selector).unwrap_or_else(|_| "null".to_string())
        + ")); })()";
    if let Err(e) = eval_json(&page, &commit_js).await {
        return err(StatusCode::BAD_GATEWAY, format!("commit failed: {e}"));
    }
    ok_json(json!({"ok": true, "selected": winner.text}))
}

// ---------------------------------------------------------------------------
// 3. script — sequential batch of input primitives in one call
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ScriptStep {
    pub action: String, // click | fill | type | press | wait | eval | navigate
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub text: String,
    #[serde(default = "default_key")]
    pub key: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub js: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct ScriptRequest {
    pub steps: Vec<ScriptStep>,
}

/// Run one script step against the active page.
async fn run_script_step(page: &Page, step: &ScriptStep) -> anyhow::Result<serde_json::Value> {
    let action = step.action.trim().to_lowercase();
    match action.as_str() {
        "click" => {
            let (x, y) = element_center(page, &step.selector).await?;
            mouse_click(page, x, y).await?;
            Ok(json!({"ok": true, "action": "click", "at": {"x": x.round(), "y": y.round()}}))
        }
        "fill" => {
            let (x, y) = element_center(page, &step.selector).await?;
            mouse_click(page, x, y).await?;
            fill_focused(page, &step.text).await?;
            Ok(json!({"ok": true, "action": "fill"}))
        }
        "type" => {
            let (x, y) = element_center(page, &step.selector).await?;
            mouse_click(page, x, y).await?;
            insert_text(page, &step.text).await?;
            Ok(json!({"ok": true, "action": "type"}))
        }
        "press" => {
            if !step.selector.is_empty() {
                let (x, y) = element_center(page, &step.selector).await?;
                mouse_click(page, x, y).await?;
            }
            press_key(page, &step.key).await?;
            Ok(json!({"ok": true, "action": "press", "key": step.key}))
        }
        "wait" => {
            if step.selector.is_empty() {
                tokio::time::sleep(Duration::from_millis(clamp_timeout(step.timeout_ms))).await;
            } else {
                let deadline = tokio::time::Instant::now()
                    + Duration::from_millis(clamp_timeout(step.timeout_ms));
                loop {
                    if element_center(page, &step.selector).await.is_ok() {
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        anyhow::bail!("wait timed out for selector '{}'", step.selector);
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            }
            Ok(json!({"ok": true, "action": "wait"}))
        }
        "eval" => {
            let result = eval_json(page, &step.js).await?;
            Ok(json!({"ok": true, "action": "eval", "result": result}))
        }
        "navigate" => {
            let url = validate_navigate_url(&step.url, &[])
                .await
                .map_err(|e| anyhow::anyhow!("bad url: {e}"))?;
            let params = NavigateParams::builder()
                .url(url.as_str())
                .build()
                .map_err(|e: String| anyhow::anyhow!("navigate params: {e}"))?;
            tokio::time::timeout(Duration::from_secs(30), page.goto(params))
                .await
                .map_err(|_| anyhow::anyhow!("navigate timed out"))?
                .map_err(|e| anyhow::anyhow!("navigate failed: {e}"))?;
            let current = page
                .url()
                .await
                .map_err(|e| anyhow::anyhow!("url read failed: {e}"))?
                .unwrap_or_default();
            Ok(json!({"ok": true, "action": "navigate", "url": current}))
        }
        other => anyhow::bail!("unknown action '{other}'"),
    }
}

/// POST /browser/script — execute steps sequentially, stopping at the
/// first error. Returns `{"ok":true,"results":[...]}` or
/// `{"ok":false,"error":...,"step":N}`.
pub async fn script(
    State(state): State<SharedState>,
    Json(req): Json<ScriptRequest>,
) -> impl IntoResponse {
    if req.steps.len() > 100 {
        return err(StatusCode::BAD_REQUEST, "too many steps (max 100)");
    }
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("no active page: {e}")),
    };
    let mut results = Vec::with_capacity(req.steps.len());
    for (i, step) in req.steps.iter().enumerate() {
        match tokio::time::timeout(Duration::from_secs(60), run_script_step(&page, step)).await {
            Ok(Ok(r)) => results.push(r),
            Ok(Err(e)) => {
                return ok_json(json!({"ok": false, "error": e.to_string(), "step": i}));
            }
            Err(_) => {
                return ok_json(json!({"ok": false, "error": "step timed out", "step": i}));
            }
        }
    }
    ok_json(json!({"ok": true, "results": results}))
}

// ---------------------------------------------------------------------------
// 4-6. Visual comparison
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AssertVisualRequest {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub reference_b64: String,
    /// Similarity 0..1; the assertion fails below this. Default 0.97.
    #[serde(default = "default_similarity_threshold")]
    pub threshold: f64,
}

fn default_similarity_threshold() -> f64 {
    0.97
}

/// POST /`browser/assert_visual` — clip a region, compare against the
/// reference PNG, and report a similarity score in \[0,1\].
/// Never mutates any baseline: this is a read-only assertion.
#[allow(clippy::cast_precision_loss)] // u64 pixel counts -> f64 ratio; sub-pixel precision irrelevant
pub async fn assert_visual(
    State(state): State<SharedState>,
    Json(req): Json<AssertVisualRequest>,
) -> impl IntoResponse {
    // Decode + full-frame pixel comparison are CPU-bound: run them on the
    // blocking pool, never the async runtime (audit 41 MED-5 — the sibling
    // endpoints were fixed for the same class in audit 40 MED-6).
    enum VisualErr {
        Capture(String),
        Reference(String),
    }
    let reference = match base64_decode(&req.reference_b64) {
        Ok(b) => b,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("bad base64 reference: {e}"),
            )
        }
    };
    let clip = match clip_viewport(req.x, req.y, req.width, req.height) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.to_string()),
    };
    // Similarity threshold is documented 0..1; reject NaN/infinity and
    // clamp (proactive-3: an unclamped NaN made every assertion fail
    // silently via `score >= NaN == false`).
    let threshold = if req.threshold.is_finite() {
        req.threshold.clamp(0.0, 1.0)
    } else {
        return err(
            StatusCode::BAD_REQUEST,
            "threshold must be a finite number in [0, 1]",
        );
    };
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("no active page: {e}")),
    };
    let png = match capture(&page, Some(clip)).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    let score = match tokio::task::spawn_blocking(move || -> Result<f64, VisualErr> {
        let cur_gray = image::load_from_memory(&png)
            .map_err(|e| VisualErr::Capture(e.to_string()))?
            .to_luma8();
        let mut ref_gray = image::load_from_memory(&reference)
            .map_err(|e| VisualErr::Reference(e.to_string()))?
            .to_luma8();
        if ref_gray.dimensions() != cur_gray.dimensions() {
            ref_gray = image::imageops::resize(
                &ref_gray,
                cur_gray.width(),
                cur_gray.height(),
                image::imageops::FilterType::Triangle,
            );
        }
        // Grayscale comparison with a small per-pixel tolerance absorbs
        // antialiasing jitter; the score is the fraction of pixels within it.
        let (w, h) = cur_gray.dimensions();
        let total = u64::from(w) * u64::from(h);
        let mut within = 0u64;
        for y in 0..h {
            for x in 0..w {
                if cur_gray.get_pixel(x, y)[0].abs_diff(ref_gray.get_pixel(x, y)[0]) <= 12 {
                    within += 1;
                }
            }
        }
        Ok(if total == 0 {
            1.0
        } else {
            within as f64 / total as f64
        })
    })
    .await
    {
        Ok(Ok(score)) => score,
        Ok(Err(VisualErr::Capture(e))) => {
            return err(StatusCode::BAD_GATEWAY, format!("decode capture: {e}"))
        }
        Ok(Err(VisualErr::Reference(e))) => {
            return err(StatusCode::BAD_REQUEST, format!("decode reference: {e}"))
        }
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("visual compare task failed: {e}"),
            )
        }
    };
    ok_json(json!({
        "ok": true,
        "match": score >= threshold,
        "score": score,
    }))
}

#[derive(Debug, Deserialize)]
pub struct CompareRequest {
    pub reference_b64: String,
    /// Per-channel difference threshold (0-255). Default 10.
    #[serde(default = "default_diff_threshold")]
    pub threshold: i64,
}

fn default_diff_threshold() -> i64 {
    10
}

/// POST /browser/screenshot/compare — full-page screenshot vs a reference
/// PNG, VM-side. Returns match verdict, similarity score, changed-pixel
/// stats, and the current perceptual hash.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // threshold clamped to [0,255] above, so the i64->u8 cast is exact
pub async fn screenshot_compare(
    State(state): State<SharedState>,
    Json(req): Json<CompareRequest>,
) -> impl IntoResponse {
    let reference = match base64_decode(&req.reference_b64) {
        Ok(b) => b,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("bad base64 reference: {e}"),
            )
        }
    };
    let threshold: u8 = req.threshold.clamp(0, 255) as u8;
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("no active page: {e}")),
    };
    let png = match capture(&page, None).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    // CPU-bound pixel work runs on the blocking pool, never the async
    // runtime (audit 40 MED-6). Clone the frame: `png` is still needed
    // below for the perceptual hash.
    let png_for_diff = png.clone();
    let diff = match tokio::task::spawn_blocking(move || {
        diff_images(&png_for_diff, &reference, threshold)
    })
    .await
    {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return err(StatusCode::BAD_REQUEST, e.to_string()),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("diff task: {e}")),
    };
    // dhash decodes the frame too: blocking pool as well.
    let hash = tokio::task::spawn_blocking(move || crate::browser::verify::dhash(&png))
        .await
        .unwrap_or(0);
    // A <1% changed-pixel budget absorbs caret blinks and antialiasing;
    // anything beyond that counts as a real visual change.
    let is_match = diff.score >= 0.99;
    ok_json(json!({
        "ok": true,
        "match": is_match,
        "score": diff.score,
        "diff_pct": (1.0 - diff.score) * 100.0,
        "changed_pixels": diff.changed,
        "hash": hash,
    }))
}

#[derive(Debug, Deserialize)]
pub struct DeltaRequest {
    #[serde(default)]
    pub reference_b64: String,
    #[serde(default = "default_diff_threshold")]
    pub threshold: i64,
    /// Accepted for contract compatibility; server-side frame caching is
    /// not implemented in this revision, so the reference must be sent
    /// with each call.
    #[serde(default)]
    pub session_id: String,
    /// Accepted for contract compatibility; patches are PNG (flat UI
    /// colors compress better as PNG than JPEG), so quality is unused.
    #[serde(default = "default_quality")]
    pub quality: u8,
}

fn default_quality() -> u8 {
    80
}

/// POST /browser/screenshot/delta — diff the current frame against the
/// reference and return the changed region as a base64 PNG patch
/// (empty string when nothing changed).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // threshold clamped to [0,255] above, so the i64->u8 cast is exact
pub async fn screenshot_delta(
    State(state): State<SharedState>,
    Json(req): Json<DeltaRequest>,
) -> impl IntoResponse {
    // CPU-bound pixel work (diff + patch decode/crop) runs on the blocking
    // pool, never the async runtime (audit 40 MED-6; proactive-1: the patch
    // decode/crop used to run on the handler thread after the diff task).
    enum DeltaErr {
        Diff(String),
        Capture(String),
    }
    let _ = req.session_id; // contract field; see struct docs
    let _ = req.quality; // contract field; patches are PNG, see struct docs
    if req.reference_b64.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "reference_b64 is required");
    }
    let reference = match base64_decode(&req.reference_b64) {
        Ok(b) => b,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("bad base64 reference: {e}"),
            )
        }
    };
    let threshold: u8 = req.threshold.clamp(0, 255) as u8;
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("no active page: {e}")),
    };
    let png = match capture(&page, None).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    let (diff, patch_b64) =
        match tokio::task::spawn_blocking(move || -> Result<(DiffResult, String), DeltaErr> {
            let diff = diff_images(&png, &reference, threshold)
                .map_err(|e| DeltaErr::Diff(e.to_string()))?;
            let patch_b64 = match diff.bbox {
                None => String::new(),
                Some((x0, y0, x1, y1)) => {
                    // Pad slightly so patch edges composite cleanly.
                    let cur = image::load_from_memory(&png)
                        .map_err(|e| DeltaErr::Capture(format!("decode capture: {e}")))?
                        .to_rgb8();
                    let (w, h) = cur.dimensions();
                    let px0 = x0.saturating_sub(4);
                    let py0 = y0.saturating_sub(4);
                    let px1 = (x1 + 4).min(w);
                    let py1 = (y1 + 4).min(h);
                    if px1 <= px0 || py1 <= py0 {
                        String::new()
                    } else {
                        let patch = image::imageops::crop_imm(&cur, px0, py0, px1 - px0, py1 - py0)
                            .to_image();
                        png_bytes(&patch)
                            .map(|b| base64_encode(&b))
                            .map_err(|e| DeltaErr::Capture(e.to_string()))?
                    }
                }
            };
            Ok((diff, patch_b64))
        })
        .await
        {
            Ok(Ok((diff, patch_b64))) => (diff, patch_b64),
            Ok(Err(DeltaErr::Diff(e))) => return err(StatusCode::BAD_REQUEST, e),
            Ok(Err(DeltaErr::Capture(e))) => return err(StatusCode::BAD_GATEWAY, e),
            Err(e) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("delta task: {e}"),
                )
            }
        };
    ok_json(json!({
        "ok": true,
        "changed": diff.bbox.is_some(),
        "patch_b64": patch_b64,
    }))
}

// ---------------------------------------------------------------------------
// 7. screenshot_region — raw PNG of a clipped region
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RegionRequest {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// POST /browser/screenshot/region — clipped screenshot returned as raw
/// PNG bytes (`image/png`), for targeted verification without shipping
/// full frames.
pub async fn screenshot_region(
    State(state): State<SharedState>,
    Json(req): Json<RegionRequest>,
) -> impl IntoResponse {
    let clip = match clip_viewport(req.x, req.y, req.width, req.height) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("no active page: {e}")),
    };
    let png = match capture(&page, Some(clip)).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    match axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .body(Body::from(png))
    {
        Ok(resp) => resp.into_response(),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("response build failed: {e}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// 8. upload — CDP file-input upload, no OS dialog
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UploadRequest {
    /// CSS selector for the `<input type="file">`. When empty, the first
    /// file input in the document is used.
    #[serde(default)]
    pub selector: String,
    /// Local path of the file to attach. Must exist, be a regular file,
    /// and live under the staging allowlist (`config.upload_dirs`).
    pub path: String,
}

/// POST /browser/upload — attach a local file to a file input via
/// `DOM.setFileInputFiles`. No OS-level file chooser is ever opened.
///
/// The staging allowlist is `config.upload_dirs` (created at startup in
/// main.rs) — the SAME knob the MCP `upload` tool validates against.
/// (Audit 41 MED-6: this route used to hardcode `~/jobs/uploads` while MCP
/// read the config, a split-brain allowlist.)
#[allow(clippy::too_many_lines)] // upload is linear validate->stage->verify steps; splitting would obscure the flow
#[allow(clippy::many_single_char_names)] // coordinate/dimension bindings follow convention
pub async fn upload(
    State(state): State<SharedState>,
    Json(req): Json<UploadRequest>,
) -> impl IntoResponse {
    // 1. The path must exist and be a regular file.
    let Ok(meta) = std::fs::metadata(&req.path) else {
        return err(
            StatusCode::BAD_REQUEST,
            format!("upload path not accessible: {}", req.path),
        );
    };
    if !meta.is_file() {
        return err(
            StatusCode::BAD_REQUEST,
            format!("upload path is not a file: {}", req.path),
        );
    }
    // 2. Canonicalize and enforce the staging allowlist. This is the
    // early check; the path is re-validated immediately before Chrome
    // reads it (step 4) to close the TOCTOU window (audit 42 Medium).
    let allowlist = state.config.upload_dirs.clone();
    if let Err(e) = validate_upload_path(&req.path, &allowlist) {
        return err(StatusCode::BAD_REQUEST, e.to_string());
    }
    let page = match active_page(&state).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("no active page: {e}")),
    };
    // 3. Resolve the file input's node id.
    let doc = match page.execute(GetDocumentParams::builder().build()).await {
        Ok(d) => d,
        Err(e) => {
            return err(
                StatusCode::BAD_GATEWAY,
                format!("DOM.getDocument failed: {e}"),
            )
        }
    };
    let query = if req.selector.trim().is_empty() {
        "input[type=file]".to_string()
    } else {
        req.selector.clone()
    };
    let qs_params = match QuerySelectorParams::builder()
        .node_id(doc.root.node_id)
        .selector(query.as_str())
        .build()
    {
        Ok(p) => p,
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("querySelector params: {e}"),
            )
        }
    };
    let found = match page.execute(qs_params).await {
        Ok(q) => q,
        Err(e) => {
            return err(
                StatusCode::BAD_GATEWAY,
                format!("DOM.querySelector failed: {e}"),
            )
        }
    };
    if *found.node_id.inner() == 0 {
        // No match through the DOM API: run the stage-1 probe for a
        // better diagnostic (it also sees inputs hidden under styled
        // buttons and one level into shadow roots).
        let detail = match eval_json(&page, UPLOAD_STAGE1_JS).await {
            Ok(v) => {
                if v.get("found")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    let hidden = v
                        .get("hidden")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    format!(
                        "a file input exists{} but the selector did not match it",
                        if hidden {
                            " (hidden under a styled button)"
                        } else {
                            ""
                        }
                    )
                } else {
                    "no <input type=file> in the document".to_string()
                }
            }
            Err(_) => "no file input matched".to_string(),
        };
        return err(
            StatusCode::NOT_FOUND,
            format!("upload target not found: {detail}"),
        );
    }
    // 4. Attach the file. The browser dispatches native input/change
    //    events; the OS dialog never opens.
    //    Re-validate the path IMMEDIATELY before handing it to Chrome
    //    (audit 42 Medium — upload TOCTOU): the allowlist check at step 2
    //    and this use are separated by slow DOM round-trips, so a
    //    symlink/file swap in between would otherwise redirect the
    //    upload. The re-validated canonical path is what Chrome reads.
    let canonical = match validate_upload_path(&req.path, &allowlist) {
        Ok(p) => p,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("upload path re-validation failed: {e}"),
            )
        }
    };
    let set_params = match SetFileInputFilesParams::builder()
        .files(vec![canonical.to_string_lossy().into_owned()])
        .node_id(found.node_id)
        .build()
    {
        Ok(p) => p,
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("setFileInputFiles params: {e}"),
            )
        }
    };
    if let Err(e) = tokio::time::timeout(Duration::from_secs(30), page.execute(set_params))
        .await
        .map_err(|_| anyhow::anyhow!("setFileInputFiles timed out"))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!("DOM.setFileInputFiles failed: {e}")))
    {
        return err(StatusCode::BAD_GATEWAY, e.to_string());
    }
    ok_json(json!({"ok": true}))
}

// ---------------------------------------------------------------------------
// Unit tests (pure helpers only)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #![allow(clippy::many_single_char_names)] // test pixel helpers: w/h/r/g/b are conventional
    use super::*;

    #[test]
    fn b64_roundtrip() {
        for data in [
            vec![],
            b"f".to_vec(),
            b"fo".to_vec(),
            b"foo".to_vec(),
            b"foob".to_vec(),
            b"fooba".to_vec(),
            b"foobar".to_vec(),
            vec![0, 255, 128, 1, 254],
        ] {
            let enc = base64_encode(&data);
            assert_eq!(base64_decode(&enc).unwrap(), data, "roundtrip {enc}");
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn b64_rejects_bad_input() {
        assert!(base64_decode("Zm9v").is_ok());
        assert!(base64_decode("Zm9").is_err()); // bad length
        assert!(base64_decode("Zm!v").is_err()); // bad char
        assert!(base64_decode("Zm=v").is_err()); // data after padding
        assert!(base64_decode("====").is_err()); // too much padding
        assert!(base64_decode("Zm9v\nYmFy").is_ok()); // whitespace tolerated
    }

    #[test]
    fn clip_viewport_rejects_bad_regions() {
        assert!(clip_viewport(0.0, 0.0, 100.0, 100.0).is_ok());
        assert!(clip_viewport(0.0, 0.0, 0.0, 100.0).is_err());
        assert!(clip_viewport(0.0, 0.0, -5.0, 100.0).is_err());
        assert!(clip_viewport(f64::NAN, 0.0, 100.0, 100.0).is_err());
    }

    /// Encode a solid-color RGB PNG for diff tests.
    fn solid_png(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([r, g, b]));
        png_bytes(&img).unwrap()
    }

    #[test]
    fn diff_identical_scores_one() {
        let a = solid_png(8, 8, 200, 200, 200);
        let d = diff_images(&a, &a, 10).unwrap();
        assert!((d.score - 1.0).abs() < f64::EPSILON);
        assert_eq!(d.changed, 0);
        assert!(d.bbox.is_none());
    }

    #[test]
    fn diff_finds_changed_bbox() {
        let a = solid_png(8, 8, 200, 200, 200);
        let mut img = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 200, 200]));
        img.put_pixel(2, 3, image::Rgb([0, 0, 0]));
        let b = png_bytes(&img).unwrap();
        let d = diff_images(&a, &b, 10).unwrap();
        assert_eq!(d.changed, 1);
        assert_eq!(d.bbox, Some((2, 3, 3, 4)));
        assert!(d.score < 1.0 && d.score > 0.9);
    }

    #[test]
    fn diff_resizes_mismatched_reference() {
        let a = solid_png(8, 8, 200, 200, 200);
        let b = solid_png(4, 4, 200, 200, 200);
        let d = diff_images(&a, &b, 10).unwrap();
        assert!((d.score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn diff_rejects_undecodable() {
        let a = solid_png(8, 8, 200, 200, 200);
        assert!(diff_images(&a, b"not png", 10).is_err());
    }

    #[test]
    fn order_resolve_js_has_all_four_stages() {
        for token in [
            "CSS.escape",
            "aria-label",
            "placeholder",
            "getAttribute('title')",
            "getBoundingClientRect",
            "querySelectorAll('label')",
        ] {
            assert!(ORDER_RESOLVE_JS.contains(token), "missing {token}");
        }
    }

    #[test]
    fn order_promise_shell_splices_finder() {
        let js = order_resolve_promise_js();
        assert!(!js.contains("__ORDER_FIND__"), "placeholder not spliced");
        // The finder's four passes must be present verbatim.
        for token in [
            "CSS.escape",
            "aria-label",
            "getAttribute('title')",
            "getBoundingClientRect",
        ] {
            assert!(js.contains(token), "missing {token}");
        }
        // The efficient-wait machinery must be present.
        for token in [
            "MutationObserver",
            "new Promise",
            "setTimeout",
            "disconnect",
        ] {
            assert!(js.contains(token), "missing {token}");
        }
    }
}
