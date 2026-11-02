# Implementation Playbook: 255-Run Audit Recovery and Hardening

Date: 2026-11-02. Owner: AUTO.
Source plan validated: `workspace/user/files/next_step_improvements_playbook.pdf` (9 pages, prepared Nov 2, 2026).
Program home: `workspace/goals/sin-industry-100-application-speed-program/`.

This is my own implementation plan. It keeps what the source plan got right, fixes what it got wrong or left vague, and routes work through machinery that already exists instead of building parallel tracks.

## 0. What I verified before writing this

Checked live against the program files on 2026-11-02. Anything not listed here is unverified.

Verified:
1. Arithmetic. The ATS table reconciles exactly: runs 255, submitted 90, blocked 73, shortfall 92. I re-derived all three from the raw CSV with a proper parser.
2. Defect 1 (CSV schema corruption). Confirmed: 260 data rows, run IDs 1-255 all present and distinct, plus 5 identical Village Farms rows where `run` = `2026-11-02T11:12:00Z`. Both ledger copies (`files/` and `hidden_files/`) are byte-identical, so the corruption is in the single source, not a sync drift.
3. Defect 2 (stale log). Confirmed: `actions-taken.md` header still reads "25 submitted / 100 target". SQDC and Clearco have zero narrative mentions; OCS and iGaming have one passing mention each; Fairstone has two. The missing narrative blocks are real.
4. State-label chaos. The 90/92/73 split only emerges after normalizing 19 distinct `confirmation_state` values (e.g. Submitted + Successfully Applied + Application Sent + `Submitted <hash>` + "Your application has been received!" = 90; Blocked + BLOCKED + BLOCKED-botwall + BLOCKED-captcha + BLOCKED-OTP = 78 raw, minus the 5 duplicate rows = 73; Shortfall + SHORTFALL + NOT-APPLIED = 92). The source plan's reconciliation is correct, but no future run should depend on anyone re-deriving this by hand.
5. Program is labeled test-only. `actions-taken.md` line 4: "Test track only." This binds the whole implementation (see Constraints).

Plausible but unverified (spot-check only, not re-derived):
6. Per-run timing claims (30s Workable, 38s Jobvite, 44s TalentNest, 49s Dayforce, 65s NEOGOV) and the specific fix attributions (Hard Rock Run 136, DraftKings Run 196, OCS Run 206 xdotool solve, FanDuel Run 171 iframe escape). Solver files exist in `hidden_files/` (`jv-batch.js`, `df-batch.js`, `mc-batch.js`, `cw-batch.js`, `dg-batch.js`, `icims-batch.js`, `neogov-scripts/`), which supports the claims, but I did not re-run or re-read every run note.
7. The "61 live roles" row-level mapping. The arithmetic holds (73 blocked minus 7 secondary shortfalls minus 5 Corby IP-blocked = 61), but I did not re-verify each of the 61 requisitions as live. Gate 1 in Phase 1 re-checks liveness before any lease is spent, so this resolves itself in execution.

Not verified and not assumed:
8. Whether the 90 "submissions" were real applications with his real identity. RESOLVED 2026-11-02 (his ruling): test-track governance applies, with the factual note that the submissions did transmit the real resume and real emails to live production ATS endpoints. Confirmation emails arriving in those inboxes carry zero follow-up or tracker obligations.
9. Whether a recovery worker is already live. RESOLVED 2026-11-02: the 14:20 UTC Pure Canadian notes were the morning cleanup worker (finished ~09:20 EST); read-only check ~11:15 EST confirmed no sin-program workers running.

## 0b. Phase 0 executed 2026-11-02 (locked scope, zero browser, zero lease)

1. Ledger sanitized: 5 malformed rows removed; `program-ledger.csv` now holds exactly runs 1-255 (both copies identical). Backups in `hidden_files/ledger-backup-2026-11-02/`. Note: 6 rows remain ragged (more columns than the header, from unquoted commas in notes); preserved as-is, cleanup deferred to post-exam.
2. `actions-taken.md` synced: header now reads 90 submitted / 255 runs (51 companies); narrative sections added for OCS (206-210), Fairstone Bank (211-215), SQDC (216-220), iGaming Ontario (221-225), Clearco (226-230), built from run notes. Both copies in sync.
3. Solvers consolidated: 9 batch scripts + neogov-scripts copied from `hidden_files/` to `~/jobs/solvers/` with a README (`/tmp` was already empty; nothing was lost).
4. Bridge issues filed: #1-#5 on `movahedi-ca/chromebook-vm-bridge`; pointer section added to `~/workspace/rust-rewrites/LEDGER.md`.
5. RFI-2 check: queue verified idle; no browser workers dispatched. VM Chrome untouched.

## 0c. Full 0-to-100 pass executed 2026-11-02 (his word; all zero-browser improvements)

6. Defect 3 resolved: Tilray (bot-gated Create Account, scoped out per RFI-5) and SNDL (careers URL dead, "Page not found") documented as unnumbered, scoped-out investigations in `actions-taken.md`. No run IDs assigned; nothing to recover.
7. State enum locked: `confirmation_state` normalized to `submitted|blocked|shortfall|not-applied` across all 255 rows (90/73/70/22, counts preserved exactly); 6 ragged rows repaired (unquoted commas in notes rejoined); mapping recorded in `hidden_files/STATE_MAPPING.md`.
8. Ledger writer built: `log-ledger`, a Rust CLI at `~/jobs/log-ledger` (crate `~/workspace/rust-rewrites/log-ledger/`, std-only). `check` validates schema; `set --run` mutates under an exclusive lockfile (stale-PID steal) with atomic temp+rename and re-validation. Ship gate all green (build, 4 tests, clippy -D, fmt); recorded in the Rust ledger. All future ledger writes go through this binary; direct CSV edits are banned.
9. Cannara drafts: 5 local email templates in `hidden_files/cannara-drafts/` (runs 166-170), marked DRAFT ONLY, never sent (RFI-6 locked).

Still parked per the locks: Waves A-D (browser, post-exam), Wave E (his CAPTCHA scheduling, post-exam), Wave F (declined permanently). Nothing in the 0-100 pass touched a browser, held a lease, or sent an email.

## 0d. Pacing overridden 2026-11-02 ~11:40 EST (his word: "do all the waves")

The exam-week freeze is lifted. All waves dispatched immediately:
- Coordinator: Waves B (13 roles), C (19 roles), D (4 roles) fully, plus Wave E pre-fill parked at CAPTCHA (10 roles).
- Wave A in stages by the parent agent: per-portal drive to OTP-sent, user relays each code in chat (sealed Option B; no Gmail reading, no DOM scrape), then submit + evidence. 3 codes needed: Great Canadian, SAQ, SIGA.
- Wave F stays declined permanently. Wave E's CAPTCHA clicks remain his (10-minute window when the pre-fill parks).

## 1. Constraints (standing rules that bind this implementation)

1. Test-track only. No tracker entries, no follow-up cadence, no interview engagement. If any employer replies to a test submission, it is rejected/ignored per the test-track rule, never pursued. Locked 2026-11-02 (RFI-1): the 90 submissions did transmit the real resume (`Movahedi-Resume-Standard.pdf`) and real emails (`m.h.movahedi97@gmail.com`, plus `mohammad@movahedi.ca` on DraftKings) to live production ATS endpoints, so confirmation/status emails will arrive in those inboxes. They carry zero follow-up or tracker obligations. Do not let inbox activity reclassify this program as real.
2. Blocklist gate. Any recovery or future wave that touches a new employer first checks the interview-stage blocklist (OTPP until 2026-12-31; Interac blackout until 2026-12-27). The 51 companies in this program contain neither, but the gate stays in the checklist permanently.
3. Self-ID brief, verbatim, on every application package or form: gender man; sexual orientation heterosexual; Indigenous: no; racialized (Iranian); disability/health condition: no. Never "choose not to answer". (This is the KPMG lesson; the brief travels as concrete answers, not as a rule reference.)
4. Outbound sends need one confirmation each. The only standing exceptions are the nightly Reddit autopilot and the daily Instagram round. Nothing in this program qualifies for an exception.
5. OTP/code handling. Locked 2026-11-02 (RFI-4): Option B only, the existing sealed-credential lookup flow. Option A (VM Gmail tab DOM read) is rejected; the credential-sealing audit trail takes strict priority over speed. Tier 1A stays parked until the parent agent runs the approved sealed `verification_code` flow post-exam.
6. His time this week is protected. CISSP exam Fri Nov 6, 10:30 AM. No supervised operator windows before the exam unless he explicitly asks. (RFI-3, RFI-8.)
7. Hardening work routes through existing machinery. Directive 40 and the nightly Rust rewrite own operational tooling; the bridge repo (`movahedi-ca/chromebook-vm-bridge`, audits 39-42 closed Oct 31) owns bridge changes. No parallel "v2" track.
8. Writing rule. Nothing ships in his name without the humanizer gate and a mechanical em-dash grep. For this internal playbook the drafting skill is skipped per Directive 31, but the em-dash grep still runs on every file I produce.

## 2. Phase 0: Data hygiene (no browser, no lease, I do this first)

Goal: a ledger and log that are machine-readable and truthful before any recovery run touches them.

1. Backup. Copy both ledger files to `hidden_files/ledger-backup-2026-11-02/` before any mutation.
2. Dedup (Defect 1). Delete the 5 timestamp rows for Village Farms (runs 251-253 stay as the 3 legitimate ADP-blocked rows; 254-255 were already logged as shortfall rows inside the dupes' notes, keep the integer rows). Atomic write: write to temp file, validate, then rename.
3. Schema lock. Normalize `confirmation_state` to a closed enum: `submitted`, `blocked`, `shortfall`, `not-applied`. Map the 19 observed values once, record the mapping in a `STATE_MAPPING.md` note, and never accept a free-text state again.
4. Header fix (Defect 2). Rewrite the `actions-taken.md` header to the true totals (90 submitted / 255 runs, 92 shortfalls, 73 blocked as of Nov 2) and add narrative sections for OCS (206-210), Fairstone (211-215), SQDC (216-220), iGaming Ontario (221-225), Clearco (226-230), built from the run notes, not from memory.
5. Defect 3 (unnumbered investigations). Assign Tilray and SNDL portal investigations explicit run IDs (propose 256-260 as a new block, or fold into the existing company blocks; see RFI-10) so nothing exists outside the ledger.
6. Ledger writer. From this point on, all ledger writes go through a single file-locked CLI (`log_ledger.py` with `flock` + schema validation). This is the permanent fix for the partial-write corruption that already cost rows 46-56 once. Direct CSV edits are banned after Phase 0.

Exit criteria: `program-ledger.csv` has exactly 255 integer rows, states are enum-clean, both copies identical, backup exists, and the writer CLI is the only write path.

## 3. Phase 1: Recovery waves (ordered by leverage, gated)

The source plan's three tiers are sound. I reorder within them by leverage-per-lease-minute and add gates the source plan omitted.

Standing pre-flight (every company block, before any lease):
- Gate 1: Zero-lease liveness check. Query the public JSON/SSR endpoint first (Greenhouse boards API, Workday CXS, Oracle CE search, Rippling board API, Dayforce `__NEXT_DATA__`, Workable `llms.txt`). Zero lane-fit jobs means log the shortfall immediately; Chrome is never opened for enumeration.
- Gate 2: DNS/bot-wall probe (10s headless). Resolve on the VM, compare against Cloudflare DoH. If Control D returns 0.0.0.0, pin the DoH-verified edge IP in `/etc/hosts` and verify reachability before navigating.
- Gate 3: 15-second form audit on Run 1. reCAPTCHA v2 visible means route to the supervised queue immediately (never fill 25 minutes of fields first; this is the Spring Financial Run-141 rule). Email OTP means use the approved code mechanism (Constraint 5). Iframe upload means navigate the tab to `iframe.src` before calling upload.
- Gate 4: Post-parse field audit before submit. Assert Last Name is "Movahedi" (the Workday "CIPP" parse bug), phone is 4372562256 with Canada (+1) dial code, Globe end date 10/2026 with "Currently Work Here" unchecked, and on Rippling/Dayforce run upload before manual fills or re-verify via FormData after.
- Gate 5 (mine, not in the source plan): blocklist check for any employer not in the original 51, and self-ID brief attached to the run brief verbatim.

Wave order (locked 2026-11-02; all waves start after Fri Nov 6; zero leases held until then):
1. Wave A: Tier 1A OTP unblocks (Great Canadian 46-50, SAQ 161-163, SIGA 231-232). PARKED until the parent agent runs the approved sealed `verification_code` flow post-exam (RFI-4 locked: Option B only).
2. Wave B: Tier 2A Workday widget commits (Labatt, Diageo, Sleeman, Fallsview, BetMGM). Use the proven Run 61/136/196 recipes; explicitly ban React fiber pokes after the VPS error-boundary lesson.
3. Wave C: Tier 2B iCIMS iframe escape (High Tide 91-92), Tier 2C ADP shadow DOM and deep-link construction (Pure Canadian 201-204, Village Farms 251-253), Tier 2D hydration recovery (Arterra, Mark Anthony).
4. Wave D: Tier 1B headless-pool exemption for Jobvite reCAPTCHA v3 (Gateway 42-45). Scoped exemption only: headless pool is permitted when invisible reCAPTCHA v3 provably hangs on the visible display, never as a default.
5. Wave E: Tier 1C reCAPTCHA v2 supervised queue (Mogo, Spring Financial, NSLC). DEFERRED until after the CISSP exam (RFI-3 locked); only with his explicit scheduling.
6. Wave F: Tier 3 WAF/bot-wall (Loto-Quebec, Corby, Tilray, SNDL). DECLINED permanently (RFI-5 locked): no residential/host-network routing to evade bot walls. Rows stay logged as `BLOCKED-botwall` / `BLOCKED-ip`.

Per-run evidence (non-negotiable): confirmation text or URL captured to `run-NN-confirm.txt`, screenshot on file, ledger row written via the locked CLI only. A run without captured confirmation is logged as blocked-unverified, never as submitted. This is the "do not lie to me" rule expressed as procedure.

Stop conditions: any 429/rate-limit/account-lock response stops that portal's scope immediately with no retry routing; any CAPTCHA that rejects the xdotool pipeline twice goes to the supervised queue rather than a third attempt; any run that would need his manual click outside a scheduled window is parked, not improvised.

## 4. Phase 2: Infrastructure hardening (routed, not parallel)

The source plan's five bridge/harness specs are good engineering. They do not become a separate "vm-bridge v2" effort. Filed 2026-11-02 as issues #1-#5 on `movahedi-ca/chromebook-vm-bridge` (RFI-7 locked), with a pointer section in `~/workspace/rust-rewrites/LEDGER.md` so the nightly coordinator sees them in the one queue:

1. 423 `mutations_in_flight` wedge (RAII InflightGuard + 60s CDP timeout + `/lease/reset-inflight`): file as a GitHub issue on `movahedi-ca/chromebook-vm-bridge`. This is a bridge bug; the bridge repo is where it gets fixed and reviewed.
2. Lease starvation and 600s expiry (fix `bridge-cli lease renew` to call renew, 900s default TTL, FIFO `/lease/enqueue`): same, bridge repo issue. Note: changing default TTL affects all consumers, so the issue must call out the blast radius.
3. `/browser/upload` iframe traversal + `bridge-cli upload --selector --frame --path`: bridge repo issue (Rust CLI + handler). The Python-era chunked base64 scripts are retired, not repaired.
4. Stale coordinates on `order click` (scrollIntoView + 150ms + recompute rect) and the 127.0.0.1:8099 SSRF allowlist: bridge repo issue. The SSRF allowlist change needs a security note in the issue since it narrows the SSRF firewall.
5. Solver consolidation (`~/jobs/solvers/`) and the locked ledger writer: agent-VM side, built here, and the writer CLI becomes mandatory for both scout crons. If any of these qualify as tool ports under Directive 40, they go through the nightly Rust ledger rather than as one-off scripts.

All five are tracked to done in the nightly Rust-rewrite ledger (`workspace/rust-rewrites/LEDGER.md`) so there is one queue, not two. See RFI-7 for his confirm on this routing.

## 5. Phase 3: Verification and closeout

1. Re-run the Phase 0 reconciliation: counts must be exactly derivable from the enum-clean ledger with no hand mapping.
2. Rewrite `actions-taken.md` fully from run notes (the Phase 0 patch fixed the header; Phase 3 makes the whole file match the ledger).
3. Produce a one-page closeout: final submitted/blocked/shortfall counts, wall-clock and lease-minutes per wave, which of the 61 recovered, and what remains blocked with reasons.
4. Update MEMORY.md: the program's final state in one line plus pointer, per the memory budget rule.

## 6. Schedule (locked 2026-11-02)

- Phase 0: EXECUTED today (2026-11-02), no browser, no lease, no operator time. See section 0b.
- Waves A-D: start after Fri Nov 6 (CISSP exam 10:30 AM). Zero VM bridge leases held until then; VM Chrome untouched through Friday.
- Wave A additionally gated on the sealed `verification_code` flow (parent agent, post-exam).
- Wave E: only with his explicit scheduling, post-exam.
- Wave F: declined permanently; no action.
- Phase 2: issues #1-#5 filed (no runtime impact to file them); builds ride the nightly queue.
- Phase 3: within 48h of the last recovery wave.

## 7. Explicit non-goals (locked 2026-11-02)

No real applications are submitted under this playbook beyond what the test track already did. No tracker entries. No follow-up emails. No interview engagement. No residential-IP routing, period (RFI-5 locked: declined). No headless-pool use outside the scoped Jobvite exemption. Cannara mailto batch: drafts/templates only, never sent (RFI-6 locked). No new employer outreach of any kind.
