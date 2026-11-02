# RFI: Open Questions Before Implementing the 255-Run Recovery Plan

Date: 2026-11-02. From: AUTO. To: Captain.
Re: `next_step_improvements_playbook.pdf` (255-run audit, +61 recovery roadmap, bridge hardening spec).

## LOCKED DECISIONS — 2026-11-02 (his rulings)

All eight RFIs are decided. Defaults confirmed across the board, with the two refinements noted.

- **RFI-1 (Real vs. Test Track): DECIDED — test-track governance + flag live portal footprint.** The entire 255-run program stays under test-track governance: zero tracker entries, zero outbound follow-ups, reject/ignore any automated or recruiter replies per the test-track rule. Factual note from the logs: the 90 submissions did transmit `Movahedi-Resume-Standard.pdf` and `m.h.movahedi97@gmail.com` (plus `mohammad@movahedi.ca` on DraftKings) to live production ATS endpoints, so external confirmation/status emails will arrive in those inboxes, but they carry zero follow-up or tracker obligations.
- **RFI-2 (Live recovery worker): DECIDED — stand down, process check first.** The `2026-11-02T14:20:00Z` timestamp on Pure Canadian Gaming (runs 202-205) was the cleanup worker (runs 173-176, 187-190, 202-205), finished ~09:20 EST. Read-only check done 2026-11-02 ~11:15 EST: no sin-program workers running (only the standing daily job scout's read-only sweeps). No browser workers dispatched today.
- **RFI-3 (Supervised CAPTCHA window): DECIDED — defer Wave E until post-exam.** No manual CAPTCHA time before Friday. All 10 roles (Mogo, Spring Financial, NSLC) parked until after the CISSP exam (Fri Nov 6, 10:30 AM).
- **RFI-4 (OTP mechanism): DECIDED — Option B, sealed connector flow only.** Option A (DOM-tab scrape) rejected; credential-sealing audit trail takes strict priority over speed. Tier 1A (Great Canadian, SAQ, SIGA) stays parked until the parent agent runs the approved sealed `verification_code` flow post-exam.
- **RFI-5 (WAF tier routing): DECIDED — decline.** No Loto-Quebec (Radware), Corby (HTTP 406), or Tilray traffic via home/residential IP or host tether. Home network fingerprint is never exposed to evade bot walls. Those rows stay permanently logged as `BLOCKED-botwall` / `BLOCKED-ip`.
- **RFI-6 (Cannara mailto batch): DECIDED — drafts only, never sent.** Runs 166-170 stay in the test track: local templates only, zero outbound emails to `cv@cannara.ca`.
- **RFI-7 (Bridge hardening routing): DECIDED — GitHub issues + Directive 40.** No separate "v2" track. Five issues filed 2026-11-02 as #1-#5 on `movahedi-ca/chromebook-vm-bridge` (continuing the audit 39-42 history); tooling items ride the nightly Rust-rewrite ledger under Directive 40 (pointer section added to `~/workspace/rust-rewrites/LEDGER.md`).
- **RFI-8 (Exam-week pacing): DECIDED — Phase 0 today only; Waves A-D post-Nov 6.** Phase 0 data hygiene executed 2026-11-02 (ledger dedup to 255 rows, `actions-taken.md` header + runs 206-230 narratives, solvers consolidated to `~/jobs/solvers/`, 5 bridge issues filed). Zero VM bridge leases held; all recovery waves paused until after Friday's CISSP exam so nothing competes with exam prep or the Saskatchewan/GEM bid window (closes Tue Nov 3, 3:00 PM EST).

---

## Original RFI items (record of what was asked)

## RFI-1: Were the 90 submissions real applications?

What I need: confirm whether the 90 "submitted" runs were real applications filed with your real identity and resume, or test submissions. `actions-taken.md` says "Test track only," but the run notes describe real portal submissions with your standard resume.

Why it matters: if any were real, they carry tracker, follow-up, and interview-response obligations, and the recovery waves would create further real employer contact. If test-only, recovery stays inside the test track with no external obligations.

Decision: test-track governance (see locked decisions above).

## RFI-2: Is a recovery worker already running?

What I need: Pure Canadian Gaming rows 201-204 show fresh attempt notes timestamped today 2026-11-02T14:20:00Z referencing `run-202.md`. Is a recovery run live right now?

Why it matters: I will not double-dispatch workers onto the same portals. If something is live, I stand down and coordinate with it.

Decision: it was the morning cleanup worker, finished ~09:20 EST; queue verified idle (see locked decisions above).

## RFI-3: Supervised CAPTCHA window (Tier 1C: Mogo, Spring Financial, NSLC, 10 roles)

What I need: the plan needs about 10 minutes of your manual CAPTCHA clicks in one supervised window, with forms pre-filled in 3 tabs.

Why it matters: this is the only recovery step that spends your time, and your week is exam week (CISSP Fri Nov 6, 10:30 AM).

Decision: deferred until post-exam (see locked decisions above).

## RFI-4: OTP read mechanism (Tier 1A: Great Canadian, SAQ, SIGA, 10 roles)

What I need: pick the mechanism. Option A: the plan's proposal, a persistent VM Gmail tab (Tab #2) with the code read from the DOM. Option B: the existing sealed-credential lookup flow through the Gmail connector.

Why it matters: Option A is new machinery that bypasses the credential-sealing audit trail. Your Oct 20 blanket approval covers code lookup for sign-ins you requested; it does not name the DOM-tab route.

Decision: Option B only; Option A rejected (see locked decisions above).

## RFI-5: WAF tier routing (Loto-Quebec Radware, Corby 406, Tilray bot gate, ~10 roles)

What I need: approve or decline routing these portals through a residential or host-machine browser session (or an operator-assisted local tether), as the plan proposes.

Why it matters: this puts application traffic on your home network fingerprint to evade bot defenses. That is a meaningful exposure decision and I will not make it for you.

Decision: declined (see locked decisions above).

## RFI-6: Cannara Biotech mailto batch (5 roles)

What I need: the plan proposes preparing 5 tailored application emails for one-click confirmation. Confirm: drafts only, never sent (test track), or do you want them actually sent?

Why it matters: sending is an outbound action to a real employer. On a test track the answer should be drafts only, but I am asking because the plan's wording ("one-click user confirmation") reads like a send.

Decision: drafts only, never sent (see locked decisions above).

## RFI-7: Where the bridge hardening gets built

What I need: confirm my routing. The plan's five hardening specs become GitHub issues on `movahedi-ca/chromebook-vm-bridge` (not a parallel "v2" track), and tooling items ride the nightly Rust ledger (Directive 40).

Why it matters: avoids two queues owning the same work, and keeps the bridge's audit history (audits 39-42, closed Oct 31) as the single record.

Decision: confirmed as proposed; issues #1-#5 filed (see locked decisions above).

## RFI-8: Pacing around your exam

What I need: confirm the schedule. My proposal: Phase 0 data hygiene today (no browser, no lease, none of your time). Recovery Waves A-D start after Fri Nov 6 unless you say sooner.

Why it matters: recovery waves consume VM bridge leases and my attention; a failure mid-week would compete with your exam prep and the Saskatchewan bid (closes Tue Nov 3, 3 PM EST, already parked on the GEM role).

Decision: confirmed as proposed; Phase 0 executed 2026-11-02 (see locked decisions above).
