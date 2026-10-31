//! Deterministic React-aware form interaction primitives.
//!
//! Modern enterprise portals (Workday, Greenhouse, React SPAs) override the
//! standard `HTMLInputElement.prototype.value` setter so input state lives in
//! React's virtual DOM fiber tree. Assigning `input.value` directly — or
//! dispatching keystrokes that miss an input mask — gets deduplicated by React,
//! which then overwrites the DOM with its stale internal state. These snippets
//! bypass that by invoking the *native* prototype setter and dispatching the
//! bubbling events React listens for, forcing synchronous reconciliation.
//!
//! All JS here is injected via CDP `Runtime.evaluate`; routes wire the calls.

/// Native-prototype value setter for React-controlled inputs.
///
/// Takes `(el, value)` and:
/// 1. Resolves the native `value` setter from `HTMLInputElement.prototype`
///    (or `HTMLTextAreaElement.prototype` for textareas — never the possibly
///    overridden instance setter).
/// 2. Calls it, then dispatches bubbling `input`, `change`, and `blur`
///    events so React reconciles the new state synchronously.
/// 3. Returns `el.value` so the caller can verify.
///
/// This is the exact primitive from the optimization doc; it replaces the
/// flaky keystroke-streaming that Workday silently discards.
pub const FILL_REACT_JS: &str = r#"(el, val) => {
    const proto = el instanceof window.HTMLTextAreaElement
        ? window.HTMLTextAreaElement.prototype
        : window.HTMLInputElement.prototype;
    const nativeSetter = Object.getOwnPropertyDescriptor(proto, 'value').set;
    nativeSetter.call(el, val);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    el.dispatchEvent(new FocusEvent('blur', { bubbles: true }));
    return el.value;
}"#;

/// Segmented date-picker detector (Workday-style ARIA spinbuttons).
///
/// Returns a JSON-serializable descriptor:
/// `{ kind: "spinbuttons", refs: [<index of Month>, <index of Day>, <index of Year>] }`
/// when segmented `[role="spinbutton"]` controls with aria-labels Month/Day/Year
/// are found inside the target container, else `{ kind: "direct" }`.
///
/// The caller then either types zero-padded numeric segments sequentially
/// (`09`, Tab, `01`, Tab, `2024`) into each spinbutton, or falls back to a
/// direct `FILL_REACT_JS` value set for plain text date inputs. This kills the
/// "MM/DD/YYYY strings rejected" failure class without brittle calendar-grid
/// clicking.
#[allow(dead_code)] // parked: fill-date endpoint is a 501 stub (audit MED-7)
pub const FILL_DATE_JS: &str = r#"(container) => {
    const root = container || document;
    const spins = Array.from(root.querySelectorAll('[role="spinbutton"]'));
    const find = (label) => spins.findIndex((s) =>
        (s.getAttribute('aria-label') || '').toLowerCase() === label);
    const m = find('month'), d = find('day'), y = find('year');
    if (m >= 0 && d >= 0 && y >= 0) {
        return { kind: 'spinbuttons', refs: [m, d, y] };
    }
    return { kind: 'direct' };
}"#;

/// Atomic paste for operational-transformation editors (Overleaf ShareDB, etc.).
///
/// Dispatches a single synthetic `ClipboardEvent('paste')` carrying a
/// `DataTransfer` payload on the target element, instead of streaming
/// thousands of individual keystrokes. Takes `(el, text)` and returns `true`
/// on dispatch. The 13 KB LaTeX paste that melted ShareDB in the Oct 30 field
/// run becomes one event.
///
/// Note: synthetic `ClipboardEvent` construction requires no clipboard
/// permissions; the payload travels in page memory via `DataTransfer`.
#[allow(dead_code)] // parked: atomic-paste endpoint is a 501 stub (audit MED-7)
pub const PASTE_ATOMIC_JS: &str = r#"(el, text) => {
    el.focus();
    const dt = new DataTransfer();
    dt.setData('text/plain', text);
    const evt = new ClipboardEvent('paste', {
        bubbles: true,
        cancelable: true,
        clipboardData: dt,
    });
    const cancelled = !el.dispatchEvent(evt);
    // Fallback for editors that only listen on document: re-dispatch there
    // if the element swallowed it without effect.
    if (cancelled) {
        document.dispatchEvent(new ClipboardEvent('paste', {
            bubbles: true,
            cancelable: true,
            clipboardData: dt,
        }));
    }
    return true;
}"#;

/// Verify a fill/type by comparing the target element's value before/after.
///
/// The legacy `type_verified` diffed `document.body.innerText.length`,
/// but `<input>`/`<textarea>` values are never reflected in `innerText` —
/// the delta was always zero and successful fills falsely reported
/// `text_not_found`. Compare the element's own value instead.
///
/// `before` / `after` are the `el.value ?? el.textContent` readings taken by
/// the route around the fill. Returns true when the value actually changed.
#[allow(dead_code)] // parked: verified-type endpoint is a 501 stub (audit MED-7)
pub fn type_verified_check(before: &str, after: &str) -> bool {
    before != after
}

/// JS snippet the route evaluates to read the verifiable value of a target:
/// `el.value` for form controls, `el.textContent` for contenteditable/others.
#[allow(dead_code)] // parked: verified-type endpoint is a 501 stub (audit MED-7)
pub const READ_TARGET_VALUE_JS: &str =
    r#"(el) => (el.value !== undefined ? el.value : (el.textContent || ''))"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_react_uses_native_prototype_setter() {
        assert!(FILL_REACT_JS.contains("getOwnPropertyDescriptor"));
        assert!(FILL_REACT_JS.contains("HTMLInputElement.prototype"));
        assert!(FILL_REACT_JS.contains("HTMLTextAreaElement.prototype"));
        assert!(FILL_REACT_JS.contains("bubbles: true"));
        assert!(FILL_REACT_JS.contains("return el.value"));
    }

    #[test]
    fn fill_date_detects_spinbuttons() {
        assert!(FILL_DATE_JS.contains(r#"[role="spinbutton"]"#));
        assert!(FILL_DATE_JS.contains("'month'"));
        assert!(FILL_DATE_JS.contains("'day'"));
        assert!(FILL_DATE_JS.contains("'year'"));
        assert!(FILL_DATE_JS.contains("'spinbuttons'"));
        assert!(FILL_DATE_JS.contains("'direct'"));
    }

    #[test]
    fn paste_atomic_dispatches_single_clipboard_event() {
        assert!(PASTE_ATOMIC_JS.contains("ClipboardEvent"));
        assert!(PASTE_ATOMIC_JS.contains("'paste'"));
        assert!(PASTE_ATOMIC_JS.contains("DataTransfer"));
        assert!(PASTE_ATOMIC_JS.contains("setData('text/plain'"));
    }

    #[test]
    fn type_verified_detects_value_change() {
        assert!(type_verified_check("", "hello"));
        assert!(type_verified_check("old", "new"));
        assert!(!type_verified_check("same", "same"));
        assert!(!type_verified_check("", ""));
    }

    #[test]
    fn type_verified_catches_react_revert() {
        // React silently reverting the DOM to stale state: value unchanged.
        assert!(!type_verified_check("stale", "stale"));
        // Whitespace-only differences still count as a change signal.
        assert!(type_verified_check("a", "a "));
    }
}
