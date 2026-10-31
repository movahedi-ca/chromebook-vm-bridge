//! Shared browser action primitives for route handlers.
//!
//! Every handler in routes/{read,input,advanced}.rs builds on these.
//! Conventions:
//! - Coordinates are CSS viewport pixels unless a function says "screen".
//! - `active_page()` returns the registry's active tab, or adopts the
//!   browser's first existing target when the registry is empty (attach
//!   scenario), or opens about:blank as a last resort.
//! - Real input goes through CDP Input.dispatchMouseEvent, not synthetic
//!   JS .`click()`: JS clicks don't trigger real browser behavior for
//!   many widgets (verified on jspaint: only real clicks change state).

use crate::state::SharedState;
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, GetBoxModelParams, ResolveNodeParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::network::SetBlockedUrLsParams;
use chromiumoxide::Page;
use std::sync::Arc;
use std::time::Duration;

/// Best-effort close of a page target held behind an `Arc`. `Page::close`
/// takes ownership, so clone the inner page — the clone addresses the same
/// CDP target, so the real target closes.
pub async fn close_page(page: &Arc<Page>) {
    let _ = page.as_ref().clone().close().await;
}

/// Literal-IP block patterns for `Network.setBlockedURLs` (audit 42
/// High-1). CDP patterns match on URL strings (`*` wildcards), so they
/// only catch literal IPs — hostnames that resolve to private IPs are
/// still the job of `validate_navigate_url` + the post-goto re-check.
/// Defense in depth: blocks literal-IP subresource loads, websocket
/// upgrades, and redirect hops inside the renderer even if a check is
/// ever bypassed.
fn blocked_url_patterns() -> Vec<String> {
    let mut pats = Vec::new();
    // IPv4: whole-octet wildcards per scheme.
    let mut v4: Vec<String> = vec![
        "0".to_string(),               // 0.0.0.0/8
        "10".to_string(),              // 10.0.0.0/8
        "127".to_string(),             // loopback
        "169.254".to_string(),         // link-local (cloud metadata)
        "192.168".to_string(),         // 192.168.0.0/16
        "255.255.255.255".to_string(), // broadcast
    ];
    for i in 16..=31 {
        v4.push(format!("172.{i}")); // 172.16.0.0/12
    }
    for i in 64..=127 {
        v4.push(format!("100.{i}")); // 100.64.0.0/10 CGNAT
    }
    for scheme in ["http", "https"] {
        for net in &v4 {
            pats.push(format!("{scheme}://{net}.*"));
        }
        // IPv6 literals (URL form [::1]): loopback, ULA, link-local.
        // The wildcard sits inside the brackets; Chromium matches the
        // pattern against the URL string.
        for net in ["::1", "fc", "fd", "fe80"] {
            pats.push(format!("{scheme}://[{net}*"));
        }
    }
    pats
}

/// Install the literal-IP block patterns on a page's Network domain
/// (audit 42 High-1). Called once per new target; idempotent.
/// Best-effort: a failure is logged, never fatal — the Rust-side
/// firewall remains the primary gate.
/// Install `Network.setBlockedURLs` on a fresh target: the audit-42
/// literal-IP SSRF patterns plus any operator-configured
/// `blocked_resource_patterns` (opt-in resource blocking for faster
/// loads, e.g. ad/tracker hosts; empty by default = off). Best-effort;
/// the Rust firewall stays the primary gate.
pub async fn install_blocked_url_patterns(page: &Page, extra: &[String]) {
    let mut pats = blocked_url_patterns();
    pats.extend(extra.iter().cloned());
    let params = SetBlockedUrLsParams::new(pats);
    if let Err(e) = page.execute(params).await {
        tracing::warn!(error = %e, "setBlockedURLs failed (best-effort)");
    }
}

/// Get the active tab's page, adopting an existing browser target if the
/// registry is empty (happens right after attaching to a live browser).
/// Ensures the browser is connected first (attach-or-launch).
pub async fn active_page(state: &SharedState) -> anyhow::Result<Arc<Page>> {
    // Ensure we're connected (attach to the live browser or launch).
    crate::browser::tabs::ensure_browser(state).await?;
    // Fast path: registry has an active tab.
    {
        let mut tabs = state.tabs.write().await;
        if let Some(entry) = tabs.active() {
            return Ok(entry.page.clone());
        }
    }
    // Adopt: browser already has targets (attach scenario).
    let browser = {
        let b = state.browser1.lock().await;
        b.browser()
            .ok_or_else(|| anyhow::anyhow!("browser not connected"))?
    };
    let pages = browser.pages().await?;
    if let Some(page) = pages.into_iter().next() {
        let page = Arc::new(page);
        let url = page.url().await?.unwrap_or_default();
        let title = page
            .evaluate("document.title")
            .await
            .ok()
            .and_then(|r| r.into_value::<String>().ok())
            .unwrap_or_default();
        let evicted = {
            let mut tabs = state.tabs.write().await;
            let id = uuid::Uuid::new_v4().to_string();
            let ev = tabs.insert(crate::state::TabEntry {
                id: id.clone(),
                page: page.clone(),
                baseline_hash: None,
                url,
                title,
            });
            tracing::info!(tab_id = %id, "adopted existing browser target");
            ev
        };
        for p in evicted {
            close_page(&p).await;
        }
        return Ok(page);
    }
    // Last resort: open a blank page.
    let page = Arc::new(browser.new_page("about:blank").await?);
    let evicted = {
        let mut tabs = state.tabs.write().await;
        let id = uuid::Uuid::new_v4().to_string();
        tabs.insert(crate::state::TabEntry {
            id: id.clone(),
            page: page.clone(),
            baseline_hash: None,
            url: "about:blank".to_string(),
            title: String::new(),
        })
    };
    for p in evicted {
        close_page(&p).await;
    }
    Ok(page)
}

/// Lifecycle mode for [`navigate_with_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavWaitMode {
    /// Return once `Page.navigate` is acknowledged (fastest).
    Commit,
    /// Wait for the DOMContentLoaded event.
    DomContentLoaded,
    /// Wait for the load event.
    Load,
    /// Historical default: `page.goto()` with networkAlmostIdle.
    NetworkIdle,
}

impl NavWaitMode {
    /// Parse a `wait_until` string. Unknown values fall back to
    /// [`NavWaitMode::NetworkIdle`] (current behavior) rather than
    /// failing the navigation.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "commit" => Self::Commit,
            "domcontentloaded" | "dom_content_loaded" => Self::DomContentLoaded,
            "load" => Self::Load,
            _ => Self::NetworkIdle,
        }
    }
}

/// Navigate with an explicit lifecycle wait mode.
///
/// `NetworkIdle` delegates to `page.goto()` — byte-for-byte the
/// historical behavior. The early modes bypass `goto()` (which hardcodes
/// networkAlmostIdle) and instead register the CDP lifecycle listener
/// BEFORE issuing `Page.navigate`, so fast or cached responses cannot
/// fire the event before we start listening. The SSRF re-check stays the
/// caller's job and runs on every path after navigation completes.
pub async fn navigate_with_mode(
    page: &Page,
    url: &str,
    mode: NavWaitMode,
    timeout: Duration,
) -> anyhow::Result<()> {
    use chromiumoxide::cdp::browser_protocol::page::{
        EventDomContentEventFired, EventLoadEventFired, NavigateParams,
    };
    use futures_util::StreamExt;
    match mode {
        NavWaitMode::NetworkIdle => {
            tokio::time::timeout(timeout, page.goto(url))
                .await
                .map_err(|_| anyhow::anyhow!("navigation timeout"))??;
            Ok(())
        }
        NavWaitMode::Commit => {
            let params = NavigateParams::new(url);
            tokio::time::timeout(timeout, page.execute(params))
                .await
                .map_err(|_| anyhow::anyhow!("navigation timeout"))?
                .map_err(|e| anyhow::anyhow!("navigate failed: {e}"))?;
            Ok(())
        }
        NavWaitMode::DomContentLoaded => {
            let mut stream = page
                .event_listener::<EventDomContentEventFired>()
                .await
                .map_err(|e| anyhow::anyhow!("lifecycle listener: {e}"))?;
            tokio::time::timeout(timeout, async {
                let params = NavigateParams::new(url);
                page.execute(params)
                    .await
                    .map_err(|e| anyhow::anyhow!("navigate failed: {e}"))?;
                stream.next().await;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .map_err(|_| anyhow::anyhow!("navigation timeout"))??;
            Ok(())
        }
        NavWaitMode::Load => {
            let mut stream = page
                .event_listener::<EventLoadEventFired>()
                .await
                .map_err(|e| anyhow::anyhow!("lifecycle listener: {e}"))?;
            tokio::time::timeout(timeout, async {
                let params = NavigateParams::new(url);
                page.execute(params)
                    .await
                    .map_err(|e| anyhow::anyhow!("navigate failed: {e}"))?;
                stream.next().await;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .map_err(|_| anyhow::anyhow!("navigation timeout"))??;
            Ok(())
        }
    }
}

/// Evaluate JS and deserialize the result as JSON.
pub async fn eval_json(page: &Page, js: &str) -> anyhow::Result<serde_json::Value> {
    let result = tokio::time::timeout(Duration::from_secs(15), page.evaluate(js))
        .await
        .map_err(|_| anyhow::anyhow!("evaluate timeout"))??;
    let v: serde_json::Value = result
        .into_value()
        .map_err(|e| anyhow::anyhow!("deserialize evaluate result: {e}"))?;
    Ok(v)
}

/// Evaluate an async JS function and await its promise via CDP
/// `awaitPromise`. For scripts that must yield to the page's microtask
/// queue mid-evaluation (e.g. `el.focus()` → framework `onFocus`
/// microtasks → value mutation), which two sequential `eval_json` calls
/// used to get for free via the network gap between them.
pub async fn eval_json_async(page: &Page, js: &str) -> anyhow::Result<serde_json::Value> {
    use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
    let params = EvaluateParams::builder()
        .expression(js)
        .await_promise(true)
        .build()
        .map_err(|e| anyhow::anyhow!("async eval params: {e}"))?;
    let result = tokio::time::timeout(Duration::from_secs(15), page.evaluate(params))
        .await
        .map_err(|_| anyhow::anyhow!("evaluate timeout"))??;
    let v: serde_json::Value = result
        .into_value()
        .map_err(|e| anyhow::anyhow!("deserialize evaluate result: {e}"))?;
    Ok(v)
}

/// Center of the first element matching a CSS selector, in viewport coords.
pub async fn element_center(page: &Page, selector: &str) -> anyhow::Result<(f64, f64)> {
    let js = format!(
        r"(() => {{
            const el = document.querySelector({sel});
            if (!el) return null;
            const r = el.getBoundingClientRect();
            return {{x: r.x + r.width / 2, y: r.y + r.height / 2,
                     w: r.width, h: r.height}};
        }})()",
        sel = serde_json::to_string(selector)
            .map_err(|e| anyhow::anyhow!("selector literal: {e}"))?
    );
    let v = eval_json(page, &js).await?;
    if v.is_null() {
        anyhow::bail!("no element matches selector: {selector}");
    }
    let x = v["x"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("bad box x"))?;
    let y = v["y"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("bad box y"))?;
    Ok((x, y))
}

/// Center of a DOM node by backendNodeId, in viewport coords.
/// Uses DOM.resolveNode + DOM.getBoxModel (the `click_node` wiring).
/// Content quad + center of the node with the given backendNodeId, in
/// viewport coords. Read-only: `DOM.resolveNode` + `DOM.getBoxModel`, no
/// input is ever dispatched (audit 42 High-7 — this is the primitive the
/// AI-escalation tooling must use to locate a node, not `click_node`).
///
/// Returns `(center_x, center_y, quad)` where quad is the 8-number
/// content quad `[x1,y1, x2,y2, x3,y3, x4,y4]`.
pub async fn node_box(page: &Page, backend_node_id: i64) -> anyhow::Result<(f64, f64, Vec<f64>)> {
    let resolve = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(BackendNodeId::new(backend_node_id))
                .build(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("DOM.resolveNode failed: {e}"))?;
    let object_id = resolve
        .result
        .object
        .object_id
        .ok_or_else(|| anyhow::anyhow!("resolveNode: no objectId"))?;
    let box_model = page
        .execute(GetBoxModelParams::builder().object_id(object_id).build())
        .await
        .map_err(|e| anyhow::anyhow!("DOM.getBoxModel failed: {e}"))?;
    let model = box_model.result.model;
    // Content quad: [x1,y1, x2,y2, x3,y3, x4,y4]
    let q = model.content.inner();
    if q.len() < 8 {
        anyhow::bail!("getBoxModel: short content quad");
    }
    let min_x = q.iter().step_by(2).copied().fold(f64::INFINITY, f64::min);
    let max_x = q
        .iter()
        .step_by(2)
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = q
        .iter()
        .skip(1)
        .step_by(2)
        .copied()
        .fold(f64::INFINITY, f64::min);
    let max_y = q
        .iter()
        .skip(1)
        .step_by(2)
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    Ok((
        f64::midpoint(min_x, max_x),
        f64::midpoint(min_y, max_y),
        q.clone(),
    ))
}

/// Center of the node with the given backendNodeId, in viewport coords.
/// Thin wrapper over [`node_box`]; the click itself is the caller's job.
pub async fn node_center(page: &Page, backend_node_id: i64) -> anyhow::Result<(f64, f64)> {
    let (x, y, _) = node_box(page, backend_node_id).await?;
    Ok((x, y))
}

/// Dispatch one CDP mouse event at viewport coordinates.
async fn mouse_event(
    page: &Page,
    typ: DispatchMouseEventType,
    x: f64,
    y: f64,
    click_count: i64,
) -> anyhow::Result<()> {
    let params = DispatchMouseEventParams::builder()
        .r#type(typ)
        .x(x)
        .y(y)
        .button(MouseButton::Left)
        .click_count(click_count)
        .build()
        .map_err(|e: String| anyhow::anyhow!("mouse params: {e}"))?;
    page.execute(params)
        .await
        .map_err(|e| anyhow::anyhow!("dispatchMouseEvent failed: {e}"))?;
    Ok(())
}

/// Real click at viewport coordinates: press + release.
///
/// `timing` comes from the `interaction_profile` config ("normal" =
/// the historical 30/40 ms sleeps; "fast" = 5/10 ms). See
/// [`ClickTiming::from_profile`].
pub async fn mouse_click(page: &Page, x: f64, y: f64, timing: ClickTiming) -> anyhow::Result<()> {
    mouse_event(page, DispatchMouseEventType::MouseMoved, x, y, 0).await?;
    tokio::time::sleep(Duration::from_millis(timing.move_ms)).await;
    mouse_event(page, DispatchMouseEventType::MousePressed, x, y, 1).await?;
    tokio::time::sleep(Duration::from_millis(timing.press_ms)).await;
    mouse_event(page, DispatchMouseEventType::MouseReleased, x, y, 1).await?;
    Ok(())
}

/// Real double-click at viewport coordinates.
///
/// Timing mirrors the historical shape exactly under "normal": no sleep
/// after the initial move, `press_ms` after each press/release pair.
pub async fn mouse_double_click(
    page: &Page,
    x: f64,
    y: f64,
    timing: ClickTiming,
) -> anyhow::Result<()> {
    mouse_event(page, DispatchMouseEventType::MouseMoved, x, y, 0).await?;
    for count in [1, 2] {
        mouse_event(page, DispatchMouseEventType::MousePressed, x, y, count).await?;
        mouse_event(page, DispatchMouseEventType::MouseReleased, x, y, count).await?;
        tokio::time::sleep(Duration::from_millis(timing.press_ms)).await;
    }
    Ok(())
}

/// Click timing derived from the `interaction_profile` config value.
#[derive(Debug, Clone, Copy)]
pub struct ClickTiming {
    /// Sleep after MouseMoved, before MousePressed.
    pub move_ms: u64,
    /// Sleep after MousePressed, before MouseReleased (and between pairs).
    pub press_ms: u64,
}

impl ClickTiming {
    /// Resolve the profile string. "normal" reproduces the historical
    /// 30/40 ms sleeps exactly; "fast" drops to 5/10 ms. Unknown values
    /// warn and fall back to "normal" — never fail a click on config.
    pub fn from_profile(profile: &str) -> Self {
        match profile {
            "fast" => Self {
                move_ms: 5,
                press_ms: 10,
            },
            "normal" => Self {
                move_ms: 30,
                press_ms: 40,
            },
            other => {
                tracing::warn!(profile = other, "unknown interaction_profile; using normal");
                Self {
                    move_ms: 30,
                    press_ms: 40,
                }
            }
        }
    }
}

/// Move the mouse to viewport coordinates (hover).
pub async fn mouse_move(page: &Page, x: f64, y: f64) -> anyhow::Result<()> {
    mouse_event(page, DispatchMouseEventType::MouseMoved, x, y, 0).await
}

/// Drag through viewport points: press at first, move through rest, release.
pub async fn mouse_drag(page: &Page, points: &[(f64, f64)]) -> anyhow::Result<()> {
    if points.is_empty() {
        anyhow::bail!("drag needs at least one point");
    }
    let (x0, y0) = points[0];
    mouse_event(page, DispatchMouseEventType::MouseMoved, x0, y0, 0).await?;
    mouse_event(page, DispatchMouseEventType::MousePressed, x0, y0, 1).await?;
    for &(x, y) in &points[1..] {
        mouse_event(page, DispatchMouseEventType::MouseMoved, x, y, 0).await?;
    }
    let (xl, yl) = points[points.len() - 1];
    mouse_event(page, DispatchMouseEventType::MouseReleased, xl, yl, 1).await?;
    Ok(())
}

/// Ramer-Douglas-Peucker polyline simplification — OPT-IN drag path
/// only (`simplify: true` on `/browser/drag`). Keeps the first and last
/// points; drops intermediate points within `epsilon` px of the chord,
/// cutting CDP round trips on dense polylines. The default drag path
/// never calls this: drag precision matters, so simplification stays
/// an explicit opt-in. Iterative (no recursion depth limit).
pub fn rdp_simplify(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((start, end)) = stack.pop() {
        if end <= start + 1 {
            continue;
        }
        let (x0, y0) = points[start];
        let (x1, y1) = points[end];
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        let mut max_dist = 0.0;
        let mut max_idx = start;
        for (i, &(px, py)) in points.iter().enumerate().take(end).skip(start + 1) {
            // Perpendicular distance from p to the chord start->end.
            let dist = if len == 0.0 {
                ((px - x0).powi(2) + (py - y0).powi(2)).sqrt()
            } else {
                (dy * px - dx * py + x1 * y0 - y1 * x0).abs() / len
            };
            if dist > max_dist {
                max_dist = dist;
                max_idx = i;
            }
        }
        if max_dist > epsilon {
            keep[max_idx] = true;
            stack.push((start, max_idx));
            stack.push((max_idx, end));
        }
    }
    points
        .iter()
        .zip(keep.iter())
        .filter(|(_, k)| **k)
        .map(|(p, _)| *p)
        .collect()
}

/// Insert text at the focused element (CDP, no keystroke timing issues).
pub async fn insert_text(page: &Page, text: &str) -> anyhow::Result<()> {
    use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
    let params = InsertTextParams::builder()
        .text(text)
        .build()
        .map_err(|e: String| anyhow::anyhow!("insertText params: {e}"))?;
    page.execute(params)
        .await
        .map_err(|e| anyhow::anyhow!("Input.insertText failed: {e}"))?;
    Ok(())
}

/// Dispatch a key event by key name (e.g. "Enter", "Tab", "Escape").
pub async fn press_key(page: &Page, key: &str) -> anyhow::Result<()> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchKeyEventParams, DispatchKeyEventType,
    };
    // Minimal key mapping: CDP wants windowsVirtualKeyCode for special keys.
    let (vk, code) = match key {
        "Enter" => (13, "Enter"),
        "Tab" => (9, "Tab"),
        "Escape" => (27, "Escape"),
        "Backspace" => (8, "Backspace"),
        "Delete" => (46, "Delete"),
        "ArrowLeft" => (37, "ArrowLeft"),
        "ArrowRight" => (39, "ArrowRight"),
        "ArrowUp" => (38, "ArrowUp"),
        "ArrowDown" => (40, "ArrowDown"),
        "Home" => (36, "Home"),
        "End" => (35, "End"),
        _ => (0, key),
    };
    for typ in [DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp] {
        let params = DispatchKeyEventParams::builder()
            .r#type(typ)
            .key(key)
            .code(code)
            .windows_virtual_key_code(vk)
            .build()
            .map_err(|e: String| anyhow::anyhow!("key params: {e}"))?;
        page.execute(params)
            .await
            .map_err(|e| anyhow::anyhow!("dispatchKeyEvent failed: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_timing_normal_is_historical() {
        let t = ClickTiming::from_profile("normal");
        assert_eq!(t.move_ms, 30);
        assert_eq!(t.press_ms, 40);
    }

    #[test]
    fn click_timing_unknown_falls_back_to_normal() {
        let t = ClickTiming::from_profile("turbo");
        assert_eq!(t.move_ms, 30);
        assert_eq!(t.press_ms, 40);
    }

    #[test]
    fn click_timing_fast_is_faster() {
        let t = ClickTiming::from_profile("fast");
        assert_eq!(t.move_ms, 5);
        assert_eq!(t.press_ms, 10);
    }

    #[test]
    fn nav_wait_mode_parses() {
        assert_eq!(NavWaitMode::parse("commit"), NavWaitMode::Commit);
        assert_eq!(
            NavWaitMode::parse("domcontentloaded"),
            NavWaitMode::DomContentLoaded
        );
        assert_eq!(NavWaitMode::parse("load"), NavWaitMode::Load);
        assert_eq!(NavWaitMode::parse("networkidle"), NavWaitMode::NetworkIdle);
        // Unknown and empty fall back to the historical default.
        assert_eq!(NavWaitMode::parse("bogus"), NavWaitMode::NetworkIdle);
        assert_eq!(NavWaitMode::parse(""), NavWaitMode::NetworkIdle);
    }

    #[test]
    fn rdp_preserves_endpoints_and_straight_lines() {
        // Collinear points collapse to the endpoints.
        let pts: Vec<(f64, f64)> = (0..=10).map(|i| (i as f64 * 10.0, 0.0)).collect();
        let out = rdp_simplify(&pts, 2.0);
        assert_eq!(out, vec![(0.0, 0.0), (100.0, 0.0)]);
    }

    #[test]
    fn rdp_keeps_significant_deviations() {
        // A V shape: the apex deviates far from the chord and must survive.
        let pts = vec![(0.0, 0.0), (50.0, 50.0), (100.0, 0.0)];
        let out = rdp_simplify(&pts, 2.0);
        assert_eq!(out, pts);
    }

    #[test]
    fn rdp_short_inputs_unchanged() {
        assert_eq!(rdp_simplify(&[], 2.0), vec![]);
        assert_eq!(rdp_simplify(&[(1.0, 2.0)], 2.0), vec![(1.0, 2.0)]);
    }
}
