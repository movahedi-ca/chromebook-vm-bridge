# portal-tool
CLI for the job-portal account registry + fresh reset-link checks.
Built 2026-11-02 after the batch that spammed the candidate's inbox with
repeat password-reset emails (5x BMO, 3x Brim): the reset path is now
executable, not just documented.
- `portal-tool registry list|get|set` — read/write the registry
  (`~/workspace/job-portal-accounts/registry.json`, or $PORTAL_REGISTRY).
- `portal-tool fresh-link <portal> [--max-age-min 90]` — check Gmail for a
  fresh reset/magic-link email via the portal's stored gmail_account +
  gmail_query. Reports metadata only (subject/date/age); link URLs stay
  behind the connector's protected markers by design and are fetched by
  the coordinator, never unwrapped here.
- `portal-tool next-action <portal>` — decide: sign-in-directly (registry
  verified-working) | request-magic-link-once | run-fresh-link-check.
Build: `cargo build --release` (clippy -D warnings, fmt clean).
Deploy: copy `target/release/portal-tool` to `~/jobs/portal-tool`.
