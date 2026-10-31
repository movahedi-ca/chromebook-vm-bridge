//! Window geometry: viewport (CSS px) <-> screen (X11 px) coordinate math.
//!
//! Audit BUG-1: the legacy bridge maintained FIVE conflicting geometry
//! spaces (the legacy driver 1920x1040 viewport vs 1920x1080 window-size, a hardcoded
//! (20,22)/110px fallback, a (683,450) scroll center from a 1366x900 laptop,
//! and CDP document quads fed straight into viewport-relative clicks).
//!
//! Worst of all, `bind_window`/`sync_window_geometry` overwrote a valid
//! JS-derived offset cache with `root.translate_coords(win, 0, 0)`. In
//! Xlib, `src.translate_coords(dst, x, y)` translates coordinates
//! *from the receiver* into the destination's space, so that call answered
//! "where is the root origin inside the window's frame" (~(-`win_x`, -`win_y`)),
//! not "where is the window on the screen". Every visible click then missed
//! whenever Chrome was not at (0,0).
//!
//! The Xlib `translate_coords` path is DELETED here and must never come back.
//!
//! ## Audit 42 Critical-1: page JS is not ground truth
//!
//! `window.screenX/screenY/outerWidth/outerHeight` are evaluated in the
//! page, and a hostile page can return ANY values. They are used ONLY for
//! the agreement check in the visible-input path (the page's story about
//! the window origin must match X11 within a small tolerance, or the
//! action is refused). The SEC-1 gate rect and the viewport origin come
//! EXCLUSIVELY from [`crate::input::x11_mouse::resolve_window_rect`]:
//! `_NET_CLIENT_LIST` + `_NET_WM_PID` matched against the supervised
//! child pid, translated to root coordinates by the window manager.
//!
//! `window.innerWidth/innerHeight` are CSS math only: they size the
//! viewport inside the X11-derived rect and cannot move it. `devicePixelRatio`
//! must be 1 — visible input is refused outright on `HiDPI`.
//!
//! ## The `no_viewport` 1:1 assumption
//!
//! This math assumes the browser launches with NO viewport emulation:
//! chromiumoxide's viewport is configured to the real window size with no
//! device-metric override, so **CSS pixels == physical X11 pixels** and
//! `window.innerWidth/innerHeight` are already in screen pixels.
//!
//! If `window.devicePixelRatio != 1` (`HiDPI`) or the legacy driver-style emulation
//! is ever enabled, every coordinate here is wrong by that factor. Visible
//! input refuses to run in that case (fail closed); if `HiDPI` ever becomes
//! a deployment reality, the supervisor must assert `devicePixelRatio == 1`
//! at connect time instead.

use serde::{Deserialize, Serialize};

/// Window geometry snapshot, populated by evaluating [`GEOMETRY_JS`] in the
/// page and deserializing the returned object.
///
/// TRUST MODEL (audit 42 Critical-1): every field here comes from page JS
/// and is therefore attacker-influenced. `screen_x/screen_y/outer_w/outer_h`
/// are used ONLY for the X11 agreement check in the visible-input path —
/// never for the SEC-1 gate rect or the viewport origin, which come from
/// X11 ground truth. `inner_w/inner_h` are CSS math only (they size the
/// viewport inside the X11 rect and cannot move it).
///
/// Field renames match the exact JS property names (`screenX`, `outerWidth`,
/// …). Note: `#[serde(rename_all = "camelCase")]` would produce `outerW`,
/// not `outerWidth` — hence the explicit renames.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    /// `window.screenX`: the page's CLAIM about the window's left edge.
    /// Agreement-check only — never trusted for input coordinates.
    #[serde(rename = "screenX")]
    pub screen_x: i32,
    /// `window.screenY`: the page's CLAIM about the window's top edge.
    /// Agreement-check only.
    #[serde(rename = "screenY")]
    pub screen_y: i32,
    /// `window.outerWidth`: the page's CLAIM about the full window width.
    /// Agreement-check only.
    #[serde(rename = "outerWidth")]
    pub outer_w: i32,
    /// `window.outerHeight`: the page's CLAIM about the full window height.
    /// Agreement-check only.
    #[serde(rename = "outerHeight")]
    pub outer_h: i32,
    /// `window.innerWidth`: viewport width in CSS px (= screen px when
    /// `device_pixel_ratio == 1`). CSS math only.
    #[serde(rename = "innerWidth")]
    pub inner_w: i32,
    /// `window.innerHeight`: viewport height in CSS px. CSS math only.
    #[serde(rename = "innerHeight")]
    pub inner_h: i32,
    /// `window.devicePixelRatio`: must be 1 for visible input; the
    /// visible-input path refuses to run otherwise (audit 42 Critical-1).
    #[serde(rename = "devicePixelRatio")]
    pub device_pixel_ratio: f64,
}

/// JS evaluated in the page immediately before any visible action.
///
/// Returns `{screenX, screenY, outerWidth, outerHeight, innerWidth,
/// innerHeight, devicePixelRatio}`. The caller deserializes the result into
/// [`WindowGeometry`] (field names match via explicit renames).
///
/// TRUST (audit 42 Critical-1): this runs IN THE PAGE, so every value is
/// attacker-influenced. The visible-input path uses `screenX/screenY` only
/// for the X11 agreement check, `innerWidth/innerHeight` for CSS math, and
/// `devicePixelRatio` for the `HiDPI` refusal. The gate rect and viewport
/// origin always come from X11 (`resolve_window_rect`).
///
/// Evaluate fresh every time — never cache across navigations, resizes, or
/// tab switches. A stale cache is exactly what BUG-1 was.
pub const GEOMETRY_JS: &str = r"(() => ({
    screenX: window.screenX,
    screenY: window.screenY,
    outerWidth: window.outerWidth,
    outerHeight: window.outerHeight,
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
    devicePixelRatio: window.devicePixelRatio,
}))()";

impl WindowGeometry {
    /// Screen X of the viewport origin (left window border width).
    ///
    /// Assumes symmetric left/right borders: `(outer_w - inner_w) / 2`.
    /// Integer division floors; a 1px error here is harmless because the
    /// result is clamped to the viewport rect before any `XTest` call.
    /// Saturating: pathological geometry can never wrap into a wild
    /// coordinate (debug builds panic on overflow — this must not).
    pub fn origin_x(&self) -> i32 {
        self.screen_x
            .saturating_add((self.outer_w.saturating_sub(self.inner_w)) / 2)
    }

    /// Screen Y of the viewport origin (top chrome bar height).
    ///
    /// `outer_h - inner_h` is the full title/tab/toolbar stack; there is no
    /// bottom chrome to subtract on a normal window.
    pub fn origin_y(&self) -> i32 {
        self.screen_y
            .saturating_add(self.outer_h.saturating_sub(self.inner_h))
    }
}

/// Translate a viewport-relative point (CSS px) to absolute screen pixels.
///
/// ```text
/// screen_x = geom.screen_x + (geom.outer_w - geom.inner_w)/2 + viewport_x
/// screen_y = geom.screen_y + (geom.outer_h - geom.inner_h) + viewport_y
/// ```
///
/// Uses saturating arithmetic so pathological geometry can never wrap into
/// a wild coordinate. Callers must still pass the result through
/// [`clamp_to_viewport`] before handing it to `XTest`.
pub fn viewport_to_screen(viewport_x: i32, viewport_y: i32, geom: &WindowGeometry) -> (i32, i32) {
    (
        geom.origin_x().saturating_add(viewport_x),
        geom.origin_y().saturating_add(viewport_y),
    )
}

/// Clamp a screen point into the inner viewport rectangle
/// `[origin_x, origin_x + inner_w - 1] x [origin_y, origin_y + inner_h - 1]`.
///
/// This is the SEC-1 guard: `XTest` must never drive the pointer outside the
/// Chrome window (no clicking terminals, file managers, or password dialogs
/// on the shared :1 display). The `-1` keeps the clamped point strictly
/// inside the viewport instead of on its far edge.
pub fn clamp_to_viewport(x: i32, y: i32, geom: &WindowGeometry) -> (i32, i32) {
    let ox = geom.origin_x();
    let oy = geom.origin_y();
    let max_x = ox.saturating_add(geom.inner_w).saturating_sub(1);
    let max_y = oy.saturating_add(geom.inner_h).saturating_sub(1);
    (x.clamp(ox, max_x.max(ox)), y.clamp(oy, max_y.max(oy)))
}

/// Viewport-space scroll center, computed dynamically.
///
/// Replaces the old hardcoded `(683, 450)` — the center of a 1366x900 laptop
/// screen that had no business being in this codebase. The caller moves BOTH
/// the `XTest` pointer (screen coords via [`viewport_to_screen`]) and the CDP
/// mouse to this point before dispatching the wheel event (audit H4: the old
/// code moved only the `XTest` cursor while `page.mouse.wheel` fired at
/// the legacy driver's unmoved virtual position).
#[allow(dead_code)] // parked: scroll-center helper for a future visible-scroll path (audit LOW-1)
pub fn scroll_center(geom: &WindowGeometry) -> (i32, i32) {
    (geom.inner_w / 2, geom.inner_h / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Typical headed window at screen origin: 1920x1080 outer, 1920x1040
    /// inner (40px top chrome), matching the production bridge.
    fn typical() -> WindowGeometry {
        WindowGeometry {
            screen_x: 0,
            screen_y: 0,
            outer_w: 1920,
            outer_h: 1080,
            inner_w: 1920,
            inner_h: 1040,
            device_pixel_ratio: 1.0,
        }
    }

    #[test]
    fn origin_math_typical_window() {
        let g = typical();
        assert_eq!(g.origin_x(), 0); // (1920-1920)/2
        assert_eq!(g.origin_y(), 40); // 1080-1040
    }

    #[test]
    fn viewport_to_screen_applies_chrome_offsets() {
        let g = typical();
        // viewport (100, 200) -> screen (100, 240): 40px top bar shifts Y only
        assert_eq!(viewport_to_screen(100, 200, &g), (100, 240));
        assert_eq!(viewport_to_screen(0, 0, &g), (0, 40));
    }

    #[test]
    fn viewport_to_screen_with_offset_window() {
        // Window dragged to (100, 50) with asymmetric chrome: 10px side
        // borders, 80px top stack.
        let g = WindowGeometry {
            screen_x: 100,
            screen_y: 50,
            outer_w: 1920,
            outer_h: 1080,
            inner_w: 1900,
            inner_h: 1000,
            device_pixel_ratio: 1.0,
        };
        assert_eq!(g.origin_x(), 110); // 100 + (1920-1900)/2
        assert_eq!(g.origin_y(), 130); // 50 + (1080-1000)
        assert_eq!(viewport_to_screen(0, 0, &g), (110, 130));
        assert_eq!(viewport_to_screen(1900, 1000, &g), (2010, 1130));
    }

    #[test]
    fn viewport_to_screen_never_wraps_on_garbage_input() {
        let g = WindowGeometry {
            screen_x: i32::MAX,
            screen_y: i32::MAX,
            outer_w: 1920,
            outer_h: 1080,
            inner_w: 1920,
            inner_h: 1040,
            device_pixel_ratio: 1.0,
        };
        let (x, y) = viewport_to_screen(i32::MAX, i32::MAX, &g);
        assert_eq!((x, y), (i32::MAX, i32::MAX)); // saturates, does not wrap
    }

    #[test]
    fn clamp_keeps_inside_points_untouched() {
        let g = typical();
        assert_eq!(clamp_to_viewport(500, 500, &g), (500, 500));
        assert_eq!(clamp_to_viewport(0, 40, &g), (0, 40)); // top-left corner
        assert_eq!(clamp_to_viewport(1919, 1079, &g), (1919, 1079)); // bottom-right
    }

    #[test]
    fn clamp_pulls_outside_points_into_viewport() {
        let g = typical();
        // Left of window, above window, far right, far bottom.
        assert_eq!(clamp_to_viewport(-50, 500, &g), (0, 500));
        assert_eq!(clamp_to_viewport(500, 10, &g), (500, 40));
        assert_eq!(clamp_to_viewport(5000, 500, &g), (1919, 500));
        assert_eq!(clamp_to_viewport(500, 5000, &g), (500, 1079));
        // A point on the desktop outside Chrome entirely lands inside it.
        assert_eq!(clamp_to_viewport(-100, -100, &g), (0, 40));
    }

    #[test]
    fn scroll_center_is_dynamic_not_hardcoded() {
        // Old code used (683, 450) regardless of window size.
        assert_eq!(scroll_center(&typical()), (960, 520));
        let small = WindowGeometry {
            screen_x: 0,
            screen_y: 0,
            outer_w: 1366,
            outer_h: 900,
            inner_w: 1366,
            inner_h: 860,
            device_pixel_ratio: 1.0,
        };
        assert_eq!(scroll_center(&small), (683, 430));
    }

    #[test]
    fn geometry_js_has_no_xlib_translate_coords() {
        // Regression guard for BUG-1: the JS path must never reference the
        // deleted Xlib approach.
        assert!(!GEOMETRY_JS.contains("translate_coords"));
        for key in [
            "screenX",
            "screenY",
            "outerWidth",
            "outerHeight",
            "innerWidth",
            "innerHeight",
            "devicePixelRatio",
        ] {
            assert!(GEOMETRY_JS.contains(key), "missing {key}");
        }
    }

    #[test]
    fn geometry_deserializes_from_js_shape() {
        // The object GEOMETRY_JS returns must map onto WindowGeometry.
        let json = serde_json::json!({
            "screenX": 100, "screenY": 50,
            "outerWidth": 1920, "outerHeight": 1080,
            "innerWidth": 1900, "innerHeight": 1000,
            "devicePixelRatio": 1.0,
        });
        let g: WindowGeometry = serde_json::from_value(json).unwrap();
        assert_eq!(g.screen_x, 100);
        assert_eq!(g.inner_h, 1000);
        assert!((g.device_pixel_ratio - 1.0).abs() < 1e-9);
    }
}
