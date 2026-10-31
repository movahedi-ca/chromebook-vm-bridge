//! `MutationObserver` DOM diffing (playbook §9).
//!
//! Replaces redundant full-page snapshots after every action. The install
//! script runs on every page load (via CDP `Page.addScriptToEvaluateOnNewDocument`);
//! it queues semantic change records while the agent acts. After an action
//! completes, the handler evaluates [`DIFF_FLUSH_JS`], which drains the queue
//! and returns only what changed: newly appeared autocomplete suggestions,
//! inline validation errors, modal dialogs. Expected per-step token overhead
//! reduction: 65-80% vs. re-snapshotting the page.
//!
//! Queue discipline: the in-page queue is capped (oldest records dropped on
//! pathological pages) and consecutive duplicate records are collapsed, so
//! one attribute flapping in a loop cannot flood the agent.
//!
//! `ref_hint` is advisory, not a stable handle: an element `id` (`#foo`)
//! when present, otherwise a compact `tag:nth-of-type` path. The agent uses
//! it to find the node in the next AX snapshot, then addresses it by `[ref]`.
//! Elements tagged `data-vm-internal="true"` (`SoM` overlay, Co-Pilot HUD) are
//! never recorded.

#![allow(dead_code)] // parked: DOM-diff observer not yet wired to any route (audit LOW-1)
use serde::Deserialize;

/// One changed node in a DOM diff.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DiffNode {
    /// Advisory locator: `#id` when present, else a compact CSS path.
    #[serde(default)]
    pub ref_hint: String,
    /// Interactive role (`button`, `textbox`, ...) or `""` when unknown.
    #[serde(default)]
    pub role: String,
    /// Accessible name or text, truncated in-page.
    #[serde(default)]
    pub name: String,
}

/// Semantic DOM diff flushed after an action.
///
/// Buckets mirror the `MutationObserver` record types; the per-record `type`
/// field is stripped at flush time because the bucket already carries it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct DomDiff {
    /// Nodes added to the document during the last action.
    #[serde(default)]
    pub added: Vec<DiffNode>,
    /// Nodes removed from the document during the last action.
    #[serde(default)]
    pub removed: Vec<DiffNode>,
    /// Nodes whose meaningful attributes or text changed.
    #[serde(default)]
    pub mutated: Vec<DiffNode>,
}

impl DomDiff {
    /// Total changed nodes across all buckets.
    pub fn len(&self) -> usize {
        self.added.len() + self.removed.len() + self.mutated.len()
    }

    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Installs the `MutationObserver` + attribute watcher.
///
/// Evaluated on every page load via `Page.addScriptToEvaluateOnNewDocument`.
/// Idempotent per document (re-install returns `false`). Starts observing
/// immediately, or on `DOMContentLoaded` when the document is still loading.
/// Returns `true` on first install.
///
/// Observed: `childList` (added/removed nodes, subtree-wide), meaningful
/// attribute changes (`class`, `disabled`, `aria-*`, `hidden`, `open`,
/// `value`), and character data (validation messages appearing as text).
/// Records are `{type, ref_hint, role, name}` queued on
/// `window.__vm_diff_queue`.
pub const DIFF_INSTALL_JS: &str = r##"(() => {
  if (window.__vm_diff_installed) return false;
  window.__vm_diff_installed = true;
  const Q = (window.__vm_diff_queue = []);
  const QCAP = 500;

  function roleOf(el) {
    const explicit = (el.getAttribute("role") || "").trim().toLowerCase().split(/\s+/)[0];
    if (explicit && explicit !== "presentation" && explicit !== "none") return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === "button") return "button";
    if (tag === "a" && el.hasAttribute("href")) return "link";
    if (tag === "input") {
      const t = (el.getAttribute("type") || "text").toLowerCase();
      if (t === "checkbox") return "checkbox";
      if (t === "radio") return "radio";
      return "textbox";
    }
    if (tag === "textarea" || tag === "select") return "textbox";
    return "";
  }

  function nameOf(el) {
    const al = (el.getAttribute("aria-label") || "").trim();
    if (al) return al.slice(0, 80);
    const t = (el.textContent || "").replace(/\s+/g, " ").trim();
    return t.slice(0, 80);
  }

  function refHint(el) {
    if (el.id) return "#" + el.id;
    const parts = [];
    let n = el;
    for (let i = 0; i < 4 && n && n !== document.documentElement; i++) {
      let seg = n.tagName.toLowerCase();
      const p = n.parentElement;
      if (p) {
        const sibs = Array.prototype.filter.call(
          p.children, c => c.tagName === n.tagName);
        if (sibs.length > 1) seg += ":nth-of-type(" + (sibs.indexOf(n) + 1) + ")";
      }
      parts.unshift(seg);
      n = p;
    }
    return parts.join(" > ");
  }

  function targetEl(t) {
    if (t instanceof Element) return t;
    if (t instanceof Text && t.parentElement) return t.parentElement;
    return null;
  }

  function push(type, target) {
    const el = targetEl(target);
    if (!el || el === document.documentElement) return;
    if (el.closest('[data-vm-internal="true"]')) return;
    if (el.closest('[aria-hidden="true"]')) return;
    if (Q.length >= QCAP) Q.shift();
    const rec = { type: type, ref_hint: refHint(el), role: roleOf(el), name: nameOf(el) };
    const last = Q[Q.length - 1];
    if (last && last.type === rec.type && last.ref_hint === rec.ref_hint
        && last.role === rec.role && last.name === rec.name) return;
    Q.push(rec);
  }

  const ATTRS = ["class", "disabled", "aria-disabled", "aria-expanded",
    "aria-selected", "aria-checked", "hidden", "open", "value"];
  const mo = new MutationObserver((muts) => {
    for (const m of muts) {
      if (m.type === "childList") {
        for (const n of m.addedNodes) push("added", n);
        for (const n of m.removedNodes) push("removed", n);
      } else {
        push("mutated", m.target);
      }
    }
  });

  function start() {
    const root = document.documentElement;
    if (!root || window.__vm_diff_observing) return;
    window.__vm_diff_observing = true;
    mo.observe(root, {
      childList: true, subtree: true,
      attributes: true, attributeFilter: ATTRS,
      characterData: true
    });
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start, { once: true });
  } else {
    start();
  }
  return true;
})()"##;

/// Drains the diff queue and returns bucketed records.
///
/// Returns `{added: [...], removed: [...], mutated: [...]}` where each entry
/// is `{ref_hint, role, name}` (the per-record `type` is stripped; the bucket
/// carries it). The queue is cleared atomically by the drain, so records are
/// never double-reported. Evaluated after each action via
/// `Runtime.evaluate`; deserialize into [`DomDiff`].
pub const DIFF_FLUSH_JS: &str = r#"(() => {
  const q = window.__vm_diff_queue || [];
  window.__vm_diff_queue = [];
  const out = { added: [], removed: [], mutated: [] };
  for (const r of q) {
    const bucket = out[r.type];
    if (!bucket) continue;
    bucket.push({ ref_hint: r.ref_hint || "", role: r.role || "", name: r.name || "" });
  }
  return out;
})()"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_full_diff() {
        let json = r##"{
            "added": [{"ref_hint": "#submit", "role": "button", "name": "Submit"}],
            "removed": [],
            "mutated": [{"ref_hint": "input:nth-of-type(2)", "role": "textbox", "name": "Email"}]
        }"##;
        let d: DomDiff = serde_json::from_str(json).unwrap();
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].ref_hint, "#submit");
        assert_eq!(d.added[0].role, "button");
        assert_eq!(d.mutated.len(), 1);
        assert_eq!(d.mutated[0].name, "Email");
        assert!(d.removed.is_empty());
        assert!(!d.is_empty());
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn deserialize_tolerates_missing_buckets_and_extra_fields() {
        let d: DomDiff = serde_json::from_str("{}").unwrap();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
        // Forward compatibility: unknown fields from future JS are ignored.
        let d: DomDiff = serde_json::from_str(r#"{"added": [], "unknown_future": 1}"#).unwrap();
        assert!(d.is_empty());
    }

    #[test]
    fn diff_node_fields_default_when_absent() {
        let n: DiffNode = serde_json::from_str("{}").unwrap();
        assert_eq!(n.ref_hint, "");
        assert_eq!(n.role, "");
        assert_eq!(n.name, "");
    }

    #[test]
    fn install_js_defines_queue_and_observer() {
        assert!(DIFF_INSTALL_JS.contains("__vm_diff_queue"));
        assert!(DIFF_INSTALL_JS.contains("MutationObserver"));
        assert!(DIFF_INSTALL_JS.contains("data-vm-internal"));
    }

    #[test]
    fn flush_js_drains_and_buckets() {
        assert!(DIFF_FLUSH_JS.contains("__vm_diff_queue"));
        assert!(DIFF_FLUSH_JS.contains("added"));
        assert!(DIFF_FLUSH_JS.contains("removed"));
        assert!(DIFF_FLUSH_JS.contains("mutated"));
    }
}

// ---------------------------------------------------------------------------
// Unified PNG decode / pixel-diff / dHash pipeline (visual routes).
//
// Previously `screenshot_compare` and `screenshot_delta` in
// `routes/advanced.rs` each decoded the current frame twice (once for the
// diff, once for the dHash/patch) and cloned the whole PNG `Vec<u8>` to
// move it into a second `spawn_blocking`. `compare_frames` below does one
// decode of each input, one row-major diff, one in-memory hash, and one
// optional patch crop inside a SINGLE `spawn_blocking` closure.
//
// Score semantics are unchanged: `score` is 1.0 for identical frames
// (`is_match = score >= 0.99` in the routes), `bbox` is inclusive-exclusive.

/// Pixel-count cap (audit 42 MED-6): a crafted PNG can declare enormous
/// dimensions (decompression bomb). Read the header only and refuse
/// anything over 64 MP before allocating the bitmap.
pub const MAX_PIXELS: u64 = 64_000_000;

/// Decode a PNG with the pixel cap enforced. Single decode site for the
/// visual pipeline; the 64 MP header check replaces ad-hoc per-route caps.
pub fn decode_png_capped(png: &[u8]) -> anyhow::Result<image::RgbImage> {
    let dims = image::ImageReader::new(std::io::Cursor::new(png))
        .with_guessed_format()
        .map_err(|e| anyhow::anyhow!("png header: {e}"))?
        .into_dimensions()
        .map_err(|e| anyhow::anyhow!("png dimensions: {e}"))?;
    let pixels = u64::from(dims.0).saturating_mul(u64::from(dims.1));
    if pixels > MAX_PIXELS {
        anyhow::bail!("image exceeds pixel cap ({pixels} > {MAX_PIXELS})");
    }
    image::load_from_memory(png)
        .map_err(|e| anyhow::anyhow!("png decode: {e}"))
        .map(|img| img.to_rgb8())
}

/// dHash from an in-memory frame — no re-decode. Bit-exact with the
/// historical `browser::verify::dhash` for RGB input: 9x8 Nearest luma,
/// LSB-first bit order (`hash |= 1 << (y * 8 + x)`). Both now share this
/// implementation, so `/browser/screenshot/hash` and `screenshot_compare`
/// can never disagree on the same frame (audit 41 MED-3).
pub fn dhash_rgb(img: &image::RgbImage) -> u64 {
    let gray = image::imageops::grayscale(img);
    let small = image::imageops::resize(&gray, 9, 8, image::imageops::FilterType::Nearest);
    let mut hash = 0u64;
    for y in 0..8u32 {
        for x in 0..8u32 {
            if small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0] {
                hash |= 1 << (y * 8 + x);
            }
        }
    }
    hash
}

/// Encode an `RgbImage` as PNG bytes.
pub fn png_bytes(img: &image::RgbImage) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgb8(img.clone())
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| anyhow::anyhow!("PNG encode failed: {e}"))?;
    Ok(buf)
}

/// Row-major pixel diff with a `memcmp` fast path.
///
/// A pixel counts as changed when its maximum per-channel absolute
/// difference exceeds `threshold`. Rows are compared with SIMD-accelerated
/// slice equality first, so unchanged scanlines are skipped at
/// memory-bandwidth speed; only changed rows pay the per-pixel loop.
/// Diff outcome: `(changed_pixels, total_pixels, bbox)` with the bbox in
/// inclusive-exclusive `(x0, y0, x1, y1)` form.
type RgbDiffOutcome = (u64, u64, Option<(u32, u32, u32, u32)>);

/// Returns `(changed_pixels, total_pixels, bbox)` with the bbox in
/// inclusive-exclusive `(x0, y0, x1, y1)` form. Both images must have
/// identical dimensions (the caller resizes the reference).
fn diff_rgb_buffers(
    cur: &image::RgbImage,
    reference: &image::RgbImage,
    threshold: u8,
) -> RgbDiffOutcome {
    let (w, h) = cur.dimensions();
    debug_assert_eq!((w, h), reference.dimensions());
    if w == 0 || h == 0 {
        return (0, 0, None);
    }
    let row_stride = (w as usize) * 3;
    let cur_raw = cur.as_raw();
    let ref_raw = reference.as_raw();
    let mut changed = 0u64;
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for (y, (cur_row, ref_row)) in cur_raw
        .chunks_exact(row_stride)
        .zip(ref_raw.chunks_exact(row_stride))
        .enumerate()
    {
        // Fast path: identical rows never enter the pixel loop.
        if cur_row == ref_row {
            continue;
        }
        let mut row_changed = false;
        for (x, (c_px, r_px)) in cur_row
            .chunks_exact(3)
            .zip(ref_row.chunks_exact(3))
            .enumerate()
        {
            let d = c_px[0]
                .abs_diff(r_px[0])
                .max(c_px[1].abs_diff(r_px[1]))
                .max(c_px[2].abs_diff(r_px[2]));
            if d > threshold {
                changed += 1;
                row_changed = true;
                let x_u32 = x as u32;
                if x_u32 < min_x {
                    min_x = x_u32;
                }
                if x_u32 + 1 > max_x {
                    max_x = x_u32 + 1;
                }
            }
        }
        if row_changed {
            let y_u32 = y as u32;
            if y_u32 < min_y {
                min_y = y_u32;
            }
            if y_u32 + 1 > max_y {
                max_y = y_u32 + 1;
            }
        }
    }
    let bbox = if changed > 0 {
        Some((min_x, min_y, max_x, max_y))
    } else {
        None
    };
    (changed, u64::from(w) * u64::from(h), bbox)
}

/// One-shot visual comparison: decode each input ONCE, diff, hash the
/// current frame from memory, and optionally crop the changed patch.
///
/// Replaces the old split flow (diff in one `spawn_blocking`, dHash in a
/// second, patch crop re-decoding the frame a third time). `want_patch`
/// pads the bbox by 4 px and PNG-encodes only the changed region;
/// `patch_png` is empty when nothing changed or `want_patch` is false.
pub struct FrameDiff {
    /// 1.0 = identical frames (routes use `score >= 0.99` as "match").
    pub score: f64,
    pub changed: u64,
    pub total: u64,
    /// Inclusive-exclusive `(x0, y0, x1, y1)`; `None` when identical.
    pub bbox: Option<(u32, u32, u32, u32)>,
    /// dHash of the CURRENT frame, from the in-memory bitmap.
    pub hash: u64,
    /// PNG bytes of the 4 px-padded changed region; empty if none.
    pub patch_png: Vec<u8>,
}

pub fn compare_frames(
    current_png: &[u8],
    reference_png: &[u8],
    threshold: u8,
    want_patch: bool,
) -> anyhow::Result<FrameDiff> {
    let cur = decode_png_capped(current_png)
        .map_err(|e| anyhow::anyhow!("cannot decode current screenshot: {e}"))?;
    let mut rf = decode_png_capped(reference_png)
        .map_err(|e| anyhow::anyhow!("cannot decode reference image: {e}"))?;
    if rf.dimensions() != cur.dimensions() {
        rf = image::imageops::resize(
            &rf,
            cur.width(),
            cur.height(),
            image::imageops::FilterType::Triangle,
        );
    }
    let (changed, total, bbox) = diff_rgb_buffers(&cur, &rf, threshold);
    let score = if total == 0 {
        1.0
    } else {
        1.0 - changed as f64 / total as f64
    };
    let hash = dhash_rgb(&cur);
    let patch_png = if want_patch {
        match bbox {
            None => Vec::new(),
            Some((x0, y0, x1, y1)) => {
                let (w, h) = cur.dimensions();
                let px0 = x0.saturating_sub(4);
                let py0 = y0.saturating_sub(4);
                let px1 = (x1 + 4).min(w);
                let py1 = (y1 + 4).min(h);
                if px1 <= px0 || py1 <= py0 {
                    Vec::new()
                } else {
                    let patch =
                        image::imageops::crop_imm(&cur, px0, py0, px1 - px0, py1 - py0).to_image();
                    png_bytes(&patch)?
                }
            }
        }
    } else {
        Vec::new()
    };
    Ok(FrameDiff {
        score,
        changed,
        total,
        bbox,
        hash,
        patch_png,
    })
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    fn solid_png(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([r, g, b]));
        png_bytes(&img).unwrap()
    }

    #[test]
    fn pipeline_identical_scores_one() {
        let a = solid_png(8, 8, 200, 200, 200);
        let d = compare_frames(&a, &a, 10, false).unwrap();
        assert!((d.score - 1.0).abs() < f64::EPSILON);
        assert_eq!(d.changed, 0);
        assert!(d.bbox.is_none());
        assert!(d.patch_png.is_empty());
        assert_eq!(d.hash, dhash_rgb(&decode_png_capped(&a).unwrap()));
    }

    #[test]
    fn pipeline_finds_changed_bbox_and_patch() {
        let a = solid_png(8, 8, 200, 200, 200);
        let mut img = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 200, 200]));
        img.put_pixel(2, 3, image::Rgb([0, 0, 0]));
        let b = png_bytes(&img).unwrap();
        let d = compare_frames(&b, &a, 10, true).unwrap();
        assert_eq!(d.changed, 1);
        assert_eq!(d.bbox, Some((2, 3, 3, 4)));
        assert!(d.score < 1.0 && d.score > 0.9);
        assert!(!d.patch_png.is_empty());
    }

    #[test]
    fn pipeline_resizes_mismatched_reference() {
        let a = solid_png(8, 8, 200, 200, 200);
        let b = solid_png(4, 4, 200, 200, 200);
        let d = compare_frames(&a, &b, 10, false).unwrap();
        assert!((d.score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pipeline_rejects_undecodable() {
        let a = solid_png(8, 8, 200, 200, 200);
        assert!(compare_frames(&a, b"not png", 10, false).is_err());
        assert!(compare_frames(b"not png", &a, 10, false).is_err());
    }

    #[test]
    fn dhash_deterministic_on_rgb() {
        let a = solid_png(16, 16, 200, 200, 200);
        let img = decode_png_capped(&a).unwrap();
        assert_eq!(dhash_rgb(&img), dhash_rgb(&img));
    }
}
