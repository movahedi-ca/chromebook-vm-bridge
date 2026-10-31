//! Pool route handlers: isolated context pool on the main browser's pages
//! (a separate headless browser was abandoned; see pool/mod.rs).
//!
//! Locking discipline: pool-state mutation happens under
//! `state.pool.lock().await` and the lock is never held across slow CDP
//! calls. Handlers open pages BEFORE taking the lock and close evicted or
//! released pages AFTER dropping it; only map bookkeeping (prune, LRU
//! evict, insert) happens under the lock.

use crate::browser::actions::{close_page, element_center, eval_json, insert_text, mouse_click};
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
    #[serde(default = "default_wait_until")]
    #[allow(dead_code)] // accepted by the API; wait strategies not implemented yet
    pub wait_until: String,
}

fn default_wait_until() -> String {
    "domcontentloaded".to_string()
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
    // Open the page BEFORE taking the pool lock: new_page is a slow CDP
    // round-trip and the lock is never held across it (audit 40 MED-4).
    let page = match browser.new_page("about:blank").await {
        Ok(p) => Arc::new(p),
        Err(e) => return err503(format!("pool new_page: {e}")),
    };
    let (id, to_close) = {
        let mut pool = state.pool.lock().await;
        let to_close = pool.prepare_acquire();
        let id = pool.register(page, req.ttl_s, req.trace);
        (id, to_close)
    };
    for p in to_close {
        close_page(&p).await;
    }
    Json(json!({"ok": true, "context_id": id})).into_response()
}

/// POST /pool/release {`context_id`} -> {"ok":true}
pub async fn release(
    State(state): State<SharedState>,
    Json(req): Json<ContextIdReq>,
) -> impl IntoResponse {
    let page = {
        let mut pool = state.pool.lock().await;
        pool.release(&req.context_id)
    };
    match page {
        Some(p) => {
            close_page(&p).await;
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
        tokio::time::timeout(Duration::from_secs(30), page.goto(validated_url.as_str())).await;
    match nav {
        Ok(Ok(_)) => match page.url().await {
            Ok(url) => Json(json!({"ok": true, "url": url.unwrap_or_default()})).into_response(),
            Err(e) => err500(format!("navigate url read: {e}")),
        },
        Ok(Err(e)) => err500(format!("navigate failed: {e}")),
        Err(_) => err500("navigate timed out after 30s"),
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
    match mouse_click(&page, x, y).await {
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
        match mouse_click(&page, x, y).await {
            Ok(()) => Json(json!({"ok": true})).into_response(),
            Err(e) => err500(format!("pool order click: {e}")),
        }
    } else {
        // fill: real click to focus, select-all on the focused field, then
        // CDP text insertion so keystroke-driven widgets react.
        if let Err(e) = mouse_click(&page, x, y).await {
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
    let (pruned, contexts, max, browser) = {
        let mut pool = state.pool.lock().await;
        let pruned = pool.prune();
        (
            pruned,
            pool.context_count(),
            pool.max_contexts(),
            PoolState::is_browser_connected(),
        )
    };
    for p in pruned {
        close_page(&p).await;
    }
    Json(json!({
        "ok": true,
        "contexts": contexts,
        "max": max,
        "browser": browser,
    }))
    .into_response()
}

/// POST /pool/speculate -> {"ok":true,"contexts":N}
/// Lightweight pool health probe; no parity target, kept trivial.
pub async fn speculate(State(state): State<SharedState>) -> impl IntoResponse {
    let pool = state.pool.lock().await;
    Json(json!({"ok": true, "contexts": pool.context_count()})).into_response()
}
