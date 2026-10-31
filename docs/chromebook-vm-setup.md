# Chromebook VM setup (overall)

How the host machine behind `vm-bridge` is put together. This documents the
current production setup (October 2026) so the box can be rebuilt or audited.
No secrets live in this repo: the API key is deployed out-of-band (see
"Secrets" below) and values are shown as `<placeholders>`.

## Hardware and OS

- ASUS Chromebook C433T with MrChromebox firmware, running **Debian 13
  (trixie)** as the main OS.
- Single user `auto` with passwordless sudo. Home directory holds everything;
  nothing durable lives outside `~` (system paths are treated as ephemeral).
- RTC wake alarm does not work on this firmware: if the box loses power, the
  only recovery is the physical power button.

## Network

- Joined to a private **Tailscale tailnet**; no inbound ports are exposed to
  the internet. All remote access goes over the tailnet via SSH.
- DNS and egress filtering are managed at the network level (Control D); if
  social sites fail from the VM but work elsewhere, check per-device profile
  assignment before assuming an account-level block.

## Display and browser

- Xorg runs headed on **display `:1`**. The bridge **spawns and supervises
  its own headed Chrome** on `:1` — there is no externally-launched browser
  and no separate Chrome keeper unit. One daemon owns the whole stack: if
  Chrome exits, the bridge relaunches it with capped backoff and reconnects,
  no manual step.
- The browser runs against a **dedicated automation profile**
  (`~/.config/chrome-automation`), seeded once from the main profile copy so
  signed-in sessions persist. Re-sync the seed after any new login,
  otherwise sessions go stale. The bridge never touches the main profile.
- The spawn/supervise layer is a **swappable backend** behind a
  `BrowserBackend` trait (launch, health check, kill, CDP URL); Chrome is
  the first implementation. A future engine plugs in as a second backend
  without touching routes, auth, or supervision.
- Chrome launches locked-down: no `--no-sandbox`, full site isolation,
  `--disable-dev-shm-usage`, `--password-store=basic`, window pinned to
  `1920x1080` at `0,0`, remote-debugging port `9222`.
- Remote viewing: TigerVNC on `5901` (display `:1`), plus noVNC services for
  browser-based viewing.
- Physical-screen mirror: an x11vnc feed on `localhost:5902` mirrors `:1`
  onto the Chromebook's own display (`:0`), so automation is always visible
  on the device. An always-on-top activity panel announces background work.
- Rule: never terminate Chrome remotely. Close/open tabs, keep the browser
  process alive. (If the bridge's own Chrome dies, the supervisor handles
  it — do not relaunch it by hand.)

## Systemd services (user-level, `~/.config/systemd/user/`)

| Unit | Purpose |
|---|---|
| `vm-bridge-rs.service` | The Rust bridge on `localhost:8899`. Spawns and supervises its own headed Chrome (no external browser). `Restart=on-failure`, `DISPLAY=:1`, `RUST_LOG=info`. API key via `EnvironmentFile`. |
| `vnc.service`, `novnc.service`, `novnc-kiosk.service` | Remote desktop viewing. |
| `mirror-feed.service`, `mirror-display.service` | Physical-screen mirror of `:1`. |
| `caddy.service`, `funnel.service` | Local reverse-proxy/tunnel helpers. Agents never use the public tunnel URL. |

## `~/jobs` layout

- `vm-bridge-rs/` — live Rust bridge build tree (see `rust/` in this repo).
  The `vm-bridge-rs` systemd unit runs it on `:8899`; it spawns and
  supervises its own Chrome.
- `fetch.py` — static page fetcher (curl with Chrome TLS impersonation),
  fenced with responsible-use rules: denylist, robots.txt honored,
  per-host rate limiting, audit log.
- Everything else under `~/jobs/` is scratch: one-off job scripts, probes,
  and experiment outputs. Treat it as disposable.

## Secrets

- The bridge API key is read from the `BRIDGE_API_KEY` environment variable
  only. On the VM it is deployed once to `~/.config/vm-bridge/env`
  (mode `600`) and referenced from the unit's `EnvironmentFile`. It is never
  committed, never pasted into chat, and never transported off the box.
- Deploy scripts must never overwrite the env file.

## Agent-side tooling (`agent/`)

The orchestrator side keeps its tooling in this repo under `agent/` (audit
41 LOW-4: this section used to say it was not in the repo): an SSH wrapper
over the tailnet, a job runner (`ping`/`ls`/`fetch`/`run`/`bg`/`status`/`get`/`kill`),
a file-transfer helper, and a CLI whose responses end with a
`__BRIDGE_DONE__:<curl_exit>:<http_code>` trailer so transient curl failures
(exits 7/28/52) can be retried once with backoff. Binary responses (e.g.
screenshots) are transferred as files, never parsed as text. See
`agent/README.md` for usage.

Parked features (designed, not yet wired — see the code comments, not the
marketing): FormGuard vault, Set-of-Marks overlay, DOM diffing, and the
playbook engine are compiled but unreachable behind `#[allow(dead_code)]`.
Do not plan workflows against them.

## Operational rules (standing)

1. Take one full-desktop screenshot at the start of every visual browser
   session; re-shoot when the page state looks unexpected. Popups and toolbar
   chips are invisible to DOM inspection.
2. The "Restore pages? / Chrome didn't shut down correctly" dialog is always
   closed on sight (never Restore); likewise clear any "Verify that it's you"
   chip or surface it to the user.
3. Never do select-all+paste without first verifying the clipboard actually
   holds the content (e.g. byte count via `xclip`), and screenshot after
   pasting on anything that matters.
4. Killing the local SSH wrapper does **not** stop the remote process: it
   keeps running on the VM. Stop remote work with `pkill` on the VM and
   verify with `pgrep`.
5. The bridge's expensive paths (`/browser/drag`, `/browser/eval`,
   `/browser/script`) are token-bucket limited (1 token per 3s, burst 5).
   "Max speed" automation is throttled by this, not by the client.

## Rust bridge

`rust/` in this repo is the live bridge (axum + tokio + chromiumoxide):
scoped API keys, SSRF firewall, FormGuard vault, perception plane (AX
snapshot / Set-of-Marks / DOM diffing), playbook engine, and an MCP server
(`--mcp-stdio`, 10 tools). CI builds and tests it on every push. The bridge
spawns and supervises its own headed Chrome via a swappable `BrowserBackend`
trait (Chrome is the first backend); it also vendors a patched
chromiumoxide 0.7.0 (`rust/vendor/chromiumoxide`) whose WS layer skips
unparseable messages (CDP protocol drift) instead of killing the connection.
