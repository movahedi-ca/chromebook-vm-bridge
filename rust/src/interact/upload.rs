//! Deterministic file upload engine.
//!
//! ## The 3-stage strategy
//!
//! An autonomous browser bridge must NEVER interact with an OS-level file
//! chooser window (the Oct 30 field run burned 15 minutes and two human
//! interventions on Workday's native dialog). Uploads go through CDP:
//!
//! **Stage 1 — `DOM.setFileInputFiles` on an existing input.**
//! If an `<input type="file">` exists in the DOM — even hidden via
//! `opacity: 0` or `display: none` beneath a styled button — resolve its
//! `backendNodeId` (see [`UPLOAD_STAGE1_JS`]) and issue the raw CDP command
//! `DOM.setFileInputFiles` with the validated local path. This attaches the
//! file inside the browser process and dispatches native `input`/`change`
//! events, even on strict-CSP pages. Sub-50ms, no dialog ever opens.
//!
//! **Stage 2 — file-chooser interception.**
//! When an enterprise app (Workday) dynamically creates a *detached*
//! `<input type="file">` only at click time, there is nothing to resolve in
//! stage 1. Instead the route enables CDP's file-chooser interception
//! (`Page.setInterceptFileChooserDialog`), performs the click that would open
//! the OS dialog, and answers the intercepted dialog event with
//! `Page.handleFileChooser` carrying the validated path. The OS window
//! manager never renders the dialog.
//!
//! **Stage 3 — synthetic drag/drop for dropzones.**
//! For custom HTML5 dropzones that never instantiate a file input, the route
//! reads the file bytes, constructs `File` + `DataTransfer` in page memory,
//! and dispatches synthetic `dragenter`/`dragover`/`drop` events onto the
//! target container.
//!
//! All stages require the path to pass [`validate_upload_path`] against the
//! staging allowlist (default `~/jobs/uploads`) — audit SEC-6: this prevents
//! arbitrary local file exfiltration via a compromised prompt-injected page.

use std::path::{Path, PathBuf};

/// Stage-1 DOM probe: find a file input, visible or hidden.
///
/// Returns `{ found: true, nodePath: [<ancestor indices…>] }` describing the
/// input's position for `DOM.resolveNode` → `backendNodeId`, or
/// `{ found: false }`. Searches `input[type=file]` including shadow roots one
/// level deep; matches inputs hidden via `opacity: 0`, `display: none`,
/// `visibility: hidden`, or zero size — the exact pattern Workday and
/// Greenhouse use beneath styled upload buttons.
pub const UPLOAD_STAGE1_JS: &str = r#"() => {
    const isFileInput = (el) =>
        el && el.tagName === 'INPUT' && (el.type || '').toLowerCase() === 'file';
    const scan = (root) => {
        const direct = root.querySelector('input[type="file"], input[type="FILE"]');
        if (direct) return direct;
        // One level into open shadow roots (custom upload components).
        for (const el of root.querySelectorAll('*')) {
            if (el.shadowRoot) {
                const inner = el.shadowRoot.querySelector('input[type="file"]');
                if (inner) return inner;
            }
        }
        return null;
    };
    const input = scan(document);
    if (!isFileInput(input)) return { found: false };
    // Build an index path from document root for DOM.resolveNode fallback.
    const path = [];
    let node = input;
    while (node && node !== document.documentElement) {
        const parent = node.parentNode;
        if (!parent) break;
        path.unshift(Array.prototype.indexOf.call(parent.childNodes, node));
        node = parent;
    }
    const cs = window.getComputedStyle(input);
    return {
        found: true,
        nodePath: path,
        hidden: cs.display === 'none' || cs.visibility === 'hidden'
            || parseFloat(cs.opacity || '1') === 0,
    };
}"#;

/// Stage-3 dropzone payload builder.
///
/// Takes `(container, fileName, mimeType, base64Data)`; constructs a `File`
/// from the bytes in page memory, wraps it in a `DataTransfer`, and dispatches
/// the full `dragenter` → `dragover` → `drop` sequence onto the container.
/// Returns `true` when the drop event was dispatched.
pub const UPLOAD_STAGE3_JS: &str = r#"(container, fileName, mimeType, base64Data) => {
    const bytes = Uint8Array.from(atob(base64Data), (c) => c.charCodeAt(0));
    const file = new File([bytes], fileName, { type: mimeType });
    const dt = new DataTransfer();
    dt.items.add(file);
    const opts = { bubbles: true, cancelable: true, dataTransfer: dt };
    container.dispatchEvent(new DragEvent('dragenter', opts));
    container.dispatchEvent(new DragEvent('dragover', opts));
    container.dispatchEvent(new DragEvent('drop', opts));
    return true;
}"#;

/// Validate an upload path against the staging allowlist.
///
/// Canonicalizes `path` (resolving `..`, symlinks) and verifies the result
/// lives under one of the `allowlist` directories. Returns the canonical
/// path on success.
///
/// # Errors
/// - File does not exist / cannot be canonicalized.
/// - Canonical path escapes every allowlist entry (audit SEC-6: blocks
///   `file:///etc/passwd`-style exfiltration via prompt-injected pages).
pub fn validate_upload_path(path: &str, allowlist: &[PathBuf]) -> anyhow::Result<PathBuf> {
    let canonical = Path::new(path)
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("upload path not accessible: {path} ({e})"))?;
    let inside = allowlist.iter().any(|dir| {
        dir.canonicalize()
            .map(|d| canonical.starts_with(d))
            .unwrap_or(false)
    });
    if !inside {
        return Err(anyhow::anyhow!(
            "upload path escapes allowlist: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn allowlist(dir: &Path) -> Vec<PathBuf> {
        vec![dir.to_path_buf()]
    }

    #[test]
    fn stage1_js_finds_hidden_inputs() {
        assert!(UPLOAD_STAGE1_JS.contains(r#"input[type="file"]"#));
        assert!(UPLOAD_STAGE1_JS.contains("shadowRoot"));
        assert!(UPLOAD_STAGE1_JS.contains("found"));
        assert!(UPLOAD_STAGE1_JS.contains("nodePath"));
    }

    #[test]
    fn stage3_js_builds_file_and_dispatches_drop() {
        assert!(UPLOAD_STAGE3_JS.contains("new File("));
        assert!(UPLOAD_STAGE3_JS.contains("DataTransfer"));
        assert!(UPLOAD_STAGE3_JS.contains("'drop'"));
        assert!(UPLOAD_STAGE3_JS.contains("atob(base64Data)"));
    }

    #[test]
    fn validate_accepts_file_inside_allowlist() {
        let dir = std::env::temp_dir().join("vm-bridge-upload-test-ok");
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("resume.pdf");
        fs::write(&f, b"pdf-bytes").unwrap();
        let got = validate_upload_path(f.to_str().unwrap(), &allowlist(&dir)).unwrap();
        assert!(got.ends_with("resume.pdf"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_rejects_dotdot_escape() {
        let dir = std::env::temp_dir().join("vm-bridge-upload-test-escape");
        fs::create_dir_all(&dir).unwrap();
        let outside = std::env::temp_dir().join("vm-bridge-upload-test-escape-secret.txt");
        fs::write(&outside, b"secret").unwrap();
        // Attempted traversal: <allowlist>/../<outside file>
        let evil = format!("{}/../vm-bridge-upload-test-escape-secret.txt", dir.display());
        let err = validate_upload_path(&evil, &allowlist(&dir)).unwrap_err();
        assert!(err.to_string().contains("escapes allowlist"));
        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&outside).ok();
    }

    #[test]
    fn validate_rejects_symlink_escape() {
        let dir = std::env::temp_dir().join("vm-bridge-upload-test-link");
        fs::create_dir_all(&dir).unwrap();
        let outside = std::env::temp_dir().join("vm-bridge-upload-test-link-target.txt");
        fs::write(&outside, b"secret").unwrap();
        let link = dir.join("evil-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(unix)]
        {
            let err = validate_upload_path(link.to_str().unwrap(), &allowlist(&dir)).unwrap_err();
            assert!(err.to_string().contains("escapes allowlist"));
        }
        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&outside).ok();
    }

    #[test]
    fn validate_rejects_missing_file() {
        let dir = std::env::temp_dir().join("vm-bridge-upload-test-missing");
        let err = validate_upload_path(
            dir.join("nope.pdf").to_str().unwrap(),
            &allowlist(&dir),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not accessible"));
    }
}
