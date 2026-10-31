# vm-bridge

A Rust (axum + tokio) bridge that exposes a headed Chrome browser running on a
Linux VM as an HTTP API, so agents can drive a real browser (with the user's own
signed-in sessions) without touching the user's own machine.

Built for a Debian VM on an ASUS Chromebook (MrChromebox firmware), reached over
Tailscale. The bridge binds to localhost on the VM; remote access goes through
an SSH tunnel or the tailnet proxy.

## What it does

- Drives a persistent headed Chromium via CDP against a copy of the
  user's real Chrome profile, so logins persist.
- REST endpoints for navigation, clicking, typing, screenshots, DOM snapshots,
  tab management, and an order-smart element resolver (deterministic
  nodeId-first, with optional LLM escalation via accessibility snapshot).
- Context pool (max 3, LRU) for parallel background work.
  (Resource blocking and failure screenshots are planned, not yet implemented.)
- Human-like mouse movement with eased trajectories, for
  interactions that need to look natural.
- Hardening: X-API-Key auth (scoped keys: read/act/admin), per-path token-bucket
  rate limiting, JSON request logging, body-size caps, lease manager for
  exclusive sessions, pixel-diff visual-assertion endpoint, crash watchdog with
  auto-relaunch, SSRF firewall.
- MCP server (`--mcp-stdio`) with 10 tools for agent integration.
- Perception plane: AX snapshot with stable `[ref]` handles, screenshot
  comparison, visual assertions.

## Endpoint status

All route groups are implemented except the endpoints below, which return
`501 not_implemented` (the standard shape, so contract tests can tell
"not built" from "broken"). Do not plan workflows against them until they
land.

| Endpoint | Status |
|---|---|
| `/browser/*` (navigate, text, html, snapshot, state, tabs, screenshot, click, fill, type, eval, drag, scroll, press, wait, …) | implemented |
| `/pool/*` | implemented |
| `/lease/*`, `/health`, `/metrics`, `/ops/render_package` | implemented |
| `/browser/fill_react`, `/browser/fill_date`, `/browser/paste_atomic` | stub (501) |
| `/browser/restore_checkpoint`, `/browser/vault_login` | stub (501) — FormGuard is parked (see `rust/src/browser/tabs.rs`) |
| `/browser/som_screenshot`, `/browser/diff_state` | stub (501) |

MCP (`--mcp-stdio`, 10 tools): `tabs` and `upload` are functional;
`navigate` validates the URL only; `snapshot`, `diff`, `click_ref`,
`fill_ref`, `order`, `screenshot_marks`, and `eval` currently return
guidance errors pointing at the equivalent HTTP endpoints.

## Files

| File | Purpose |
|---|---|
| `rust/` | The bridge: axum app, all endpoints, tab LRU, lease manager, rate limits, MCP server |
| `rust/systemd/vm-bridge-rs.service` | systemd unit (localhost:8899, auto-restart, watchdog) |
| `docs/chromebook-vm-setup.md` | The host VM's overall setup: hardware, display, services, secrets handling |
| `agent/` | Orchestrator-side tooling: hardened SSH wrapper, job protocol, rsync transfer (no secrets; host via `CB_HOST`) |
| `.github/workflows/vm-bridge-rs.yml` | CI: build, test, clippy, fmt |

## Setup

1. Build the release binary: `cargo build --release` (in `rust/`).
2. Set `BRIDGE_API_KEY` in `~/.config/vm-bridge/env` (mode 600; see the systemd
   unit's `EnvironmentFile`; never hardcode it).
3. Copy your Chrome profile to the configured profile dir so signed-in
   sessions are available.
4. Install the systemd unit: copy `rust/systemd/vm-bridge-rs.service` to
   `~/.config/systemd/user/`, then `systemctl --user enable --now vm-bridge-rs.service`.

Every request carries `X-API-Key: <key>`, except `/health`.

## Security notes

- Binds to 127.0.0.1 only. Do not expose it to a LAN or the internet without
  an authenticated tunnel in front of it.
- The key lives in an EnvironmentFile with mode 600, never in the code.
- Persistent profile copies mean anyone with bridge access inherits the
  browser's sessions. Treat the key like a password.
- Scoped keys (`VM_BRIDGE_KEYS__READ/ACT/ADMIN`) provide least-privilege
  access; `BRIDGE_API_KEY` grants full scope for drop-in compatibility.
- SSRF firewall: navigation URLs are validated (http/https only; loopback,
  link-local, private, CGNAT, and IPv6 unique-local ranges blocked).
  Residual risk (audit 40 MED-2): DNS-rebinding TOCTOU — the bridge resolves
  the host, then Chrome resolves it again at navigate time, so a hostile DNS
  zone could serve a benign IP to the check and a private IP to Chrome.
  Accepted for a single-operator bridge; do not point the bridge at
  untrusted DNS.
