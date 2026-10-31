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
use chromiumoxide::Page;
use std::sync::Arc;
use std::time::Duration;

/// Best-effort close of a page target held behind an `Arc`. `Page::close`
/// takes ownership, so clone the inner page — the clone addresses the same
/// CDP target, so the real target closes.
pub async fn close_page(page: &Arc<Page>) {
    let _ = page.as_ref().clone().close().await;
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
pub async fn node_center(page: &Page, backend_node_id: i64) -> anyhow::Result<(f64, f64)> {
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
    Ok((f64::midpoint(min_x, max_x), f64::midpoint(min_y, max_y)))
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
pub async fn mouse_click(page: &Page, x: f64, y: f64) -> anyhow::Result<()> {
    mouse_event(page, DispatchMouseEventType::MouseMoved, x, y, 0).await?;
    tokio::time::sleep(Duration::from_millis(30)).await;
    mouse_event(page, DispatchMouseEventType::MousePressed, x, y, 1).await?;
    tokio::time::sleep(Duration::from_millis(40)).await;
    mouse_event(page, DispatchMouseEventType::MouseReleased, x, y, 1).await?;
    Ok(())
}

/// Real double-click at viewport coordinates.
pub async fn mouse_double_click(page: &Page, x: f64, y: f64) -> anyhow::Result<()> {
    mouse_event(page, DispatchMouseEventType::MouseMoved, x, y, 0).await?;
    for count in [1, 2] {
        mouse_event(page, DispatchMouseEventType::MousePressed, x, y, count).await?;
        mouse_event(page, DispatchMouseEventType::MouseReleased, x, y, count).await?;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    Ok(())
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
