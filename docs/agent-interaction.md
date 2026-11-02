# How agents interact with the chromebook-vm

The chromebook-vm is Mohammad's always-on physical machine: an Asus
Chromebook Flip C433T (Intel Core m3-8100Y, 8GB RAM) running Debian 13,
on the tailnet at `100.112.27.106` as user `auto`. Agents never touch it
directly as a second brain. The standing shape is a three-sided protocol:

1. **The VM executes.** Deterministic scripts and the browser bridge do
   the work; no LLM runs on the VM.
2. **The connection carries compact results.** One SSH call per task;
   results come back as compact text or JSON, never a running narration.
3. **The judgment stays here.** The agent VM does the thinking, the
   planning, and the delivery. Token discipline is enforced at every
   layer.

A full agent loop on the VM was considered and rejected: it would burn
tokens twice, add coordination overhead, widen the credential attack
surface, and add a second LLM hop of latency.

## The layers, outside in

**SSH (`cb-ssh.sh`).** The only transport. Key auth over the Tailscale
tunnel through a managed egress proxy; proxy auth is minted fresh per
call and never cached. The wrapper auto-recovers stale multiplexed
masters, classifies failures into distinct exit codes (proxy-down,
host-down, auth, host-key-changed, keepalive timeout), retries transient
errors with capped backoff, pins the host key, and logs every call.

**Job protocol (`cb-job.sh`).** One SSH call per task: `ping`,
`preflight`, `doctor`, `top`, `fetch` (headless-Chrome page extraction
to compact JSON), `run` (synchronous, 600s cap), `bg`/`status`/`get`
(background jobs with file-lock state, never PID-based), `kill`, `rm`,
plus `fetch-lp` (Lightpanda isolated unsigned fetch, explicit opt-in).

**The bridge (`vm-bridge`, Rust, v2.0.0).** A browser-automation service
on the VM at `127.0.0.1:8899`. It spawns and supervises its own headed
Chrome on display `:1` (automation profile
`~/.config/chrome-automation`, seeded from his signed-in profile) with
capped-backoff relaunch. Selector-based control, deterministic interact
engine, SSRF firewall, scoped API keys, FormGuard vault, PrivyGate, and
an MCP server (10 tools over stdio). The Python/Playwright predecessor
is kept as an instant-rollback legacy.

**The client (`bridge-cli`).** Typed commands, JSON built in code (no
quoting hell), auth handled VM-side so the key never crosses SSH:
`health`, `state`, `navigate`, `text`, `click`, `fill`, `type`,
`press`, `eval` (single-line JS), `wait`, `order` (deterministic
natural-language element resolution), `order-smart` (order plus Gemini
escalation), `script` (multi-step JSON), `tabs`/`tab-new`/`tab-switch`,
`snapshot`, `shot`, `call` (raw method+path), `lease`
(acquire/release/status, mutating calls need the lease id when held),
`pool` (headless context pool for parallel background browsing),
`click-node` (CDP-direct click by backendNodeId). `--visible` drives
the real OS cursor via XTest when he is watching.

**Visual desktop (`cbvnc.sh`).** VNC into display `:1` for what the DOM
cannot see: `shot`, `click`, `key`, `type`, `sh`. The physical
Chromebrook screen mirrors `:1`, so everything visual is also visible
to him in the room.

**Virtual desktops (vd1..vd10).** Ten extra headed sessions for parallel
work: Xvfb `:2`..`:11`, VNC `5903`..`5912`, CDP `9333`..`9342`, one
Chrome profile each. `vd-start.sh [N]` / `vd-stop.sh [N|all]` /
`vd-chrome.sh vdK [url]`. Sustained parallel work stays at 6 or fewer;
10 is for short bursts.

## When to use which layer

- **Page content, one shot:** `cb-job.sh fetch` — headless Chrome,
  compact JSON, one SSH call. Never screenshot to read text.
- **Login-walled browsing with a persistent session:** the bridge on
  `:1`, driving the automation profile that holds his signed-in
  cookies. Never attempt to create a Google session on his behalf.
- **Interactive or bot-defended pages:** the visual desktop via
  `cbvnc.sh`, or the bridge with `--visible`.
- **Parallel background browsing:** the bridge `pool`, or virtual
  desktops for fully separate headed sessions.
- **Long-running work:** `cb-job.sh bg`, poll `status`, pull once with
  `get`. Never a chatty poll loop.
- **Files:** `cb-xfer.sh` (rsync, sha256-verified). rsync over the raw
  tunnel is broken; tar-over-ssh is the fallback pattern.

## Standing rules

- **Look before you drive.** The bridge is DOM-blind to browser-chrome
  UI: popups, the "Restore pages?" dialog, and toolbar chips never
  appear in eval or snapshots. At the start of every VM browser session,
  take one `cbvnc.sh shot` and look at the full desktop before touching
  anything. Never click Restore; ESC dismisses it.
- **Never terminate Chrome.** The bridge supervises its own browser;
  killing it fights the supervisor.
- **Verify before clicking.** The bridge exposes perceptual hashes and
  screenshot deltas; check hash before mouse actions, re-baseline after
  navigation.
- **Keyboard beats mouse for forms.** Clicks need visual grounding
  (full-res screenshot, 0-1000 normalized coordinates to desktop
  pixels); typing after a verified click is trustworthy blind.
- **Google Password Manager is the forward credential store.** Always
  accept Chrome's save-password prompt; prefer autofill over typing.
  Password values never travel in chat, memory, or logs.
- **Show mode.** He watches the display. Visual work is inherently
  visible; headless work announces itself on the desktop dashboard.
  Batch xdotool sub-steps into one SSH call; screenshot only to ground
  a click target, never to "check" inferable state.
- **Stop, don't escalate.** Bot challenge, CAPTCHA, or login wall:
  park and report. Never route around WAF/bot defenses via residential
  IPs or retry loops. His authenticator taps and phone verifications
  stay his.
- **Keep it light.** One visual Chrome plus one headless fetch is fine;
  five parallel headless Chromes on 8GB is not.

## What lives where

| Concern | Location |
|---|---|
| Bridge source (Rust) | `rust/` in this repo; deployed to `~/jobs/vm-bridge/` |
| Bridge systemd unit | `rust/systemd/` |
| Agent tooling | `agent/` (bridge-cli.py legacy, cb-ssh.sh, cb-job.sh, cb-xfer.sh) |
| VM job scripts | `~/jobs/` on the VM (fetch.py, activity panel, mirror scripts) |
| Virtual desktops | `vm/virtual-desktops/` |
| LinkedIn outreach scripts | `linkedin-outreach/` |
| Full machine setup | `docs/chromebook-vm-setup.md` |
| Technical reference | `docs/agent-interaction-technical.md` |
