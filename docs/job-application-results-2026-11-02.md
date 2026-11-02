# Job-application automation: technical results 2026-11-02

5 applications submitted end to end via browser automation. This doc records
only the technical findings: blockers hit, root causes, and the fixes that
are now standing procedure. No candidate narrative.

## Batch result

| # | Employer / role | Portal | Submitted (EST) | Confirmation |
|---|---|---|---|---|
| 1 | Randstad, Sr Privacy Impact Assessment Specialist | randstad.ca | 12:31 | "thank you for your application" page |
| 2 | Vancity, Technology & AI Risk Sr Manager (SRTEC007886) | Ultipro | 12:34 | "Your application was submitted", appl ID 4456b445 |
| 3 | ScaleUp Recruiting Partners, Sr GRC Consultant | Jobgether -> Recruiterflow | 12:43 | "Application submitted successfully" |
| 4 | Brim Financial, Director Payments Compliance & Regulatory Affairs | Jobgether -> ClearCompany | 12:46 | "Thank you for applying." |
| 5 | BMO, Manager Insurance Compliance (R260024589) | Workday | 12:52 | "Application Submitted", status Under Review |

## New standing infrastructure

### Portal account registry
`job-portal-accounts/registry.json` (companion repo location: track per-portal
state, do not commit password values). Schema per portal:
`label, email, auth, command_center_entry (id only), status
(verified-working | needs-reset | reset-in-progress | locked),
last_verified (ISO), notes`. Rule: every run reads the registry before
touching a portal sign-in. `verified-working` + fresh timestamp = sign in
directly, no reset flow.

### Optimized reset path (standing procedure)
Problem it fixes: repeated sign-in retries re-triggered password resets and
spammed the candidate's inbox (5x BMO, 3x Brim in one morning); reset links
expire (Workday 2h, ClearCompany 15m).
1. Registry check first; sign in with stored password when verified-working.
2. One sign-in attempt with the stored password. Success -> mark verified.
3. Before requesting a reset, check Gmail for a fresh (<90 min) reset email;
   reuse its link, request nothing new.
4. Max ONE reset request per portal per run (never re-click); record
   `reset_requested_at`.
5. Link retrieval is the coordinator's job: poll Gmail, extract the link,
   hand it to the browser task. The browser task never opens Gmail.
6. Set the new password to the EXISTING stored entry value so it stays valid
   (generate + update the entry only if portal complexity rules reject it).
7. Verify sign-in, mark registry verified-working + timestamp.

## Per-portal technical findings

### BMO / Workday (bmo.wd3.myworkdayjobs.com)
- Create Account submitted 3x (email + generated password, consent checked):
  no form error, but every submission redirected to the sign-in page instead
  of advancing. Sign-in then rejected 4x: "wrong email or password or account
  might be locked."
- "Forgot your password?" -> Reset Password ALWAYS fails with a Workday
  backend error: "Failed to initiate password reset, please contact
  administrator." This is server-side, not a wrong-email message. The
  in-portal reset form is unusable on this tenant; do not retry it.
- Working path: password-reset LINK delivered by email (candidate supplied a
  fresh one). Opened the link, set the password to the stored entry value,
  "Password has been reset", sign-in succeeded.
- Resume autofill parser defects fixed against the resume: Last Name parsed
  as "CIPP" (corrected), city Toronto->Mississauga, 3 junk work entries from
  cover-letter text (one undeletable via UI, converted into the real Globe
  entry, duplicate deleted), spurious "Hiring" skill removed, volunteer
  companies corrected, duplicate education entry removed, website/LinkedIn
  corrected. Degree-level dropdowns had no parsed value; selected by program
  duration (Masters 2-yr, Bachelors 4-yr); resume holds the actual credentials.
- Registry status after run: verified-working.

### Brim / ClearCompany (brim.clearcompany.com)
- Auth is email magic-link, 15-minute expiry, no password. An accidental
  navigation dropped the first session and the ATS demanded re-auth.
- 3 magic links requested in ~3 minutes all expired before use. Working
  pattern: request ONE link, candidate pastes it immediately, open within
  the 15-minute window.
- Resume parser created 6 bad education rows (duplicate Northeastern,
  2 blank rows, OneTrust + AWS misread as schools when they are tools).
  Removed; kept the 2 real degrees.
- Registry: auth=magic-link, no password entry.

### ScaleUp / Recruiterflow (recruiterflow.com)
- The Location group's "Zip code" field carries no asterisk but form
  validation treats it as required (red border + "Location is required",
  blocks submit). Entering the postal code clears it. Check for
  unmarked-required fields on every Recruiterflow form before debugging
  anything else.

### Vancity / Ultipro — clean submit, no blockers.
### Randstad — clean submit, no blockers. No screening questions on site.

## Reusable rules distilled

- Workday tenant reset forms can be backend-broken while emailed reset links
  still work: always prefer the link-from-email path on failure.
- Magic-link ATS (ClearCompany): one link, use immediately, 15-min clock.
- Never let a browser task open the candidate's Gmail; the coordinator
  fetches links/codes and hands them over.
- One reset request per portal per run, max. Repeat emails to the candidate
  = defect in the run, not a portal quirk.
- Always diff the ATS-parsed profile against the resume before continuing;
  parser junk (wrong names, ghost entries, misread skills) is the norm, not
  the exception.
