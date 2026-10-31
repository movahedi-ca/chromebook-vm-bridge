//! Browser input route handlers: clicks, drags, fills, keys, waits, eval.
//!
//! All native input goes through CDP (`Input.dispatchMouseEvent`,
//! `Input.insertText`, `Input.dispatchKeyEvent`) via the shared primitives
//! in `crate::browser::actions` — never synthetic JS `.click()`.
//!
//! The `visible` flag (on `/browser/click` and `/browser/fill`) drives the
//! REAL OS cursor through the X11/XTest driver in
//! `crate::input::x11_mouse`, fully visible on the attached display. The
//! driver is fully synchronous blocking X11 I/O, so it always runs inside
//! `tokio::task::spawn_blocking` and never holds the connection across
//! `.await`.
//!
//! SEC-1 (audit 42 Critical-1): the window rect that gates visible input
//! comes EXCLUSIVELY from X11 ground truth
//! (`x11_mouse::resolve_window_rect`: `_NET_CLIENT_LIST` + `_NET_WM_PID`
//! matched against the supervised child pid). Page-evaluated geometry
//! (`window.screenX`, `outerWidth`, …) is attacker-influenced: it is used
//! only for the X11 agreement check and for CSS sizing (`innerWidth` /
//! `innerHeight`), never to position the gate rect. Visible input is
//! refused when the X11 query fails, when `devicePixelRatio != 1`, when
//! the page's origin story disagrees with X11, or when the lease
//! generation changed mid-flight.
//!
//! Coordinate convention: CSS viewport pixels everywhere in the API, except
//! the handlers explicitly documented as taking absolute screen pixels
//! (`click_screen`, `double_click`, `move_to`).

use crate::browser::actions::{
    active_page, element_center, eval_json, insert_text, mouse_click, mouse_double_click,
    mouse_drag, mouse_move, node_center, press_key,
};
use crate::browser::geometry::{
    clamp_to_viewport, viewport_to_screen, WindowGeometry, GEOMETRY_JS,
};
use crate::input::x11_mouse::{resolve_window_rect, X11Mouse};
use crate::interact::react::FILL_REACT_JS;
use crate::state::SharedState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chromiumoxide::Page;
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Agreement tolerance between the page-reported window origin
/// (`window.screenX/screenY`) and X11 ground truth (audit 42 Critical-1).
/// Small enough that a lying page cannot steer input onto another window;
/// large enough for window-manager rounding.
const WINDOW_ORIGIN_TOLERANCE_PX: i32 = 32;

// ---------------------------------------------------------------------------
// Request bodies (mirror the Req models of the bridge's HTTP contract).
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ClickReq {
    pub selector: String,
    #[serde(default)]
    pub visible: bool,
}

#[derive(Deserialize)]
pub struct FillReq {
    pub selector: String,
    pub text: String,
    #[serde(default)]
    pub visible: bool,
}

#[derive(Deserialize)]
pub struct ClickAtReq {
    pub x: f64,
    pub y: f64,
}

#[derive(Deserialize)]
pub struct ClickNodeReq {
    #[serde(rename = "backendNodeId")]
    pub backend_node_id: i64,
}

/// POST `/browser/node_box` — `{backendNodeId}`. Read-only: DOM.resolveNode +
/// DOM.getBoxModel; returns the content quad and its center in viewport
/// coords. NO input is ever dispatched (audit 42 High-7): AI-escalation
/// tooling locates nodes through this endpoint, never through
/// `click_node` as a "dry run".
pub async fn node_box(
    State(state): State<SharedState>,
    Json(req): Json<ClickNodeReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        let (x, y, quad) = tokio::time::timeout(
            Duration::from_secs(30),
            crate::browser::actions::node_box(&page, req.backend_node_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("node_box timed out"))??;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "at": {"x": x, "y": y}, "quad": quad})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

#[derive(Deserialize)]
pub struct ClickScreenReq {
    pub x: f64,
    pub y: f64,
}

fn default_draw_speed() -> f64 {
    1.0
}

#[derive(Deserialize)]
pub struct DragReq {
    pub points: Vec<Vec<f64>>,
    #[serde(default = "default_draw_speed")]
    pub draw_speed: f64,
}

#[derive(Deserialize)]
pub struct ScrollReq {
    #[serde(default = "default_scroll_direction")]
    pub direction: String,
    #[serde(default = "default_scroll_amount")]
    pub amount: i64,
}

fn default_scroll_direction() -> String {
    "down".to_string()
}

fn default_scroll_amount() -> i64 {
    500
}

#[derive(Deserialize)]
pub struct PressReq {
    pub selector: String,
    #[serde(default = "default_press_key")]
    pub key: String,
}

fn default_press_key() -> String {
    "Enter".to_string()
}

#[derive(Deserialize)]
pub struct WaitReq {
    #[serde(default)]
    pub selector: String,
    #[serde(default = "default_wait_timeout")]
    pub timeout_ms: u64,
}

fn default_wait_timeout() -> u64 {
    10_000
}

#[derive(Deserialize)]
pub struct EvalReq {
    pub js: String,
}

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

fn bad_gateway(err: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"ok": false, "error": err.to_string()})),
    )
        .into_response()
}

/// Evaluate `GEOMETRY_JS` in the page and deserialize into `WindowGeometry`.
/// Always evaluated fresh, never cached across navigations or actions.
pub(crate) async fn window_geometry(page: &Page) -> anyhow::Result<WindowGeometry> {
    let v = eval_json(page, GEOMETRY_JS).await?;
    serde_json::from_value::<WindowGeometry>(v)
        .map_err(|e| anyhow::anyhow!("bad geometry shape: {e}"))
}

/// Convert absolute screen coords to viewport coords using a fresh
/// geometry snapshot (the reverse of `viewport_to_screen`).
async fn screen_to_viewport(page: &Page, sx: f64, sy: f64) -> anyhow::Result<(f64, f64)> {
    let geom = window_geometry(page).await?;
    Ok((
        sx - f64::from(geom.origin_x()),
        sy - f64::from(geom.origin_y()),
    ))
}

/// Drive the REAL OS cursor to a viewport point and click, fully visible on
/// the display. Runs the blocking `XTest` driver in `spawn_blocking`; the
/// returned coordinates are absolute screen pixels.
/// The display comes from config (audit 41 LOW-6): `X11Mouse` used to read
/// ambient $DISPLAY, which silently diverges from the configured display
/// the browser was spawned on.
///
/// TRUST (audit 42 Critical-1): the SEC-1 gate rect and the viewport origin
/// come EXCLUSIVELY from `resolve_window_rect` (X11: `_NET_CLIENT_LIST` +
/// `_NET_WM_PID` == supervised child pid). The fresh page geometry is used
/// only for (a) the `devicePixelRatio == 1` refusal, (b) the agreement
/// check of the page's origin story against X11, and (c) `innerWidth` /
/// `innerHeight` CSS sizing — which cannot move the rect. `gen0` is the
/// lease generation captured at admission: a hand-over mid-flight aborts
/// before any `XTest` input is emitted (audit 42 High-4).
#[allow(clippy::cast_possible_truncation)] // viewport f64 -> i32 screen px; finite and range-checked downstream
pub(crate) async fn visible_click(
    state: &SharedState,
    vx: f64,
    vy: f64,
    geom: &WindowGeometry,
    gen0: u64,
) -> anyhow::Result<(i32, i32)> {
    // HiDPI breaks the CSS-px == screen-px assumption: refuse rather than
    // click blind.
    if (geom.device_pixel_ratio - 1.0).abs() > 1e-9 {
        anyhow::bail!(
            "visible input refused: devicePixelRatio is {}, need 1",
            geom.device_pixel_ratio
        );
    }
    let pid = state.chrome_pid.load(Ordering::SeqCst);
    if pid == 0 {
        anyhow::bail!(
            "visible input refused: Chrome child pid unknown (browser not supervised yet)"
        );
    }
    // Lease hand-over mid-flight: abort before any irreversible input.
    if state.lease_gen_changed(gen0) {
        anyhow::bail!("visible input refused: lease changed mid-flight");
    }
    let display = state.config.display.clone();
    // X11 ground truth for the gate rect. NEVER page JS.
    let rect =
        tokio::task::spawn_blocking(move || resolve_window_rect(Some(display.as_str()), pid))
            .await
            .map_err(|e| anyhow::anyhow!("window-resolve task failed: {e}"))?
            .map_err(|e| anyhow::anyhow!("visible input refused: {e}"))?;
    // Agreement: the page's story about the window origin must match X11.
    // A hostile page reporting a shifted origin is refused loudly.
    if (geom.screen_x - rect.x).abs() > WINDOW_ORIGIN_TOLERANCE_PX
        || (geom.screen_y - rect.y).abs() > WINDOW_ORIGIN_TOLERANCE_PX
    {
        anyhow::bail!(
            "visible input refused: page-reported origin ({},{}) disagrees with X11 ({},{})",
            geom.screen_x,
            geom.screen_y,
            rect.x,
            rect.y
        );
    }
    // Viewport math anchored on the X11 rect: build a synthetic geometry
    // whose origin/size are X11-derived and whose inner size is the page's
    // CSS math. Page values can no longer move the rect.
    let x11_w = i32::try_from(rect.w).unwrap_or(i32::MAX);
    let x11_h = i32::try_from(rect.h).unwrap_or(i32::MAX);
    let anchored = WindowGeometry {
        screen_x: rect.x,
        screen_y: rect.y,
        outer_w: x11_w,
        outer_h: x11_h,
        inner_w: geom.inner_w,
        inner_h: geom.inner_h,
        device_pixel_ratio: geom.device_pixel_ratio,
    };
    let (sx, sy) = viewport_to_screen(vx.round() as i32, vy.round() as i32, &anchored);
    let (sx, sy) = clamp_to_viewport(sx, sy, &anchored);
    let display = state.config.display.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<(i32, i32)> {
        let mut mouse = X11Mouse::new(Some(display.as_str()))?;
        mouse.set_window_rect(rect); // X11 ground truth ONLY — never page JS
        mouse.click(sx, sy)?;
        Ok((sx, sy))
    })
    .await
    .map_err(|e| anyhow::anyhow!("visible click task failed: {e}"))?
}

/// Focus the element matching a selector; errors when nothing matches.
async fn focus_element(page: &Page, selector: &str) -> anyhow::Result<()> {
    let sel_lit =
        serde_json::to_string(selector).map_err(|e| anyhow::anyhow!("selector literal: {e}"))?;
    let js = format!(
        "(() => {{ const el = document.querySelector({sel_lit}); if (!el) return null; el.focus(); return true; }})()"
    );
    let v = eval_json(page, &js).await?;
    if v.is_null() {
        anyhow::bail!("no element matches selector: {selector}");
    }
    Ok(())
}

/// Read the verifiable value of the element matching a selector:
/// `el.value` for form controls, `el.textContent` otherwise.
async fn read_target_value(page: &Page, selector: &str) -> anyhow::Result<String> {
    let sel_lit =
        serde_json::to_string(selector).map_err(|e| anyhow::anyhow!("selector literal: {e}"))?;
    let js = format!(
        "(() => {{ const el = document.querySelector({sel_lit}); if (!el) return null; \
           return (el.value !== undefined ? el.value : (el.textContent || '')); }})()"
    );
    let v = eval_json(page, &js).await?;
    if v.is_null() {
        anyhow::bail!("no element matches selector: {selector}");
    }
    Ok(v.as_str().unwrap_or_default().to_string())
}

/// Set a form control's value through the React-native-prototype primitive
/// (native value setter + bubbling input/change/blur), returning the
/// read-back `el.value`.
async fn fill_react(page: &Page, selector: &str, text: &str) -> anyhow::Result<String> {
    let sel_lit =
        serde_json::to_string(selector).map_err(|e| anyhow::anyhow!("selector literal: {e}"))?;
    let text_lit = serde_json::to_string(text).map_err(|e| anyhow::anyhow!("text literal: {e}"))?;
    let js = format!(
        "(() => {{ const el = document.querySelector({sel_lit}); if (!el) return null; \
           const fill = {FILL_REACT_JS}; return fill(el, {text_lit}); }})()"
    );
    let v = eval_json(page, &js).await?;
    if v.is_null() {
        anyhow::bail!("no element matches selector: {selector}");
    }
    Ok(v.as_str().unwrap_or_default().to_string())
}

/// Click an element (center of its bounding box) then insert text at it.
async fn click_then_type(page: &Page, selector: &str, text: &str) -> anyhow::Result<()> {
    let (x, y) = element_center(page, selector).await?;
    mouse_click(page, x, y).await?;
    insert_text(page, text).await?;
    Ok(())
}

/// Evaluate JS and cap a string result at ~200KB so a huge page dump can
/// never blow up the response. Shared by /browser/eval and /pool/eval
/// (audit 41 LOW-9).
///
/// Audit 42 High-5: string results are PII/secret-redacted on the way
/// out. Audit 42 Medium: the old cap applied to strings ONLY — a huge
/// object/array result passed through uncapped. Non-string results are
/// now capped on their serialized size with an explicit truncation
/// marker, so no eval result can exceed the cap in any shape.
pub(crate) async fn eval_capped(page: &Page, js: &str) -> anyhow::Result<serde_json::Value> {
    const MAX_CHARS: usize = 200_000;
    let v = eval_json(page, js).await?;
    match v {
        serde_json::Value::String(s) => {
            let (redacted, _) = crate::privy::redact(&s);
            if redacted.chars().count() > MAX_CHARS {
                let truncated: String = redacted.chars().take(MAX_CHARS).collect();
                Ok(json!(format!("{}...[truncated]", truncated)))
            } else {
                Ok(serde_json::Value::String(redacted))
            }
        }
        other => {
            let serialized = serde_json::to_string(&other).unwrap_or_default();
            if serialized.chars().count() > MAX_CHARS {
                let truncated: String = serialized.chars().take(MAX_CHARS).collect();
                Ok(json!(format!("{}...[truncated]", truncated)))
            } else {
                Ok(other)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// POST /browser/click — {selector, visible}. Real CDP click at the
/// element's center; visible=true drives the real OS cursor instead.
pub async fn click(
    State(state): State<SharedState>,
    Json(req): Json<ClickReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        let (x, y) = element_center(&page, &req.selector).await?;
        if req.visible {
            // Lease generation at admission: a hand-over mid-flight aborts
            // the visible input before any XTest event (audit 42 High-4).
            let gen0 = state.lease_gen.load(Ordering::SeqCst);
            let geom = window_geometry(&page).await?;
            let (sx, sy) = tokio::time::timeout(
                Duration::from_secs(60),
                visible_click(&state, x, y, &geom, gen0),
            )
            .await
            .map_err(|_| anyhow::anyhow!("visible click timed out"))??;
            Ok((
                StatusCode::OK,
                Json(json!({"ok": true, "at": {"x": sx, "y": sy}, "mode": "visible"})),
            ))
        } else {
            tokio::time::timeout(Duration::from_secs(30), mouse_click(&page, x, y))
                .await
                .map_err(|_| anyhow::anyhow!("click timed out"))??;
            Ok((
                StatusCode::OK,
                Json(json!({"ok": true, "at": {"x": x.round(), "y": y.round()}, "mode": "native"})),
            ))
        }
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /`browser/click_at` — {x, y} in viewport coords. Real CDP click.
pub async fn click_at(
    State(state): State<SharedState>,
    Json(req): Json<ClickAtReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        tokio::time::timeout(Duration::from_secs(30), mouse_click(&page, req.x, req.y))
            .await
            .map_err(|_| anyhow::anyhow!("click_at timed out"))??;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "at": {"x": req.x, "y": req.y}})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /`browser/click_node` — {backendNodeId}. DOM.resolveNode +
/// DOM.getBoxModel give the content quad; real CDP click on its center.
pub async fn click_node(
    State(state): State<SharedState>,
    Json(req): Json<ClickNodeReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        let (x, y) = tokio::time::timeout(
            Duration::from_secs(30),
            node_center(&page, req.backend_node_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("node_center timed out"))??;
        mouse_click(&page, x, y).await?;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "at": {"x": x.round(), "y": y.round()}})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /`browser/click_screen` — {x, y} ABSOLUTE SCREEN coords. Converts to
/// viewport via the fresh window offset, then real CDP click.
pub async fn click_screen(
    State(state): State<SharedState>,
    Json(req): Json<ClickScreenReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        let (vx, vy) = screen_to_viewport(&page, req.x, req.y).await?;
        tokio::time::timeout(Duration::from_secs(30), mouse_click(&page, vx, vy))
            .await
            .map_err(|_| anyhow::anyhow!("click_screen timed out"))??;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "at": {"x": req.x, "y": req.y}})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /`browser/double_click` — {x, y} absolute screen coords (same shape
/// as `click_screen`). Converts to viewport, then real CDP double-click.
pub async fn double_click(
    State(state): State<SharedState>,
    Json(req): Json<ClickScreenReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        let (vx, vy) = screen_to_viewport(&page, req.x, req.y).await?;
        tokio::time::timeout(Duration::from_secs(30), mouse_double_click(&page, vx, vy))
            .await
            .map_err(|_| anyhow::anyhow!("double_click timed out"))??;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "at": {"x": req.x, "y": req.y}})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /browser/drag — `{points: [[x,y],...], draw_speed}`. Points are
/// viewport coords; the button is held through the whole path.
pub async fn drag(State(state): State<SharedState>, Json(req): Json<DragReq>) -> impl IntoResponse {
    if req.points.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "drag needs at least one point"})),
        )
            .into_response();
    }
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(req.points.len());
    for p in &req.points {
        if p.len() != 2 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "each drag point must be [x, y]"})),
            )
                .into_response();
        }
        pts.push((p[0], p[1]));
    }
    // draw_speed is not implemented; fail fast rather than silently
    // ignoring it (audit 40 LOW-8).
    if (req.draw_speed - 1.0).abs() > f64::EPSILON {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"ok": false, "error": "draw_speed not implemented; use 1.0"})),
        )
            .into_response();
    }
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        tokio::time::timeout(Duration::from_secs(120), mouse_drag(&page, &pts))
            .await
            .map_err(|_| anyhow::anyhow!("drag timed out"))??;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "points": pts.len()})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /`browser/move_to` — {x, y} ABSOLUTE SCREEN coords. Converts to
/// viewport, then moves the mouse (hover, no click).
pub async fn move_to(
    State(state): State<SharedState>,
    Json(req): Json<ClickScreenReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        let (vx, vy) = screen_to_viewport(&page, req.x, req.y).await?;
        mouse_move(&page, vx, vy).await?;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "at": {"x": req.x, "y": req.y}})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /`browser/move_to_viewport` — {x, y} viewport coords. Mouse hover,
/// no click.
pub async fn move_to_viewport(
    State(state): State<SharedState>,
    Json(req): Json<ClickAtReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        mouse_move(&page, req.x, req.y).await?;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "at": {"x": req.x, "y": req.y}})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /browser/scroll — {direction: "up"|"down", amount}. Real
/// window.scrollBy on the page.
pub async fn scroll(
    State(state): State<SharedState>,
    Json(req): Json<ScrollReq>,
) -> impl IntoResponse {
    let delta: i64 = match req.direction.as_str() {
        "down" => req.amount,
        "up" => -req.amount,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": format!("direction must be up|down, got {}", other)})),
            )
                .into_response();
        }
    };
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        eval_json(&page, &format!("window.scrollBy(0, {delta})")).await?;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "direction": req.direction, "amount": req.amount})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /browser/fill — {selector, text, visible}. Focuses the element,
/// sets the value through the React-native-prototype primitive (so
/// React-controlled inputs reconcile), and returns the read-back value.
/// visible=true clicks the element with the real OS cursor first.
pub async fn fill(State(state): State<SharedState>, Json(req): Json<FillReq>) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        let mut mode = "native";
        if req.visible {
            let (x, y) = element_center(&page, &req.selector).await?;
            // Lease generation at admission (audit 42 High-4).
            let gen0 = state.lease_gen.load(Ordering::SeqCst);
            let geom = window_geometry(&page).await?;
            tokio::time::timeout(
                Duration::from_secs(60),
                visible_click(&state, x, y, &geom, gen0),
            )
            .await
            .map_err(|_| anyhow::anyhow!("visible click timed out"))??;
            mode = "visible";
        }
        focus_element(&page, &req.selector).await?;
        let value = tokio::time::timeout(
            Duration::from_secs(30),
            fill_react(&page, &req.selector, &req.text),
        )
        .await
        .map_err(|_| anyhow::anyhow!("fill timed out"))??;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "value": value, "mode": mode})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /browser/type — {selector, text}. Real CDP click on the element,
/// then inserts the text at the focused element.
pub async fn type_text(
    State(state): State<SharedState>,
    Json(req): Json<FillReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        tokio::time::timeout(
            Duration::from_secs(60),
            click_then_type(&page, &req.selector, &req.text),
        )
        .await
        .map_err(|_| anyhow::anyhow!("type timed out"))??;
        Ok((StatusCode::OK, Json(json!({"ok": true}))))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /`browser/type_verified` — {selector, text}. Types, reads the
/// element's value back, retries once on mismatch. `verified` is true only
/// when the read-back equals the requested text.
pub async fn type_verified(
    State(state): State<SharedState>,
    Json(req): Json<FillReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        tokio::time::timeout(
            Duration::from_secs(60),
            click_then_type(&page, &req.selector, &req.text),
        )
        .await
        .map_err(|_| anyhow::anyhow!("type timed out"))??;
        let mut value = read_target_value(&page, &req.selector).await?;
        if value != req.text {
            // One retry: re-focus and insert again.
            click_then_type(&page, &req.selector, &req.text).await?;
            value = read_target_value(&page, &req.selector).await?;
        }
        let verified = value == req.text;
        Ok((
            StatusCode::OK,
            Json(json!({"ok": true, "verified": verified, "value": value})),
        ))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /browser/press — {selector, key}. Focuses the element, then
/// dispatches the key event through CDP.
pub async fn press(
    State(state): State<SharedState>,
    Json(req): Json<PressReq>,
) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        focus_element(&page, &req.selector).await?;
        tokio::time::timeout(Duration::from_secs(30), press_key(&page, &req.key))
            .await
            .map_err(|_| anyhow::anyhow!("press timed out"))??;
        Ok((StatusCode::OK, Json(json!({"ok": true}))))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /browser/wait — {selector, `timeout_ms`}. Polls every 250ms for the
/// selector to appear; an empty selector just sleeps the timeout.
pub async fn wait(State(state): State<SharedState>, Json(req): Json<WaitReq>) -> impl IntoResponse {
    // Clamp: an unclamped timeout_ms lets a client park a worker (audit 40
    // LOW-7). The 120s TimeoutLayer is the absolute backstop, so the
    // handler ceiling sits below it: a max-timeout wait must be able to
    // return its own honest result instead of racing the outer timeout
    // and losing (audit 41 LOW-1).
    let timeout_ms = req.timeout_ms.clamp(250, 110_000);
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        if req.selector.is_empty() {
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
            return Ok((StatusCode::OK, Json(json!({"ok": true, "found": true}))));
        }
        let sel_lit = serde_json::to_string(&req.selector)
            .map_err(|e| anyhow::anyhow!("selector literal: {e}"))?;
        let js = format!("document.querySelector({sel_lit}) !== null");
        let page = active_page(&state).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let found = loop {
            let v = eval_json(&page, &js).await?;
            if v.as_bool().unwrap_or(false) {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        Ok((StatusCode::OK, Json(json!({"ok": true, "found": found}))))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}

/// POST /browser/eval — {js}. Evaluates arbitrary JS and returns the
/// result; string results are truncated at ~200KB.
pub async fn eval(State(state): State<SharedState>, Json(req): Json<EvalReq>) -> impl IntoResponse {
    let res: anyhow::Result<(StatusCode, Json<serde_json::Value>)> = async {
        let page = active_page(&state).await?;
        let result = eval_capped(&page, &req.js).await?;
        Ok((StatusCode::OK, Json(json!({"ok": true, "result": result}))))
    }
    .await;
    match res {
        Ok(tup) => tup.into_response(),
        Err(e) => bad_gateway(e),
    }
}
