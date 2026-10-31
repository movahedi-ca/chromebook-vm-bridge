//! Pool route handlers: isolated context pool on the main browser's pages
//! (a separate headless browser was abandoned; see pool/mod.rs).
//!
//! Locking discipline: pool-state mutation happens under
//! `state.pool.lock().await` and the lock is never held across slow CDP
//! calls. Handlers open pages BEFORE taking the lock and close evicted or
//! released pages AFTER dropping it; only map bookkeeping (prune, LRU
//! evict, insert) happens under the lock.

use crate::browser::actions::{
    close_page, element_center, eval_json, insert_text, mouse_click, navigate_with_mode,
    NavWaitMode,
};
use crate::pool::PoolState;
use crate::state::SharedState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chromiumoxide::Page;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

// ---------- request models ----------

#[derive(Deserialize)]
pub struct AcquireReq {
    #[serde(default = "default_ttl")]
    pub ttl_s: u64,
    #[serde(default)]
    pub trace: bool,
}

fn default_ttl() -> u64 {
    600
}

#[derive(Deserialize)]
pub struct ContextIdReq {
    pub context_id: String,
}

#[derive(Deserialize)]
pub struct NavigateReq {
    pub context_id: String,
    pub url: String,
    /// Lifecycle wait mode: "networkidle" (default — historical
    /// `page.goto()` behavior), "load", "domcontentloaded", "commit".
    #[serde(default = "default_wait_until")]
    pub wait_until: String,
}

/// Default `wait_until` for pooled navigation: "networkidle" preserves
/// the historical `page.goto()` behavior exactly.
fn default_wait_until() -> String {
    "networkidle".to_string()
}

#[derive(Deserialize)]
pub struct ClickReq {
    pub context_id: String,
    pub selector: String,
}

#[derive(Deserialize)]
pub struct FillReq {
    pub context_id: String,
    pub selector: String,
    pub text: String,
}

#[derive(Deserialize)]
pub struct EvalReq {
    pub context_id: String,
    pub js: String,
}

#[derive(Deserialize)]
pub struct OrderReq {
    pub context_id: String,
    pub action: String,
    pub text: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Deserialize)]
pub struct TextQuery {
    pub context_id: String,
}

// ---------- helpers ----------

fn err500(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"ok": false, "error": e.to_string()})),
    )
        .into_response()
}

/// 503 for "dependency down": the supervised browser is relaunching or
/// unreachable — retryable, not a bridge bug (audit 41 LOW-10). Keeps
/// 5xx dashboards and retry policies from conflating this with 500s.
fn err503(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"ok": false, "error": e.to_string()})),
    )
        .into_response()
}

/// Fetch a pool page, touching it for LRU/TTL. The pool lock is released
/// before this returns; drive the page outside the lock.
#[allow(clippy::result_large_err)] // axum Response is large by design; not worth boxing
async fn pool_page(state: &SharedState, id: &str) -> Result<Arc<Page>, Response> {
    let mut pool = state.pool.lock().await;
    match pool.get(id) {
        Some(page) => Ok(page),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": format!("unknown context: {}", id)})),
        )
            .into_response()),
    }
}

/// Best-effort disposal of a pool browser context (audit 42 High-3).
/// A missing id or a dead browser just logs; the context dies with the
/// browser anyway. Disposal is what wipes the context's cookies,
/// localStorage, and cache — closing the page alone does not.
async fn dispose_pool_context(browser: &chromiumoxide::Browser, ctx_id: Option<String>) {
    let Some(id) = ctx_id else {
        return;
    };
    let ctx = chromiumoxide::cdp::browser_protocol::browser::BrowserContextId::new(id);
    if let Err(e) = browser.dispose_browser_context(ctx).await {
        tracing::debug!(error = %e, "pool browser-context dispose failed (best-effort)");
    }
}

// ---------- handlers ----------

/// POST /pool/acquire {`ttl_s`, trace} -> {"`ok":true,"context_id`"}
pub async fn acquire(
    State(state): State<SharedState>,
    Json(req): Json<AcquireReq>,
) -> impl IntoResponse {
    // Get the browser directly from browser1 (like tab_new does).
    // ensure_browser() can return a broken reconnect; the stored browser works.
    let browser = {
        let b = state.browser1.lock().await;
        match b.browser() {
            Some(br) => br,
            None => return err503("browser not connected"),
        }
    };
    // Disposable browser context per acquire (audit 42 High-3): the pool
    // page runs in its own cookie jar / storage / cache — NEVER in the
    // default context alongside the signed-in profile. Disposed on
    // release/eviction. CDP round-trips happen BEFORE taking the pool
    // lock: the lock is never held across them (audit 40 MED-4).
    let ctx_id = match browser
        .create_browser_context(
            chromiumoxide::cdp::browser_protocol::target::CreateBrowserContextParams::default(),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => return err503(format!("pool create_browser_context: {e}")),
    };
    let mut target_params =
        chromiumoxide::cdp::browser_protocol::target::CreateTargetParams::new("about:blank");
    target_params.browser_context_id = Some(ctx_id.clone());
    let page = match browser.new_page(target_params).await {
        Ok(p) => Arc::new(p),
        Err(e) => {
            // Don't leak the context when the page fails to open.
            let _ = browser.dispose_browser_context(ctx_id).await;
            return err503(format!("pool new_page: {e}"));
        }
    };
    // Literal-IP block patterns on the new target (audit 42 High-1).
    crate::browser::actions::install_blocked_url_patterns(
        &page,
        &state.config.blocked_resource_patterns,
    )
    .await;
    let (id, to_close) = {
        let mut pool = state.pool.lock().await;
        let to_close = pool.prepare_acquire();
        let id = pool.register(page, ctx_id.inner().clone(), req.ttl_s, req.trace);
        (id, to_close)
    };
    for (p, c) in to_close {
        close_page(&p).await;
        dispose_pool_context(&browser, c).await;
    }
    Json(json!({"ok": true, "context_id": id})).into_response()
}

/// POST /pool/release {`context_id`} -> {"ok":true}
pub async fn release(
    State(state): State<SharedState>,
    Json(req): Json<ContextIdReq>,
) -> impl IntoResponse {
    let browser = {
        let b = state.browser1.lock().await;
        b.browser()
    };
    let released = {
        let mut pool = state.pool.lock().await;
        pool.release(&req.context_id)
    };
    match released {
        Some((p, ctx)) => {
            close_page(&p).await;
            // Dispose the browser context so its cookies, storage, and
            // cache die with it (audit 42 High-3).
            if let Some(b) = browser {
                dispose_pool_context(&b, ctx).await;
            }
            Json(json!({"ok": true})).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": format!("unknown context: {}", req.context_id)})),
        )
            .into_response(),
    }
}

/// POST /pool/navigate {`context_id`, url, `wait_until`} -> {"ok":true,"url"}
pub async fn navigate(
    State(state): State<SharedState>,
    Json(req): Json<NavigateReq>,
) -> impl IntoResponse {
    let page = match pool_page(&state, &req.context_id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    // SSRF guard: same validation as /browser/navigate (audit 40 HIGH-1).
    let validated_url = match crate::privy::validated_url_or_400(&req.url).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let nav =
        // `wait_until` is honored (was accepted-but-ignored; `page.goto`
        // was fire-and-forget). Navigation failures now surface instead
        // of silently continuing.
        navigate_with_mode(
            &page,
            validated_url.as_str(),
            NavWaitMode::parse(&req.wait_until),
            Duration::from_secs(30),
        )
        .await;
    match nav {
        Ok(()) => {
            let landed = page.url().await.ok().flatten().unwrap_or_default();
            // Re-run the SSRF firewall on the LANDED url (audit 42 High-1).
            // Fail closed: never report success on a blocked landing.
            match crate::privy::validate_navigate_url(&landed, &[]).await {
                Ok(_) => Json(json!({"ok": true, "url": landed, "verified": true})).into_response(),
                Err(e) => {
                    tracing::warn!(context_id = %req.context_id, landed, error = %e, "pool landed URL failed SSRF re-check");
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"ok": false, "error": format!("landed URL blocked: {e}"), "url": landed, "verified": false})),
                    )
                        .into_response()
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("timeout") {
                err500("navigate timed out after 30s")
            } else {
                err500(format!("navigate failed: {e}"))
            }
        }
    }
}

/// POST /pool/click {`context_id`, selector} -> {"ok":true}
/// Real CDP click at the element's center (getBoundingClientRect +
/// Input.dispatchMouseEvent), the same input path as the headed browser.
pub async fn click(
    State(state): State<SharedState>,
    Json(req): Json<ClickReq>,
) -> impl IntoResponse {
    let page = match pool_page(&state, &req.context_id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (x, y) = match element_center(&page, &req.selector).await {
        Ok(c) => c,
        Err(e) => return err500(format!("pool click: {e}")),
    };
    match mouse_click(&page, x, y, state.click_timing()).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => err500(format!("pool click: {e}")),
    }
}

/// POST /pool/fill {`context_id`, selector, text} -> {"ok":true,"value"}
/// Focus + set value via evaluate (fires input/change), then read back.
pub async fn fill(State(state): State<SharedState>, Json(req): Json<FillReq>) -> impl IntoResponse {
    let page = match pool_page(&state, &req.context_id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let sel = serde_json::to_string(&req.selector).unwrap_or_default();
    let text = serde_json::to_string(&req.text).unwrap_or_default();
    let js = format!(
        r"(() => {{
            const el = document.querySelector({sel});
            if (!el) return null;
            el.focus();
            el.value = {text};
            el.dispatchEvent(new Event('input', {{bubbles: true}}));
            el.dispatchEvent(new Event('change', {{bubbles: true}}));
            return el.value === undefined ? '' : String(el.value);
        }})()"
    );
    match eval_json(&page, &js).await {
        Ok(v) if !v.is_null() => {
            Json(json!({"ok": true, "value": v.as_str().unwrap_or_default()})).into_response()
        }
        Ok(_) => err500(format!(
            "pool fill: no element matches selector: {}",
            req.selector
        )),
        Err(e) => err500(format!("pool fill: {e}")),
    }
}

/// POST /pool/eval {`context_id`, js} -> {"ok":true,"result"}
/// Same 200KB result cap as /browser/eval (audit 41 LOW-9).
pub async fn eval(State(state): State<SharedState>, Json(req): Json<EvalReq>) -> impl IntoResponse {
    let page = match pool_page(&state, &req.context_id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    match crate::routes::input::eval_capped(&page, &req.js).await {
        Ok(result) => Json(json!({"ok": true, "result": result})).into_response(),
        Err(e) => err500(format!("pool eval: {e}")),
    }
}

/// Deterministic order resolver, same cascade as the headed browser:
/// role+name, then visible text, then label/placeholder (fill), then title.
/// Returns the target's viewport center, or None when nothing resolves.
async fn resolve_order_center(
    page: &Page,
    action: &str,
    text: &str,
    role: &str,
) -> anyhow::Result<Option<(f64, f64)>> {
    let action_js = serde_json::to_string(action).unwrap_or_default();
    let text_js = serde_json::to_string(text).unwrap_or_default();
    let role_js = serde_json::to_string(role).unwrap_or_default();
    let js = format!(
        r#"((action, text, role) => {{
            const norm = s => (s == null ? '' : String(s)).trim().toLowerCase().replace(/\s+/g, ' ');
            const q = norm(text);
            if (!q) return null;
            const visible = el => {{
                const r = el.getBoundingClientRect();
                return r.width > 0 && r.height > 0 &&
                    getComputedStyle(el).visibility !== 'hidden' && !el.disabled;
            }};
            const nameOf = el => norm(el.getAttribute('aria-label')) || norm(el.innerText) ||
                norm(el.value) || norm(el.getAttribute('placeholder')) ||
                norm(el.getAttribute('title')) || norm(el.getAttribute('alt'));
            const center = el => {{
                const r = el.getBoundingClientRect();
                return {{x: r.x + r.width / 2, y: r.y + r.height / 2}};
            }};
            const roleSel = {{
                button: 'button,[role="button"]', link: 'a,[role="link"]',
                textbox: 'input,textarea,[role="textbox"]',
                checkbox: 'input[type="checkbox"],[role="checkbox"]',
                radio: 'input[type="radio"],[role="radio"]',
                combobox: 'select,[role="combobox"]', tab: '[role="tab"]',
                menuitem: '[role="menuitem"]', switch: '[role="switch"]'
            }};
            let cands = [];
            if (role) {{
                const sel = roleSel[role.toLowerCase()] || '[role="' + role.replace(/"/g, '') + '"]';
                cands = [...document.querySelectorAll(sel)]
                    .filter(e => visible(e) && nameOf(e).includes(q));
            }}
            if (!cands.length) {{
                cands = [...document.querySelectorAll('a,button,input,select,textarea,[role]')]
                    .filter(e => visible(e) && nameOf(e).includes(q));
            }}
            if (!cands.length && action === 'fill') {{
                cands = [...document.querySelectorAll('input,textarea,select,[contenteditable]')]
                    .filter(e => {{
                        if (!visible(e)) return false;
                        let lab = null;
                        if (e.id) lab = document.querySelector('label[for="' + e.id.replace(/"/g, '') + '"]');
                        return norm(e.getAttribute('aria-label')) === q ||
                            norm(e.getAttribute('placeholder')) === q ||
                            (lab && norm(lab.innerText) === q);
                    }});
            }}
            if (!cands.length) {{
                cands = [...document.querySelectorAll('[title]')]
                    .filter(e => visible(e) && norm(e.getAttribute('title')).includes(q));
            }}
            if (!cands.length) return null;
            return center(cands[0]);
        }})({action_js}, {text_js}, {role_js})"#
    );
    let v = eval_json(page, &js).await?;
    if v.is_null() {
        return Ok(None);
    }
    let x = v
        .get("x")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("order resolve: bad center x"))?;
    let y = v
        .get("y")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("order resolve: bad center y"))?;
    Ok(Some((x, y)))
}

/// POST /pool/order {`context_id`, action, text, role, value} -> {"ok":true}
/// Semantic order in a pool context: deterministic resolution, headless
/// execution. Unknown actions and unresolvable targets are 200s with
/// ok:false, matching the parity endpoint's contract.
pub async fn order(
    State(state): State<SharedState>,
    Json(req): Json<OrderReq>,
) -> impl IntoResponse {
    let page = match pool_page(&state, &req.context_id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if req.action != "click" && req.action != "fill" {
        return Json(json!({"ok": false, "error": format!("unknown action {}", req.action)}))
            .into_response();
    }
    let target = match resolve_order_center(&page, &req.action, &req.text, &req.role).await {
        Ok(t) => t,
        Err(e) => return err500(format!("pool order: {e}")),
    };
    let Some((x, y)) = target else {
        return Json(json!({"ok": false, "error": format!("could not resolve '{}'", req.text)}))
            .into_response();
    };
    if req.action == "click" {
        match mouse_click(&page, x, y, state.click_timing()).await {
            Ok(()) => Json(json!({"ok": true})).into_response(),
            Err(e) => err500(format!("pool order click: {e}")),
        }
    } else {
        // fill: real click to focus, select-all on the focused field, then
        // CDP text insertion so keystroke-driven widgets react.
        if let Err(e) = mouse_click(&page, x, y, state.click_timing()).await {
            return err500(format!("pool order fill focus: {e}"));
        }
        let select_js = r"(() => {
            const el = document.activeElement;
            if (!el || !('value' in el)) return false;
            if (el.select) el.select();
            return true;
        })()";
        if let Err(e) = eval_json(&page, select_js).await {
            return err500(format!("pool order fill select: {e}"));
        }
        if let Err(e) = insert_text(&page, &req.value).await {
            return err500(format!("pool order fill: {e}"));
        }
        Json(json!({"ok": true})).into_response()
    }
}

/// POST /pool/snapshot {`context_id`} -> {"ok":true,"snapshot":[{role,name}]}
/// Compact interactive-element list from a DOM walker.
pub async fn snapshot(
    State(state): State<SharedState>,
    Json(req): Json<ContextIdReq>,
) -> impl IntoResponse {
    let page = match pool_page(&state, &req.context_id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let js = r#"(() => {
        const out = [];
        const seen = new Set();
        const roleOf = el => el.getAttribute('role') || ({
            A: 'link', BUTTON: 'button', SELECT: 'combobox', TEXTAREA: 'textbox',
            INPUT: el.type === 'checkbox' ? 'checkbox' :
                   el.type === 'radio' ? 'radio' :
                   (el.type === 'submit' || el.type === 'button') ? 'button' : 'textbox'
        }[el.tagName] || el.tagName.toLowerCase());
        const nameOf = el => ((el.getAttribute('aria-label') || el.innerText || el.value ||
            el.getAttribute('placeholder') || el.getAttribute('title') ||
            el.getAttribute('alt') || '').trim().replace(/\s+/g, ' ').slice(0, 120));
        const sel = 'a,button,input,select,textarea,' +
            '[role="button"],[role="link"],[role="checkbox"],[role="radio"],' +
            '[role="switch"],[role="tab"],[role="menuitem"],[role="combobox"]';
        for (const el of document.querySelectorAll(sel)) {
            const r = el.getBoundingClientRect();
            if (r.width === 0 && r.height === 0) continue;
            if (el.disabled) continue;
            const name = nameOf(el);
            if (!name) continue;
            const role = roleOf(el);
            const key = role + '|' + name;
            if (seen.has(key)) continue;
            seen.add(key);
            out.push({role, name});
            if (out.length >= 200) break;
        }
        return out;
    })()"#;
    match eval_json(&page, js).await {
        Ok(snap) => Json(json!({"ok": true, "snapshot": snap})).into_response(),
        Err(e) => err500(format!("pool snapshot: {e}")),
    }
}

/// GET /`pool/text?context_id`= -> {"ok":true,"text"}
pub async fn text(
    State(state): State<SharedState>,
    Query(q): Query<TextQuery>,
) -> impl IntoResponse {
    let page = match pool_page(&state, &q.context_id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    match eval_json(
        &page,
        "() => document.body ? document.body.innerText.slice(0, 12000) : ''",
    )
    .await
    {
        Ok(v) => Json(json!({"ok": true, "text": v.as_str().unwrap_or_default()})).into_response(),
        Err(e) => err500(format!("pool text: {e}")),
    }
}

/// GET /pool/status -> {"ok":true,"contexts":N,"max":3,"browser":bool}
pub async fn status(State(state): State<SharedState>) -> impl IntoResponse {
    let browser = {
        let b = state.browser1.lock().await;
        b.browser()
    };
    let (pruned, contexts, max, browser_up) = {
        let mut pool = state.pool.lock().await;
        let pruned = pool.prune();
        (
            pruned,
            pool.context_count(),
            pool.max_contexts(),
            PoolState::is_browser_connected(),
        )
    };
    for (p, c) in pruned {
        close_page(&p).await;
        // TTL-expired contexts are disposed too: their cookie jars must
        // not outlive them (audit 42 High-3).
        if let Some(b) = browser.as_ref() {
            dispose_pool_context(b, c).await;
        }
    }
    Json(json!({
        "ok": true,
        "contexts": contexts,
        "max": max,
        "browser": browser_up,
    }))
    .into_response()
}

/// POST /pool/speculate -> {"ok":true,"contexts":N}
/// Lightweight pool health probe; no parity target, kept trivial.
pub async fn speculate(State(state): State<SharedState>) -> impl IntoResponse {
    let pool = state.pool.lock().await;
    Json(json!({"ok": true, "contexts": pool.context_count()})).into_response()
}
