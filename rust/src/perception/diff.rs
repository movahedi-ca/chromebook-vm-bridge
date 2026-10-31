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
