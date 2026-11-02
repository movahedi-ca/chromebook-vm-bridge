# Optimized Portal Password / Reset Path
Adopted 2026-11-02 at Captain's order ("optimize this path of reset password").
Problem it fixes: portals emailed him repeatedly (BMO 5x "Reset your password", Clearco 3x)
because runs retried sign-in blindly and re-triggered resets. Reset links expire (Workday: 2h).

## Registry
`~/workspace/job-portal-accounts/registry.json` is the single source of truth per portal:
email used, Command Center entry id, status (`verified-working` | `needs-reset` |
`reset-in-progress` | `locked`), `last_verified` timestamp, notes.
Every application run MUST read it before touching a portal's sign-in.

## Procedure (every run, every portal)
1. **Registry check.** If `verified-working` and `last_verified` within 30 days: sign in
   directly with the stored Command Center password. No reset, no extra emails.
2. **One sign-in attempt** with the stored password. Success -> mark `verified-working`
   with timestamp, continue the application.
3. **Fresh-link check BEFORE requesting.** If sign-in fails, the coordinator (main agent)
   checks Gmail for a reset email from that portal within the last 90 minutes. Fresh link
   exists -> hand it to the browser task, request nothing new.
4. **Single reset request.** Only if no fresh link: click Forgot Password ONCE for the
   registered email. Never re-click. Record `reset_requested_at` in the registry.
5. **Link retrieval is the coordinator's job.** The coordinator polls Gmail, extracts the
   reset link, and hands it to the browser task via `browser.browse`. The browser task
   never opens Gmail itself.
6. **Stable password.** Set the new password to the EXISTING Command Center entry value
   so the stored value stays valid. If portal complexity rules reject it, generate a new
   one and UPDATE the Command Center entry immediately (same run, no exceptions).
7. **Verify + record.** Sign in with the new password, mark `verified-working` + timestamp.
8. **Dedup limits.** Max one reset request per portal per run; max one per 24h unless the
   previous link verifiably expired. Identical repeat emails to him = defect, fix the run.

## Notes
- Old Command Center entries (58 = BMO old, 41 = Manulife) stay for history; the registry
  names the live entry per portal.
- "Wrong stored password -> reset immediately" (SIN-100 rule) still holds; this procedure
  is HOW the reset runs without spamming him.
