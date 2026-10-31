//! Fixed autocomplete for stubborn enterprise widgets (Workday multiselect).
//!
//! ## The 4-step Workday sequence
//!
//! Discovered after 37 minutes of troubleshooting on Oct 30: Workday's
//! multiselect only cooperates with this exact interaction order:
//!
//! 1. **Focus** the multiselect input (real click).
//! 2. **Type** the search string (the caller types; human-like delays live in
//!    the route, not here).
//! 3. **Press Enter** to trigger Workday's backend query.
//! 4. **Wait** for `[role="option"]` items to render, **click the EXACT text
//!    match**, then dispatch a neutral **outside-click / blur** to commit the
//!    selection chip.
//!
//! [`AUTOCOMPLETE_JS`] implements steps 4's DOM half as an injectable helper
//! so routes stay thin: it collects `[role="option"]` texts, and the Rust side
//! picks the target with [`exact_match`] (never in the page, where a
//! compromised DOM could lie about what matched).
//!
//! ## Audit H9 — the two defects this fixes
//!
//! The legacy implementation had two silent-corruption bugs:
//! 1. **Bidirectional substring**: `query in option or option in query` meant
//!    typing "No" matched "North". Fixed: [`exact_match`] is case-insensitive
//!    *exact* equality only.
//! 2. **Silent `options[0]` fallback**: when nothing matched, it clicked the
//!    first arbitrary row and returned `ok: true`. Fixed: return `None`; the
//!    route reports the available options list and fails loudly.

/// DOM helper for the option-collection half of step 4.
///
/// Returns an array of `{ index, text }` for every visible `[role="option"]`
/// inside the nearest `[role="listbox"]` (or document). The Rust caller runs
/// [`exact_match`] over the texts and clicks the winner by index — matching
/// happens outside the page so injected DOM text cannot spoof it.
pub const AUTOCOMPLETE_JS: &str = r#"() => {
    const listbox = document.querySelector('[role="listbox"]') || document;
    const opts = Array.from(listbox.querySelectorAll('[role="option"]'));
    return opts
        .filter((o) => {
            const r = o.getBoundingClientRect();
            return r.width > 0 && r.height > 0;
        })
        .map((o, index) => ({ index, text: (o.innerText || '').trim() }));
}"#;

/// JS helper for the commit half of step 4: neutral outside-click + blur.
///
/// Takes `(inputEl)`; clicks a safe neutral point (document body top-left
/// corner, which Workday treats as "commit") and blurs the input so the
/// selection chip finalizes instead of lingering as a pending token.
pub const AUTOCOMPLETE_COMMIT_JS: &str = r#"(inputEl) => {
    const x = 8, y = 8;
    const el = document.elementFromPoint(x, y) || document.body;
    for (const type of ['mousedown', 'mouseup', 'click']) {
        el.dispatchEvent(new MouseEvent(type, {
            bubbles: true, cancelable: true, clientX: x, clientY: y,
        }));
    }
    if (inputEl) inputEl.blur();
    return true;
}"#;

/// Case-insensitive EXACT match over option texts.
///
/// Returns the first option whose trimmed text equals the trimmed query
/// ignoring ASCII case. This is deliberately *not* substring, prefix, or
/// fuzzy matching:
///
/// - Audit H9: `"No"` must NOT match `"North"` (bidirectional substring bug).
/// - Audit H9: there is NO `options[0]` fallback. `None` means "no exact
///   match" and the route must surface the candidate list instead of
///   clicking an arbitrary row and reporting success.
///
/// Whitespace is trimmed on both sides; comparison is Unicode-aware
/// case-insensitive via `to_lowercase`.
pub fn exact_match<'a>(query: &str, options: &[&'a str]) -> Option<&'a str> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    options
        .iter()
        .find(|o| o.trim().to_lowercase() == q)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_finds_case_insensitive() {
        let opts = ["Data Protection", "Privacy Engineering", "North"];
        assert_eq!(
            exact_match("privacy engineering", &opts),
            Some("Privacy Engineering")
        );
        assert_eq!(
            exact_match("DATA PROTECTION", &opts),
            Some("Data Protection")
        );
        assert_eq!(exact_match("  north  ", &opts), Some("North"));
    }

    #[test]
    fn exact_match_rejects_substring_h9() {
        // The audit's headline case: "No" must not match "North".
        let opts = ["North", "Northern Territory", "Norway"];
        assert_eq!(exact_match("No", &opts), None);
        assert_eq!(exact_match("Nor", &opts), None);
        // And the reverse direction: option must not be a substring of query.
        let opts2 = ["No"];
        assert_eq!(exact_match("North", &opts2), None);
    }

    #[test]
    fn exact_match_returns_none_on_no_match() {
        // NO options[0] fallback: silence here, loud error at the route.
        let opts = ["Apple", "Banana", "Cherry"];
        assert_eq!(exact_match("Durian", &opts), None);
        assert_eq!(exact_match("Durian", &[]), None);
    }

    #[test]
    fn exact_match_rejects_empty_query() {
        let opts = ["", "  ", "Apple"];
        assert_eq!(exact_match("", &opts), None);
        assert_eq!(exact_match("   ", &opts), None);
    }

    #[test]
    fn exact_match_trims_both_sides() {
        let opts = ["  Padded Option  "];
        assert_eq!(
            exact_match("padded option", &opts),
            Some("  Padded Option  ")
        );
    }

    #[test]
    fn exact_match_first_exact_wins() {
        let opts = ["dup", "Dup", "DUP"];
        assert_eq!(exact_match("dup", &opts), Some("dup"));
    }

    #[test]
    fn autocomplete_js_collects_options() {
        assert!(AUTOCOMPLETE_JS.contains(r#"[role="option"]"#));
        assert!(AUTOCOMPLETE_JS.contains(r#"[role="listbox"]"#));
        assert!(AUTOCOMPLETE_JS.contains("innerText"));
        assert!(AUTOCOMPLETE_JS.contains("getBoundingClientRect"));
    }

    #[test]
    fn commit_js_clicks_neutral_point_and_blurs() {
        assert!(AUTOCOMPLETE_COMMIT_JS.contains("elementFromPoint"));
        assert!(AUTOCOMPLETE_COMMIT_JS.contains("blur()"));
        assert!(AUTOCOMPLETE_COMMIT_JS.contains("'click'"));
    }
}
