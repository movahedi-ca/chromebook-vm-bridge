# vm-bridge

A FastAPI + Playwright bridge that exposes a headed Chrome browser running on a
Linux VM as an HTTP API, so agents can drive a real browser (with the user's own
signed-in sessions) without touching the user's own machine.

Built for a Debian VM on an ASUS Chromebook (MrChromebox firmware), reached over
Tailscale. The bridge binds to localhost on the VM; remote access goes through
an SSH tunnel or the tailnet proxy.

## What it does

- Drives a persistent headed Chromium via Playwright against a copy of the
  user's real Chrome profile, so logins persist.
- REST endpoints for navigation, clicking, typing, screenshots, DOM snapshots,
  tab management, and an order-smart element resolver (deterministic
  nodeId-first, with optional LLM escalation via accessibility snapshot).
- Headless context pool (max 3, LRU) for parallel background work, with
  resource blocking and failure screenshots.
- Human-like mouse movement (`human_mouse.py`) with eased trajectories, for
  interactions that need to look natural.
- Hardening: X-API-Key auth, per-path token-bucket rate limiting, JSON request
  logging, body-size caps, lease manager for exclusive sessions, SSIM
  visual-assertion endpoint, crash watchdog with auto-relaunch.

## Files

| File | Purpose |
|---|---|
| `app.py` | The bridge: FastAPI app, all endpoints, tab LRU, lease manager, rate limits |
| `human_mouse.py` | Eased mouse trajectories (Mona Lisa smoothness-gated) |
| `vm-bridge.service` | systemd unit (localhost:8899, auto-restart, watchdog) |

## Setup

1. Copy the files to the VM, install Python deps (`fastapi`, `uvicorn`,
   `playwright`, plus a Playwright Chromium build).
2. Set `BRIDGE_API_KEY` in the environment (see the systemd unit's
   `EnvironmentFile`; never hardcode it).
3. Copy your Chrome profile to the configured profile dir so signed-in
   sessions are available.
4. `systemctl enable --now vm-bridge.service`

Every request carries `X-API-Key: <key>`, except `/health`.

## Security notes

- Binds to 127.0.0.1 only. Do not expose it to a LAN or the internet without
  an authenticated tunnel in front of it.
- The key lives in an EnvironmentFile with mode 600, never in the code.
- Persistent profile copies mean anyone with bridge access inherits the
  browser's sessions. Treat the key like a password.
