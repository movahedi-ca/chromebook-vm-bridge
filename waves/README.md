# Application waves (nightly 100+20 program)
Technical output of the nightly test-track application program (SIN-100 speed
program + 20 timed fills, Nov 2026). The program's standing goal is process
improvement for future use, not finishing blocked applications.
- `docs/` — wave-operations-manual.md (standing process: governance modes,
  locked policies, five pre-flight gates, per-ATS runbook index),
  implementation-playbook.md (execution record), rfi.md (locked decisions).
- `solvers/` — persistent ATS batch solver scripts (Workday, SuccessFactors,
  Dayforce, iCIMS, Jobvite, UKG, etc.). Canonical copies; do not edit in
  place for one-off runs.
Related: `../rust/log-ledger` (the only ledger writer CLI), `../docs/`
(bridge docs).
