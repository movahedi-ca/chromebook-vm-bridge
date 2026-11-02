# Application Wave Operations Manual

Standing process for running job-application waves, test or real. Distilled from the 255-run sin-industry program (Nov 2026): the audit, the recovery playbooks, the locked policy decisions, and the infrastructure hardening. Read this before dispatching any wave. It replaces re-deriving any of it.

Owner: AUTO. Last updated: 2026-11-02.

Related: `implementation-playbook.md` (the 255-run execution record), `rfi.md` (locked decisions), `program-ledger.csv` (the ledger), `improvements.md` (per-run changelog), `~/jobs/solvers/` (solver library), `~/jobs/log-ledger` (the only ledger writer).

**2026-11-02 redirect (his word): the program's goal is process improvement for future use, not finishing blocked applications. Recovery waves B/C/D/E and staged Wave A were cancelled ~11:45 EST the same day they were dispatched; no browser actions had been taken and nothing was submitted. This manual is the durable output of that redirect.**

## 1. The two governance modes

Every wave runs in exactly one mode. The mode is declared in the wave brief and never changes mid-wave.

**Test-track mode.** The wave exists to improve the process, not to get jobs. Zero tracker entries. Zero follow-up emails. Recruiter or automated replies are rejected or ignored, never pursued. Interviews are rejected. Confirmation emails arriving in the inbox carry zero obligations and must never reclassify the wave as real.

**Real mode.** The wave exists to get hired. Every submission gets a tracker entry with the Ledger Link paragraph, follow-ups on day 3/10/21, and normal interview engagement. The job-application playbook (`~/workspace/custom-skills/local-job-application-playbook/SKILL.md`) governs.

## 2. Locked policies (do not re-debate these)

1. **OTP codes: sealed flow only.** One-time codes are read through the protected connector lookup and consumed by the browser's `verification_code` field, or relayed by the user in chat. Never scrape them from a Gmail tab DOM, never guess digits. (RFI-4.)
2. **WAF and bot walls: never evade via home network.** No residential IP, no host tether, no datacenter-IP retry loops to get around Radware, Cloudflare 406, or Imperva. Log as `blocked` with the reason and move on. (RFI-5.)
3. **CAPTCHA: supervised queue.** Interactive challenges (reCAPTCHA v2, image tiles) are solved either via the proven visual-grounding + xdotool pipeline (screenshot, ground, VM-side clicks, verify) or parked for a supervised operator window with his explicit scheduling. Never spend 25 minutes filling a form before checking for a CAPTCHA. (Spring Financial Run-141 rule.)
4. **Self-ID verbatim.** Every application carries the concrete answers, not a rule reference: gender man; sexual orientation heterosexual; Indigenous: no; racialized (Iranian); disability/health condition: no. Never "choose not to answer". (KPMG lesson: the brief must state the answers; the rule alone does not survive summarization.)
5. **Outbound sends: one confirmation each.** No exceptions except the standing ones (nightly Reddit autopilot, daily Instagram round).
6. **Interview-stage blocklist.** Any new employer is checked against the blocklist before the first run. (OTPP until 2026-12-31; Interac blackout until 2026-12-27.)
7. **Honesty rails.** A run without captured confirmation is `blocked`, never `submitted`. Never present a claim as verified unless verified live in that run. (His standing rule.)

## 3. Pre-flight: the five gates

Run these in order for every company block, before any lease is spent.

- **Gate 1: zero-lease liveness.** Never open Chrome to check for jobs. Query the public endpoint first: Greenhouse `boards-api.greenhouse.io/v1/boards/<slug>/jobs`, Workday CXS `POST /wday/cxs/<tenant>/<site>/jobs`, Oracle CE `recruitingCEJobSearch/list`, Rippling `ats.us1.rippling.com/api/v2/board/<slug>/jobs`, Dayforce `__NEXT_DATA__`, Workable `llms.txt`. Zero lane-fit jobs: log the shortfall immediately.
- **Gate 2: DNS and bot-wall probe (10s, headless).** Resolve on the VM, compare against Cloudflare DoH. If Control D returns 0.0.0.0, pin the DoH-verified edge IP in `/etc/hosts` and verify reachability before navigating.
- **Gate 3: 15-second form audit (run 1 of the block).** reCAPTCHA v2 visible: route to the supervised queue at once. Email OTP: use the sealed code flow (policy 1). Upload inside an iframe: navigate the tab to `iframe.src` before calling upload.
- **Gate 4: post-parse field audit before submit.** Assert Last Name is "Movahedi" (the Workday "CIPP" parse bug), phone is 4372562256 with Canada (+1), Globe end date 10/2026 with Currently Work Here unchecked. On Rippling/Dayforce, upload before manual fills or re-verify via FormData after.
- **Gate 5: blocklist + brief check.** New employer: check the blocklist. Every brief carries the self-ID answers verbatim.

## 4. Per-ATS runbook index

Proven classes and their recipes. Solver files live in `~/jobs/solvers/`; the per-run detail lives in `hidden_files/run-NN.md` and `improvements.md`.

| ATS class | Status | Recipe source |
|---|---|---|
| Workday (wd1/3/5/10/503/CXS) | Proven with exceptions | Two-stage hidden-input keyboard commit for virtualized multiselects (scrollIntoView center, elementFromPoint verify, native click, prototype setter + onChange + input event, CDP ArrowDown, Enter). Never React fiber pokes (VPS error boundary). `wd-batch.js`. |
| SAP SuccessFactors | Proven | `sf-batch.js`; OTP at profile level (one verification unlocks the block). |
| Dayforce HCM | Proven | Manual flow: upload then click "Import Resume" (raw input shows 0 files). Silent submit stall means screenshot at once (likely reCAPTCHA). `df-batch.js`. |
| Ashby | Proven | React prototype setter for tel/number/URL inputs (never bridge fill); button groups via JS click scoped to the question text. `ashby-fill.js` (see `jv-batch.js` naming in solvers; check `hidden_files` for the Ashby fill script). |
| Greenhouse | Proven | Iframe escape: navigate the tab to `iframe.src`, then upload finds the input natively. Public JSON API for enumeration. |
| Jobvite | Proven, gated | `jv-batch.js`. Headless pool ONLY when invisible reCAPTCHA v3 provably hangs on visible Chrome; never as default. |
| TalentNest / NEOGOV / Rippling | Proven, fastest | Persistent scripts where they exist (`neogov-scripts/`). Rippling: run upload before manual fills. |
| Workable | Proven, fastest | No account needed; `llms.txt` for enumeration. |
| Manitou / Teamtailor | Proven | Inline forms; watch for unresolved i18n keys on submit buttons (e.g. `sendPostuler`). |
| Oracle Recruiting Cloud | OTP-gated | Form fills are solved; the gate is email OTP at profile level (policy 1). |
| ADP Workforce Now | Hard | Stencil shadow DOM: native CDP type with key delay, explicit blur+change, inner-button click via bounding rect + xdotool. Deep-link construction via ccId/jobId from XHR when Apply loops to board. |
| iCIMS | Hard | Iframe escape (same as Greenhouse). Base64 DataTransfer fallbacks are banned (they hang parsers). |
| UKG Pro | Hard | Board root first, click the job title in-page to preserve SPA state; direct `/OpportunityApply?opportunityId=` as fallback. |
| JazzHR / BambooHR | CAPTCHA-gated | ReCAPTCHA v2: supervised queue (policy 3). |
| Bot-walled (Radware, Imperva 406) | Declined | Policy 2. Log and move on. |

## 5. The three hard gates (how to handle each when hit)

**Email OTP.** Sealed flow only (policy 1). The code is looked up through the protected connector path or relayed by the user in chat, then entered. Profile-level verification (Oracle CE, SuccessFactors) unlocks the whole company block at once: prioritize these, one code per employer.

**Interactive CAPTCHA.** First try the visual-grounding + xdotool pipeline (it solved Dayforce image tiles on OCS Run 206). Two rejections means park for the supervised operator window: pre-fill all tabs, pause, one short manual click per tab with his explicit scheduling, then the agent finishes and captures evidence.

**WAF / IP block.** Policy 2. No evasion, no retries from another network. Log `blocked` with `BLOCKED-botwall` or `BLOCKED-ip` in the notes and move on.

## 6. Evidence and ledger discipline

- Every run produces: `hidden_files/run-NN-confirm.txt` (confirmation text or URL) + screenshot. No confirmation captured means `blocked`, never `submitted`.
- Every ledger mutation goes through `~/jobs/log-ledger` (`check` to validate, `set --run` to mutate). Direct CSV edits are banned. The writer enforces the closed state enum (`submitted|blocked|shortfall|not-applied`) and the atomic locked write.
- Run notes (`hidden_files/run-NN.md`) record what was tried, what worked, and what generalizes. Write them from the run, not from memory.

## 7. Stop conditions

- 429, rate limit, account lock, or provider block: stop that portal's scope immediately. No retry routing, no endpoint switching, no rate lowering.
- A run that would need his manual action outside a scheduled window: park it, do not improvise.
- Any step the brief did not authorize: stop and escalate. A general "keep going" never authorizes a withheld step.

## 8. Infrastructure (what exists, who owns it)

- Bridge bugs: filed as issues #1-5 on `movahedi-ca/chromebook-vm-bridge`; builds ride the nightly Rust-rewrite queue under Directive 40. No parallel tracks.
- Solvers: `~/jobs/solvers/` is the persistent home. Never work from `/tmp` copies. Copy, adapt per run, record the adaptation in the run note.
- Ledger writer: `~/jobs/log-ledger` (crate `~/workspace/rust-rewrites/log-ledger/`). The only write path.
- New tooling is Rust-first (Directive 40). Python ports go through the nightly ledger.

## 9. Wave brief template

Every wave dispatch carries, verbatim:
1. Governance mode (section 1) and the program ledger path.
2. The self-ID answers (policy 4), written out, not referenced.
3. The five gates (section 3) and the per-run evidence rule (section 6).
4. The stop conditions (section 7) and the hard-gate handling (section 5).
5. Scope: exact run IDs, portals, and which waves are in and out. Declined scopes are named so nobody re-opens them.

## 10. Closeout

When a wave ends: re-derive counts from the enum-clean ledger (no hand mapping), sync `actions-taken.md` from the run notes, write the one-page closeout (counts, wall-clock and lease-minutes per wave, recovered vs still blocked with reasons), and record the final state in memory in one line plus pointer.
