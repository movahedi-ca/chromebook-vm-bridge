//! Interactive accessibility snapshot with `[ref=N]` handles (playbook §9).
//!
//! Fixes vs. the legacy implementation (audit M3): the old `_ax_snapshot` filter,
//! `role in interesting or (name and role)`, was effectively a no-op.
//! Nearly every named DOM node (StaticText, generic, paragraph, image)
//! satisfied it, so the 200-node cap filled with static header and body
//! text in document order before reaching interactive buttons or inputs.
//! This module filters STRICTLY to interactive/form roles plus headings
//! for section context, skips `ignored` subtrees, and returns an explicit
//! `truncated` flag.
//!
//! Token budget: full-page text dumps cost ~165K tokens per multi-step
//! task; an interactive-only snapshot addressed by integer `[ref]` handles
//! targets <45K (playbook §9).
//!
//! Data flow (no chromiumoxide calls here, pure data + JS):
//! 1. The route handler evaluates [`AX_SNAPSHOT_JS`] via CDP
//!    `Runtime.evaluate`; it returns `{ nodes, truncated }`.
//! 2. For each node the handler resolves `backendDOMNodeId` (via
//!    `DOM.resolveNode`) and calls [`AxMapper::insert`].
//! 3. The JS assigns `ref` sequentially from 1 in document order; the
//!    handler inserts backend ids in the same order, so mapper refs match
//!    JS refs in the common 1:1 case. `insert` is idempotent, so duplicate
//!    nodes keep stable refs.
//! 4. [`AxMapper::clear`] runs on every navigation: refs are per-page-load
//!    and must never address nodes from a previous document.

use std::collections::HashMap;

pub mod diff;
pub mod som;

/// Roles that survive the interactive filter.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "searchbox",
    "combobox",
    "listbox",
    "option",
    "checkbox",
    "radio",
    "tab",
    "menuitem",
    "spinbutton",
    "slider",
];

/// Non-actionable roles kept for section context only.
const CONTEXT_ROLES: &[&str] = &["heading"];

/// Maximum nodes per snapshot (audit M3: cap plus explicit truncated flag).
pub const AX_SNAPSHOT_CAP: usize = 200;

/// Pure allowlist check over a set of candidate roles.
///
/// Returns `true` if **any** role in `roles` is interactive or a context
/// heading (case-insensitive, surrounding whitespace ignored). An empty
/// slice returns `false`.
///
/// This mirrors the role gate inside [`AX_SNAPSHOT_JS`]; keep the two in
/// sync. Rust-side code (tests, validators, future CDP
/// `Accessibility.getFullAXTree` consumers) must apply exactly the same
/// allowlist as the in-page script.
#[allow(dead_code)] // parked: used by the unwired SoM path (audit LOW-1)
pub fn filter_interactive(roles: &[&str]) -> bool {
    roles.iter().any(|role| {
        let lower = role.trim().to_ascii_lowercase();
        INTERACTIVE_ROLES.contains(&lower.as_str()) || CONTEXT_ROLES.contains(&lower.as_str())
    })
}

/// Maps `[ref=N]` handles (u32, dense from 1) to CDP `backendDOMNodeId` (i64).
///
/// Ref 0 is never assigned and resolves to `None`; treat it as "no ref".
/// Mappings are per page load: the route handler MUST call [`AxMapper::clear`]
/// on every navigation, otherwise a stale ref would address a node from a
/// previous document.
#[derive(Debug, Default)]
pub struct AxMapper {
    next_ref: u32,
    forward: HashMap<u32, i64>,
    reverse: HashMap<i64, u32>,
}

impl AxMapper {
    /// Empty mapper; the first [`AxMapper::insert`] returns ref 1.
    pub fn new() -> Self {
        Self {
            next_ref: 1,
            forward: HashMap::new(),
            reverse: HashMap::new(),
        }
    }

    /// Insert a backend node id and return its `[ref]`.
    ///
    /// Idempotent: inserting an already-mapped backend id returns the
    /// existing ref instead of allocating a new one, so nodes that appear
    /// twice in a snapshot (AX tree vs. DOM query overlap) keep one stable
    /// handle.
    pub fn insert(&mut self, backend_id: i64) -> u32 {
        if let Some(&r) = self.reverse.get(&backend_id) {
            return r;
        }
        let r = self.next_ref;
        self.next_ref += 1;
        self.forward.insert(r, backend_id);
        self.reverse.insert(backend_id, r);
        r
    }

    /// Resolve a `[ref]` to its `backendDOMNodeId`, or `None` when unknown
    /// (e.g. a ref from a previous page load after [`AxMapper::clear`]).
    #[allow(dead_code)] // parked: ref-resolution for the unwired MCP click/fill path
    pub fn resolve(&self, ref_id: u32) -> Option<i64> {
        self.forward.get(&ref_id).copied()
    }

    /// Drop all mappings. Call on navigation: refs never cross documents.
    pub fn clear(&mut self) {
        self.next_ref = 1;
        self.forward.clear();
        self.reverse.clear();
    }

    /// Number of mapped refs.
    #[allow(dead_code)] // part of the public mapper API; no caller yet
    pub fn len(&self) -> usize {
        self.forward.len()
    }

    /// True when no refs are mapped.
    #[allow(dead_code)] // part of the public mapper API; no caller yet
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }
}

/// In-page accessibility snapshot.
///
/// Evaluated via CDP `Runtime.evaluate`; returns
/// `{ nodes: [{ref, role, name, bounds}], truncated: bool }` where `ref`
/// is a dense 1-based integer in document order, `bounds` are CSS pixels
/// in top-viewport coordinates, and `truncated` is true when the
/// [`AX_SNAPSHOT_CAP`]-node cap was hit before the document was exhausted.
///
/// Design notes:
/// - Role allowlist mirrors [`filter_interactive`]: only interactive/form
///   roles plus headings. `role="presentation"`/`"none"` and
///   `aria-hidden="true"` subtrees are skipped (the DOM analogue of AX
///   `ignored: true`).
/// - Same-origin iframes are descended one level deep (Workday renders in
///   iframes); bounds are translated to top-viewport coordinates.
///   Cross-origin frames are skipped silently.
/// - Invisible elements (zero area, `display: none`, `visibility: hidden`)
///   are excluded. Hidden file inputs are intentionally out of scope here;
///   `/browser/upload` resolves those via CDP directly.
/// - Accessible name order: `aria-labelledby`, `aria-label`, associated
///   `<label>`, text content, then `placeholder`/`title`/`alt`/`value`.
#[allow(dead_code)] // parked: JS snapshot path superseded by the Rust AX walker (audit LOW-1)
pub const AX_SNAPSHOT_JS: &str = r##"(() => {
  const CAP = 200;
  // Mirrors perception::filter_interactive (Rust). Keep in sync.
  const INTERACTIVE = new Set([
    "button", "link", "textbox", "searchbox", "combobox",
    "listbox", "option", "checkbox", "radio", "tab",
    "menuitem", "spinbutton", "slider"
  ]);
  const CONTEXT = new Set(["heading"]);
  function allowed(role) {
    return INTERACTIVE.has(role) || CONTEXT.has(role);
  }

  function implicitRole(el) {
    const tag = el.tagName.toLowerCase();
    switch (tag) {
      case "button": return "button";
      case "a": return el.hasAttribute("href") ? "link" : "";
      case "textarea": return "textbox";
      case "select": return el.multiple ? "listbox" : "combobox";
      case "option": return "option";
      case "summary": return "button";
      case "h1": case "h2": case "h3":
      case "h4": case "h5": case "h6": return "heading";
      case "input": {
        const t = (el.getAttribute("type") || "text").toLowerCase();
        if (t === "hidden") return "";
        if (t === "checkbox") return "checkbox";
        if (t === "radio") return "radio";
        if (t === "range") return "slider";
        if (t === "search") return "searchbox";
        if (t === "number") return "spinbutton";
        if (t === "submit" || t === "button" || t === "reset" || t === "image") return "button";
        return "textbox";
      }
      default: return "";
    }
  }

  function roleOf(el) {
    const explicit = (el.getAttribute("role") || "").trim().toLowerCase().split(/\s+/)[0];
    if (explicit === "presentation" || explicit === "none") return "";
    return explicit || implicitRole(el);
  }

  function visible(el) {
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return false;
    const cs = getComputedStyle(el);
    return cs.display !== "none" && cs.visibility !== "hidden" && cs.visibility !== "collapse";
  }

  function accName(el, doc) {
    const lb = el.getAttribute("aria-labelledby");
    if (lb) {
      const txt = lb.split(/\s+/).map(id => {
        const n = doc.getElementById(id);
        return n ? n.textContent : "";
      }).join(" ").replace(/\s+/g, " ").trim();
      if (txt) return txt.slice(0, 120);
    }
    const al = (el.getAttribute("aria-label") || "").trim();
    if (al) return al.slice(0, 120);
    if (el.id) {
      try {
        const lab = doc.querySelector('label[for="' + CSS.escape(el.id) + '"]');
        if (lab) {
          const t = lab.textContent.replace(/\s+/g, " ").trim();
          if (t) return t.slice(0, 120);
        }
      } catch (e) { /* malformed id: fall through */ }
    }
    const t = el.textContent.replace(/\s+/g, " ").trim();
    if (t) return t.slice(0, 120);
    const fb = el.getAttribute("placeholder") || el.getAttribute("title")
      || el.getAttribute("alt") || el.getAttribute("value") || "";
    return String(fb).trim().slice(0, 120);
  }

  const SEL = "a[href],button,input,select,textarea,[role],summary,h1,h2,h3,h4,h5,h6";

  // Same-origin frames, one level deep. Bounds are translated to
  // top-viewport coordinates so snapshot, SoM overlay, and click
  // coordinates share one space.
  function roots() {
    const out = [{ doc: document, dx: 0, dy: 0 }];
    for (const f of document.querySelectorAll("iframe")) {
      try {
        const d = f.contentDocument;
        if (!d) continue;
        const r = f.getBoundingClientRect();
        out.push({ doc: d, dx: Math.round(r.x), dy: Math.round(r.y) });
      } catch (e) { /* cross-origin: skip */ }
    }
    return out;
  }

  const nodes = [];
  let truncated = false;
  const seen = new Set();
  outer:
  for (const { doc, dx, dy } of roots()) {
    let q;
    try { q = doc.querySelectorAll(SEL); } catch (e) { continue; }
    for (const el of q) {
      if (nodes.length >= CAP) { truncated = true; break outer; }
      if (seen.has(el)) continue;
      seen.add(el);
      if (el.closest('[aria-hidden="true"]')) continue;
      const role = roleOf(el);
      if (!allowed(role)) continue;
      if (!visible(el)) continue;
      const r = el.getBoundingClientRect();
      nodes.push({
        ref: nodes.length + 1,
        role: role,
        name: accName(el, doc),
        bounds: {
          x: Math.round(r.x) + dx,
          y: Math.round(r.y) + dy,
          w: Math.round(r.width),
          h: Math.round(r.height)
        }
      });
    }
  }
  return { nodes: nodes, truncated: truncated };
})()"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_accepts_interactive_roles_and_headings() {
        for role in [
            "button",
            "link",
            "textbox",
            "searchbox",
            "combobox",
            "listbox",
            "option",
            "checkbox",
            "radio",
            "tab",
            "menuitem",
            "spinbutton",
            "slider",
            "heading",
        ] {
            assert!(filter_interactive(&[role]), "role '{role}' should pass");
        }
    }

    #[test]
    fn allowlist_rejects_static_roles() {
        // Audit M3: these node kinds filled the old 200-node cap in document
        // order before any interactive control was reached.
        for role in [
            "div",
            "generic",
            "paragraph",
            "StaticText",
            "image",
            "banner",
            "",
        ] {
            assert!(
                !filter_interactive(&[role]),
                "role '{role}' should be rejected"
            );
        }
        assert!(!filter_interactive(&[]), "empty slice must not pass");
    }

    #[test]
    fn allowlist_is_case_insensitive_and_trims() {
        assert!(filter_interactive(&["BUTTON"]));
        assert!(filter_interactive(&["  link  "]));
        assert!(filter_interactive(&["CheckBox"]));
    }

    #[test]
    fn allowlist_uses_any_semantics() {
        assert!(filter_interactive(&["div", "link"]));
        assert!(!filter_interactive(&["div", "span"]));
    }

    #[test]
    fn mapper_assigns_dense_refs_from_one() {
        let mut m = AxMapper::new();
        assert_eq!(m.insert(101), 1);
        assert_eq!(m.insert(202), 2);
        assert_eq!(m.insert(303), 3);
        assert_eq!(m.len(), 3);
        assert!(!m.is_empty());
    }

    #[test]
    fn mapper_insert_is_idempotent() {
        let mut m = AxMapper::new();
        assert_eq!(m.insert(101), 1);
        assert_eq!(m.insert(101), 1, "re-insert must return the existing ref");
        assert_eq!(m.len(), 1, "no duplicate mapping may be created");
    }

    #[test]
    fn mapper_resolve_roundtrip() {
        let mut m = AxMapper::new();
        m.insert(101);
        m.insert(202);
        assert_eq!(m.resolve(1), Some(101));
        assert_eq!(m.resolve(2), Some(202));
        assert_eq!(m.resolve(99), None);
        assert_eq!(m.resolve(0), None, "ref 0 is reserved as no-ref");
    }

    #[test]
    fn mapper_clear_resets_for_next_page_load() {
        let mut m = AxMapper::new();
        m.insert(101);
        m.clear();
        assert!(m.is_empty());
        assert_eq!(m.resolve(1), None, "stale refs must not survive navigation");
        assert_eq!(m.insert(202), 1, "refs restart at 1 per page load");
    }

    #[test]
    fn snapshot_js_has_required_shape() {
        assert!(
            AX_SNAPSHOT_JS.contains("truncated"),
            "must return truncated flag"
        );
        assert!(
            AX_SNAPSHOT_JS.contains("aria-hidden"),
            "must skip ignored subtrees"
        );
        assert!(AX_SNAPSHOT_JS.contains("CAP"), "must enforce the node cap");
        assert!(
            AX_SNAPSHOT_JS.contains("iframe"),
            "must descend into frames"
        );
    }
}
