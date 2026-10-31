//! Action verification: per-tab baselines and the target-local gate.
//!
//! Audit BUG-5: the old "knowing bridge" check downscaled a 1920x1040 frame
//! to a 9x8 grid (64 bits) and allowed a Hamming distance of 10 (~15.6% of
//! the frame) in `/browser/order` while demanding exact equality in
//! `/browser/state`. Two mostly-white pages (two Workday steps, two Google
//! Docs) routinely fell within 10 bits and falsely passed; a critical inline
//! error banner changed <2 bits and went undetected. Worse, the pipelines
//! disagreed (`page.screenshot()` vs raw CDP `Page.captureScreenshot`),
//! `tab_switch` never re-baselined (guaranteed false `hash_mismatch` after
//! every switch), and `/browser/assert_visual` MUTATED the global baseline
//! as a side effect of a read.
//!
//! The replacement, per the playbook:
//!
//! 1. **Per-tab baselines** ([`VerifyState`]): refreshed on `tab_switch`
//!    and after the post-navigation hydration settle window. Never global.
//! 2. **Read-only asserts never mutate.** `get_baseline`/`check` take
//!    `&self`. Any endpoint whose name starts with "assert", "state", or
//!    "hash" must not call `set_baseline` — this is a code-review gate,
//!    not just a comment.
//! 3. **One capture pipeline**: everything goes through CDP
//!    `Page.captureScreenshot`. No mixing Playwright screenshots with CDP
//!    captures.
//! 4. **Target-local gate** ([`TargetCheck`]): before dispatching input to
//!    `[ref=N]`, verify the node is attached, visible, enabled, and that
//!    `document.elementFromPoint(cx, cy)` actually hits it (catches modal
//!    dialogs that stole focus). The coarse full-screen dHash remains only
//!    as a cheap smoke signal; the gate is what blocks a click.

#![allow(dead_code)] // parked: visual-verification feature not yet wired to any route (audit LOW-1)
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-tab visual baselines.
///
/// Keyed by tab id. A tab with no entry has NO baseline — [`VerifyState::check`]
/// returns `None` for it, and callers must treat that as "unverified, do not
/// act", never as a pass.
#[derive(Debug, Default)]
pub struct VerifyState {
    baselines: HashMap<String, u64>,
}

impl VerifyState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the baseline hash for a tab. Call on `tab_switch`, after the
    /// post-navigation hydration settle, and after any action that
    /// legitimately changes the page. NEVER call from a read-only assert
    /// endpoint (audit: `assert_visual` mutated state as a side effect).
    pub fn set_baseline(&mut self, tab_id: &str, hash: u64) {
        self.baselines.insert(tab_id.to_string(), hash);
    }

    /// Read a tab's baseline. Takes `&self` — reading never mutates.
    pub fn get_baseline(&self, tab_id: &str) -> Option<u64> {
        self.baselines.get(tab_id).copied()
    }

    /// Drop a tab's baseline (tab closed). Prevents stale hashes from
    /// haunting a recycled tab id.
    pub fn clear_baseline(&mut self, tab_id: &str) {
        self.baselines.remove(tab_id);
    }

    /// Compare a fresh capture against the tab's baseline.
    ///
    /// Returns `None` when the tab has no baseline (unverified — the caller
    /// must decide whether to proceed, and must NOT treat it as a pass).
    /// Otherwise `Some(true)` when `hamming(baseline, current) <= max_hamming`.
    pub fn check(&self, tab_id: &str, current: u64, max_hamming: u32) -> Option<bool> {
        self.get_baseline(tab_id)
            .map(|base| hamming(base, current) <= max_hamming)
    }
}

/// 64-bit difference hash of an image.
///
/// Decodes `image_bytes` (PNG/JPEG), converts to 8-bit grayscale, resamples
/// to 9x8 with nearest-neighbor, then sets bit `y*8+x` when pixel `(x,y)` is
/// brighter than its right neighbor `(x+1,y)`.
///
/// Undecodable input yields `0`. Callers must treat a `0` hash as
/// inconclusive (see [`VerifyState::check`]) — two decode failures must
/// never "match" each other into a false pass.
pub fn dhash(image_bytes: &[u8]) -> u64 {
    dhash_opt(image_bytes).unwrap_or(0)
}

fn dhash_opt(image_bytes: &[u8]) -> Option<u64> {
    let img = image::load_from_memory(image_bytes).ok()?;
    let gray = img.to_luma8();
    // 9x8, nearest: deterministic and cheap. Any resampling filter would do
    // for hashing, but Nearest keeps the transform bit-exact across runs.
    let small = image::imageops::resize(&gray, 9, 8, image::imageops::FilterType::Nearest);
    let mut hash: u64 = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            if small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0] {
                hash |= 1 << (y * 8 + x);
            }
        }
    }
    Some(hash)
}

/// Hamming distance between two 64-bit hashes: number of differing bits.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Result of the target-local precondition check ([`TARGET_CHECK_JS`]).
///
/// All four must hold before input is dispatched to the element. The struct
/// deserializes directly from the JS snippet's return object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetCheck {
    /// Node is still connected to the document (not detached by a re-render).
    pub attached: bool,
    /// Node has non-zero size and is not hidden via CSS.
    pub visible: bool,
    /// Node is not `disabled` and not `aria-disabled="true"`.
    pub enabled: bool,
    /// `document.elementFromPoint(cx, cy)` hits the node or its descendant.
    /// False while a modal/overlay covers the target.
    pub hit_target: bool,
}

impl TargetCheck {
    /// All preconditions hold: safe to dispatch input.
    pub fn ok(&self) -> bool {
        self.attached && self.visible && self.enabled && self.hit_target
    }

    /// The element is fine but something is ON TOP of it (modal dialog,
    /// cookie banner, overlay). The caller should dismiss the overlay or
    /// surface it to the agent — not click through it.
    pub fn blocked_by_overlay(&self) -> bool {
        self.attached && self.visible && self.enabled && !self.hit_target
    }
}

/// JS precondition check, evaluated as a function taking the target element.
///
/// Usage (routes layer): resolve the `[ref=N]` handle's `backendDOMNodeId`
/// via CDP `DOM.resolveNode` to a remote object, then call this function
/// with that object as its single argument, e.g.
/// `Runtime.callFunctionOn` with `functionDeclaration = TARGET_CHECK_JS`.
///
/// Returns `{attached, visible, enabled, hitTarget}` matching [`TargetCheck`]
/// (`rename_all = "camelCase"`).
///
/// Notes:
/// - `offsetParent` is deliberately NOT used for visibility: it is `null`
///   for `position: fixed` elements that are perfectly visible.
/// - `hitTarget` uses the element's own center; callers targeting a
///   sub-rectangle should pass explicit coordinates instead (not covered
///   here — the ref-based flow always targets element center).
pub const TARGET_CHECK_JS: &str = r#"(el) => {
    if (!el || !el.isConnected) {
        return { attached: false, visible: false, enabled: false, hitTarget: false };
    }
    const rect = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    const visible = rect.width > 0 && rect.height > 0
        && style.visibility !== 'hidden'
        && style.display !== 'none'
        && style.opacity !== '0'
        && style.contentVisibility !== 'hidden';
    const enabled = el.disabled !== true
        && el.getAttribute('aria-disabled') !== 'true';
    let hitTarget = false;
    if (visible) {
        const cx = rect.left + rect.width / 2;
        const cy = rect.top + rect.height / 2;
        const hit = document.elementFromPoint(cx, cy);
        hitTarget = !!hit && (hit === el || el.contains(hit));
    }
    return { attached: true, visible, enabled, hitTarget };
}"#;

/// Default Hamming tolerance for the coarse full-screen smoke check.
///
/// The old code allowed 10 bits (~15.6%) in `/browser/order` — wide enough
/// for two different white pages to "match". The smoke check is now advisory
/// only; the [`TargetCheck`] gate makes the real decision. Kept small (3)
/// so genuine page changes are noticed without caret-blink false alarms.
pub const SMOKE_MAX_HAMMING: u32 = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, ImageBuffer};
    use std::io::Cursor;

    /// Encode a grayscale image to PNG bytes (exercises the decode path).
    fn png_of(img: &GrayImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    /// Horizontal gradient: brightness increases left -> right.
    fn gradient(w: u32, h: u32) -> GrayImage {
        ImageBuffer::from_fn(w, h, |x, _| {
            image::Luma([((x as f32 / w as f32) * 255.0) as u8])
        })
    }

    #[test]
    fn dhash_is_deterministic() {
        let bytes = png_of(&gradient(64, 64));
        assert_eq!(dhash(&bytes), dhash(&bytes));
        // Decreasing gradient (right-to-left) has left>right transitions
        let dec = ImageBuffer::from_fn(64, 64, |x, _| {
            image::Luma([255 - ((x as f32 / 64.0) * 255.0) as u8])
        });
        let dec_bytes = png_of(&dec);
        assert_ne!(dhash(&dec_bytes), 0); // decreasing gradient has real transitions
    }

    #[test]
    fn dhash_detects_content_change() {
        // Decreasing gradient vs increasing gradient must hash differently.
        let inc = png_of(&gradient(64, 64));
        let dec_img: GrayImage = ImageBuffer::from_fn(64, 64, |x, _| {
            image::Luma([255 - ((x as f32 / 64.0) * 255.0) as u8])
        });
        let dec = png_of(&dec_img);
        assert_ne!(dhash(&inc), dhash(&dec));
    }

    #[test]
    fn dhash_ignores_uniform_images() {
        // A flat image has no left-right transitions: hash is 0 either way.
        // (Documents why uniform screenshots are a weak signal — the reason
        // the old 10-bit tolerance was dangerous.)
        let white = png_of(&GrayImage::from_pixel(64, 64, image::Luma([255])));
        let black = png_of(&GrayImage::from_pixel(64, 64, image::Luma([0])));
        assert_eq!(dhash(&white), 0);
        assert_eq!(dhash(&black), 0);
    }

    #[test]
    fn dhash_undecodable_input_yields_zero() {
        assert_eq!(dhash(b"not an image"), 0);
        assert_eq!(dhash(&[]), 0);
    }

    #[test]
    fn hamming_basics() {
        assert_eq!(hamming(0, 0), 0);
        assert_eq!(hamming(u64::MAX, u64::MAX), 0);
        assert_eq!(hamming(0, u64::MAX), 64);
        assert_eq!(hamming(0b101, 0b100), 1);
        assert_eq!(hamming(0b101, 0b010), 3);
    }

    #[test]
    fn verify_state_roundtrip() {
        let mut s = VerifyState::new();
        assert_eq!(s.get_baseline("tab1"), None);
        s.set_baseline("tab1", 0xDEAD_BEEF);
        assert_eq!(s.get_baseline("tab1"), Some(0xDEAD_BEEF));
        s.clear_baseline("tab1");
        assert_eq!(s.get_baseline("tab1"), None);
    }

    #[test]
    fn verify_state_check_tolerance() {
        let mut s = VerifyState::new();
        s.set_baseline("tab1", 0b1111_0000);
        // 1 bit differs -> within tolerance 3
        assert_eq!(s.check("tab1", 0b1111_0001, SMOKE_MAX_HAMMING), Some(true));
        // 4 bits differ -> outside tolerance 3
        assert_eq!(s.check("tab1", 0b0000_1111, SMOKE_MAX_HAMMING), Some(false));
    }

    #[test]
    fn verify_state_no_baseline_is_not_a_pass() {
        let s = VerifyState::new();
        // No baseline -> None, never Some(true). Callers must not act.
        assert_eq!(s.check("ghost-tab", 12345, SMOKE_MAX_HAMMING), None);
    }

    #[test]
    fn target_check_gate_logic() {
        let good = TargetCheck {
            attached: true,
            visible: true,
            enabled: true,
            hit_target: true,
        };
        assert!(good.ok());
        assert!(!good.blocked_by_overlay());

        // Modal covering the target: everything fine except hit_target.
        let covered = TargetCheck {
            hit_target: false,
            ..good
        };
        assert!(!covered.ok());
        assert!(covered.blocked_by_overlay());

        // Detached node is not an overlay problem — it's gone.
        let detached = TargetCheck {
            attached: false,
            visible: false,
            enabled: false,
            hit_target: false,
        };
        assert!(!detached.ok());
        assert!(!detached.blocked_by_overlay());
    }

    #[test]
    fn target_check_deserializes_from_js_shape() {
        // The object TARGET_CHECK_JS returns must map onto TargetCheck.
        let json = serde_json::json!({
            "attached": true, "visible": true,
            "enabled": false, "hitTarget": true,
        });
        let t: TargetCheck = serde_json::from_value(json).unwrap();
        assert!(!t.ok()); // disabled
        assert!(!t.blocked_by_overlay()); // not merely covered
    }

    #[test]
    fn target_check_js_checks_all_four_conditions() {
        for token in [
            "isConnected",
            "getBoundingClientRect",
            "getComputedStyle",
            "disabled",
            "aria-disabled",
            "elementFromPoint",
        ] {
            assert!(TARGET_CHECK_JS.contains(token), "missing {token}");
        }
        // Must not rely on offsetParent (null for position:fixed).
        assert!(!TARGET_CHECK_JS.contains("offsetParent"));
    }
}
