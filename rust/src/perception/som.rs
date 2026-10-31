//! Set-of-Marks (SoM) visual grounding overlay (playbook §9).
//!
//! Modelled on the CAMEL Hybrid Browser Toolkit, WebVoyager, and PrivySe:
//! instead of asking a VLM to guess raw `(x, y)` pixel coordinates from an
//! unannotated screenshot (the old `move-ai` path that estimated coordinates
//! from a bare image), the bridge outlines every visible interactive element
//! with a high-contrast box and a numeric tag. The agent then targets integer
//! `[ref]` ids, the same refs as the AX snapshot in [`crate::perception`],
//! never pixels.
//!
//! Merging is structural, not geometric: marks are drawn per interactive
//! *element* from the snapshot node list, so a button's icon span plus its
//! text label collapse into a single `[N]` by construction. Per-fragment
//! approaches need geometric merging; this one does not.
//!
//! Memory and display discipline:
//! - The bridge captures the annotated screenshot **to disk** and returns
//!   the file path. Large image buffers are never held in Rust RAM.
//! - [`SOM_CLEANUP_JS`] removes the overlay synchronously (same frame), so
//!   the human watching display :1 sees zero visual clutter.
//! - The overlay container has `pointer-events: none` throughout: it can
//!   never steal clicks from the page or from a human on VNC.
//!
//! Calling convention (no chromiumoxide calls here, pure JS constants):
//! 1. `Runtime.evaluate("const __vm_som_nodes = <AX_SNAPSHOT_NODES_JSON>;")`
//! 2. `Runtime.evaluate("(" + SOM_INJECT_JS + ")(__vm_som_nodes)")`
//!    returns the number of marks drawn.
//! 3. `Page.captureScreenshot`, write bytes to disk, return the path.
//! 4. `Runtime.evaluate(SOM_CLEANUP_JS)` to strip the overlay.
//!
//! Tag consistency: [`SOM_INJECT_JS`] takes the exact node list produced by
//! the AX snapshot, so `[N]` tags always match the snapshot refs even if
//! the DOM changed between the two calls. Never re-collect inside the
//! inject step.

/// Injects the Set-of-Marks overlay for one AX snapshot node list.
///
/// A JS arrow-function expression string; invoke as
/// `(<SOM_INJECT_JS>)(<nodes JSON>)`. Each node needs `{ref, bounds}` with
/// `bounds = {x, y, w, h}` in top-viewport CSS pixels (the snapshot's
/// coordinate space). Returns the number of marks drawn.
///
/// - One mark per node: high-contrast magenta outline plus a `[N]` tag in
///   the box's top-left corner.
/// - Off-viewport and zero-area nodes are skipped.
/// - Re-invoking first removes any previous overlay (idempotent).
/// - The container is tagged `data-vm-internal="true"` so the DOM diff
///   observer ([`crate::perception::diff`]) ignores our own marks.
pub const SOM_INJECT_JS: &str = r##"((nodes) => {
  const ID = "__vm_som_overlay";
  const old = document.getElementById(ID);
  if (old) old.remove();
  const root = document.createElement("div");
  root.id = ID;
  root.setAttribute("data-vm-internal", "true");
  root.style.cssText = "position:fixed;inset:0;z-index:2147483647;" +
    "pointer-events:none;margin:0;padding:0;";
  let marks = 0;
  for (const n of nodes || []) {
    const b = n && n.bounds;
    if (!b || b.w < 1 || b.h < 1) continue;
    if (b.x + b.w < 0 || b.y + b.h < 0) continue;
    if (b.x > window.innerWidth || b.y > window.innerHeight) continue;
    const box = document.createElement("div");
    box.style.cssText = "position:absolute;left:" + b.x + "px;top:" + b.y + "px;" +
      "width:" + b.w + "px;height:" + b.h + "px;" +
      "border:2px solid #ff00ff;background:rgba(255,0,255,0.06);" +
      "box-sizing:border-box;pointer-events:none;margin:0;padding:0;";
    const tag = document.createElement("div");
    tag.style.cssText = "position:absolute;top:-2px;left:-2px;" +
      "background:#ff00ff;color:#ffffff;font:bold 11px/1.4 monospace;" +
      "padding:1px 5px;border-radius:2px;pointer-events:none;white-space:nowrap;";
    tag.textContent = "[" + n.ref + "]";
    box.appendChild(tag);
    root.appendChild(box);
    marks++;
  }
  document.documentElement.appendChild(root);
  return marks;
})"##;

/// Removes the Set-of-Marks overlay synchronously.
///
/// Returns 1 when an overlay was present, 0 otherwise. DOM removal is
/// synchronous, so the overlay is gone in the same frame: the watched
/// display never shows stale marks.
pub const SOM_CLEANUP_JS: &str = r##"(() => {
  const el = document.getElementById("__vm_som_overlay");
  if (el) { el.remove(); return 1; }
  return 0;
})()"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_and_cleanup_share_overlay_id() {
        assert!(SOM_INJECT_JS.contains("__vm_som_overlay"));
        assert!(SOM_CLEANUP_JS.contains("__vm_som_overlay"));
    }

    #[test]
    fn overlay_never_intercepts_pointer() {
        assert!(
            SOM_INJECT_JS.contains("pointer-events:none"),
            "overlay must not steal clicks from the page or VNC"
        );
    }

    #[test]
    fn inject_consumes_snapshot_node_list() {
        assert!(
            SOM_INJECT_JS.contains("nodes"),
            "inject must accept the [ref=N] node list for tag consistency"
        );
        assert!(
            SOM_INJECT_JS.contains("[\" + n.ref + \"]") || SOM_INJECT_JS.contains("\"[\" + n.ref"),
            "tags must render the snapshot ref"
        );
    }

    #[test]
    fn inject_marks_own_dom_as_internal() {
        assert!(
            SOM_INJECT_JS.contains("data-vm-internal"),
            "overlay must be excludable from DOM diffing"
        );
    }
}
