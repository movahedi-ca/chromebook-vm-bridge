# Vendored chromiumoxide — patch provenance (audit 40 LOW-11)

## Base

- Crate: `chromiumoxide` **0.7.0**, pristine source from
  `https://crates.io/api/v1/crates/chromiumoxide/0.7.0/download`
  (verified 2026-10-31; the vendor tree differs from it in exactly one
  file, `src/conn.rs` — see `chromiumoxide-conn.patch`).
- Wired via `[patch.crates-io]` in `rust/Cargo.toml`:
  `chromiumoxide = { path = "vendor/chromiumoxide" }`.

## Why vendored

chromiumoxide 0.7's WebSocket layer treats *any* unparseable CDP message as
a fatal stream error. Chrome 156 renamed
`ClientSecurityState.privateNetworkRequestPolicy` to
`localNetworkAccessRequestPolicy`, so every `Network.requestWillBeSentExtraInfo`
event killed the CDP connection ("oneshot canceled", bridge deaf) on any real
page load. Upstream has no release with a fix; vendoring was the pragmatic
unblock.

## The local change (`src/conn.rs`, `chromiumoxide-conn.patch`)

When a WS text message fails to deserialize as a CDP `Message`:

- **CDP events** (no `"id"` field): skipped with a loud `warn!` (message
  preview included) instead of killing the stream. Safe: no pending call
  waits on an event.
- **CDP responses** (have `"id"`): the stream now fails with
  `CdpError::Serde`, so the pending oneshot resolves with an error instead
  of hanging. The browser supervisor relaunches on a dead connection, and
  the HTTP `TimeoutLayer` (120s) bounds the request either way.

## Maintenance contract

- Re-evaluate on every upstream chromiumoxide release: if upstream handles
  protocol drift, drop the vendor tree and unpin `[patch.crates-io]`.
- Until then, upstream security fixes (CDP parsing, WS handling) do NOT
  arrive automatically. When pulling an upstream fix, re-apply
  `chromiumoxide-conn.patch` on top of the new base and refresh this file.
- CI note: `cargo audit` covers the *direct* tree; the vendored crate's own
  dependency versions are pinned by its vendored `Cargo.toml` — review them
  when re-basing.
