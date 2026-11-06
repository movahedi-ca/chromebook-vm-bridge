# vm-bridge

[![Architecture diagram](https://gitdiagram.com/diagram-badge.svg)](https://gitdiagram.com/movahedi-ca/chromebook-vm-bridge?utm_source=readme&utm_medium=badge)

**A headed Chrome browser on a Linux VM, exposed as an HTTP API (and MCP tools) so agents can drive a real browser with the user’s own signed-in sessions — without touching the user’s own machine.**

Built as a single Rust static binary (axum + tokio + chromiumoxide) for a Debian VM on an ASUS Chromebook (MrChromebox firmware), reached over Tailscale. The bridge binds to **localhost only**; remote access goes through an SSH tunnel or the tailnet.

Version: **2.0.0** · License: **MIT** · MSRV: **1.87**

---

## Overview

vm-bridge solves a practical problem for agent operators: many workflows need a real, headed browser with persistent logins (cookies, OAuth sessions), human-like input, and screenshots — but running that browser on the orchestrator’s laptop is fragile and mixes automation with personal browsing.

The bridge runs on a dedicated Linux VM, spawns and supervises its own headed Chromium against a **dedicated automation profile**, and exposes a JSON HTTP API (plus an optional MCP stdio server). Agents on another machine call the API over an authenticated tunnel. The user’s daily Chrome profile is never touched.

Intended users are developers and operators who run automated browser work (form filling, multi-tab research, visual checks) and need isolation, session persistence, and a clear security boundary.

---

## Key features

- **Persistent headed Chromium** via CDP against a copy of the user’s real profile (cookies/sessions kept; saved passwords deliberately excluded from the seed).
- **REST API** for navigate, click, type, fill, drag, scroll, wait, tabs, screenshots, DOM/AX snapshots, eval, order-smart element resolution, and more.
- **Context pool** (max 3, LRU) for parallel background work.
- **Lease manager** for exclusive sessions.
- **Human-like mouse movement** with eased trajectories (X11/XTest).
- **Hardening**: X-API-Key auth (scoped keys: read / act / admin), per-path token-bucket rate limiting, body-size caps, JSON request logging, SSRF firewall on navigation URLs, crash watchdog with auto-relaunch.
- **MCP server** (`--mcp-stdio`) with tools for agent integration.
- **Perception plane**: accessibility snapshot with stable `[ref]` handles, screenshot comparison, visual assertions.
- **Swappable browser backend** (`BrowserBackend` trait); Chrome is the first implementation.
- **Orchestrator-side tooling** (`agent/`): hardened SSH wrapper, job protocol, rsync transfer, typed CLI.

---

## Project status

| Area | Status |
|------|--------|
| Core browser routes (`/browser/*`) | Implemented |
| Pool, lease, health, metrics, ops | Implemented |
| MCP (`--mcp-stdio`) | Partial — `tabs`, `upload`, `navigate` functional; others return guidance toward HTTP |
| `/browser/fill_react`, `fill_date`, `paste_atomic` | Stub (`501 not_implemented`) |
| `/browser/restore_checkpoint`, `vault_login` | Stub — FormGuard parked |
| `/browser/som_screenshot`, `diff_state` | Stub |
| Resource blocking / failure screenshots | Planned, not yet implemented |

Stubs return a consistent `501 not_implemented` shape so contract tests can distinguish “not built” from “broken.” Do not plan workflows against stub endpoints until they land.

---

## Requirements

### On the VM (bridge host)

- Linux with X11 (reference: Debian 13 on Chromebook with MrChromebox firmware).
- Display available (default `:1`).
- Chromium / Google Chrome binary.
- Rust **1.87+** to build from source.
- Optional but typical: Tailscale (or other private network), TigerVNC / noVNC for remote viewing.

### On the orchestrator (agent side)

- SSH access to the VM (`CB_HOST`).
- Bash, `rsync`, `curl` for the `agent/` scripts.
- Optional: `GEMINI_BIN` for order-smart / move-ai helpers.

Hardware note: the reference host is an ASUS Chromebook C433T with limited RAM; sustained parallel virtual desktops are capped by design (see `docs/virtual-desktops.md`).

---

## Installation

### Build the bridge

```bash
git clone https://github.com/movahedi-ca/chromebook-vm-bridge.git
cd chromebook-vm-bridge/rust
cargo build --release
# binary: target/release/vm-bridge
```

### Configure secrets and paths

```bash
mkdir -p ~/.config/vm-bridge
# mode 600 — never commit this file
cat > ~/.config/vm-bridge/env << 'EOF'
BRIDGE_API_KEY=your-long-random-key
# Optional scoped keys (least privilege):
# VM_BRIDGE_KEYS__READ=...
# VM_BRIDGE_KEYS__ACT=...
# VM_BRIDGE_KEYS__ADMIN=...
EOF
chmod 600 ~/.config/vm-bridge/env
```

Seed a **dedicated automation profile** (never the daily profile). Do **not** copy `Login Data` / `Login Data-journal` — with `--password-store=basic` those are plaintext SQLite and must not live on the bridge host. Cookies (signed-in sessions) are what the seed is for.

### systemd (user unit)

```bash
mkdir -p ~/.config/systemd/user
cp rust/systemd/vm-bridge-rs.service ~/.config/systemd/user/
# Adjust ExecStart / WorkingDirectory if your binary path differs
systemctl --user daemon-reload
systemctl --user enable --now vm-bridge-rs.service
```

The unit binds `127.0.0.1:8899`, uses `EnvironmentFile` for keys, sets `DISPLAY=:1`, and enables systemd notify + watchdog.

### Orchestrator tooling

```bash
export CB_HOST="user@100.x.y.z"   # Tailscale or other reachable host
export CB_DIR="$HOME/chromebook-vm"
# Optional: CB_KEY, CB_PROXY_CMD, CB_PINNED_HOSTS, GEMINI_BIN
```

See `agent/README.md` for SSH pinning, job protocol, and transfer helpers.

---

## Quick start

1. Start the service on the VM (or run the binary directly):

   ```bash
   # via systemd
   systemctl --user start vm-bridge-rs.service

   # or foreground
   BRIDGE_API_KEY=... ./target/release/vm-bridge
   ```

2. From the orchestrator, open a tunnel (example):

   ```bash
   ssh -N -L 8899:127.0.0.1:8899 "$CB_HOST"
   ```

3. Health check:

   ```bash
   curl -s http://127.0.0.1:8899/health
   ```

4. Navigate and snapshot (every request except `/health` needs the key):

   ```bash
   curl -s -X POST http://127.0.0.1:8899/browser/navigate \
     -H "X-API-Key: $BRIDGE_API_KEY" \
     -H "Content-Type: application/json" \
     -d '{"url":"https://example.com"}'

   curl -s "http://127.0.0.1:8899/browser/snapshot" \
     -H "X-API-Key: $BRIDGE_API_KEY"
   ```

5. MCP mode (stdio, no HTTP listener):

   ```bash
   ./target/release/vm-bridge --mcp-stdio
   ```

---

## Configuration

Configuration is loaded from environment variables (prefix `VM_BRIDGE_`) and optional `~/.config/vm-bridge/config.toml`. The process **fails fast** if required API keys are unset.

| Setting | Default | Notes |
|---------|---------|--------|
| `VM_BRIDGE_BIND_ADDR` | `127.0.0.1` | Non-loopback refused unless `VM_BRIDGE_ALLOW_NON_LOOPBACK=1` |
| `VM_BRIDGE_BIND_PORT` | `8899` | |
| `BRIDGE_API_KEY` | (required) | Full-scope key for drop-in clients |
| `VM_BRIDGE_KEYS__READ` / `__ACT` / `__ADMIN` | optional | Scoped least-privilege keys |
| `VM_BRIDGE_DISPLAY` / `DISPLAY` | `:1` | X11 display |
| `VM_BRIDGE_CHROME_BIN` | `google-chrome-stable` | |
| `VM_BRIDGE_PROFILE_DIR` | resolved via `dirs` | Dedicated automation profile |
| Watchdog / body limits / artifact TTL | see `rust/src/config.rs` | |
| `VM_BRIDGE_BLOCKED_RESOURCE_PATTERNS` | empty | Optional CDP resource blocking |

Paths resolve via the `dirs` crate; no hardcoded `/home/auto`.

---

## API surface (summary)

All JSON over HTTP. Auth: `X-API-Key` header (except `/health`). Unmatched routes return honest JSON `404`.

| Group | Examples |
|-------|----------|
| Health / ops | `GET /health`, `GET /metrics`, `POST /ops/render_package` |
| Browser read | `/browser/text`, `html`, `snapshot`, `state`, `tabs`, `screenshot`, `screenshot/hash` |
| Browser input | `/browser/navigate`, `click`, `click_at`, `click_node`, `fill`, `type`, `type_verified`, `press`, `drag`, `scroll`, `wait`, `eval`, `batch` |
| Advanced | `/browser/order`, `autocomplete`, `assert_visual`, `screenshot/compare`, `upload`, … |
| Pool | `/pool/acquire`, `release`, `navigate`, `click`, `fill`, `eval`, `order`, `snapshot`, `status`, … |
| Lease | `/lease/acquire`, `renew`, `release`, `status` |

Expensive paths (`/browser/drag`, `/browser/eval`, `/browser/script`, …) are token-bucket limited. Global request timeout is 120 seconds.

Navigation URLs pass an SSRF firewall (http/https only; loopback, link-local, private, CGNAT, and IPv6 unique-local ranges blocked). Residual DNS-rebinding TOCTOU is documented and accepted for a single-operator deployment.

---

## Agent tooling (`agent/`)

Runs on the **orchestrator**, not on the VM.

| Script | Role |
|--------|------|
| `cb-ssh.sh` | Hardened SSH: multiplexing, stale-master recovery, exit-code classification, retry on transient errors only, pinned host key, audit log |
| `cb-job.sh` | Job protocol: `ping`, `doctor`, `fetch`, `run`, `bg` / `status` / `get` / `kill` / `rm`, `activity` |
| `cb-xfer.sh` | `rsync` push/pull with sha256 verification |
| `bridge-cli.py` | Typed CLI over the HTTP API |

Design notes: one SSH call per task; kill on the local wrapper does **not** stop the remote process — use `cb-job.sh kill`. Large artifacts move via `cb-xfer.sh`, not text transport. See `agent/README.md`.

---

## Architecture

```
┌──────────────────┐     SSH / tailnet      ┌─────────────────────────────────┐
│  Orchestrator    │ ─────────────────────► │  Chromebook VM (Debian)         │
│  agent/ scripts  │                        │                                 │
│  bridge-cli.py   │     localhost:8899     │  vm-bridge (Rust)               │
└──────────────────┘ ◄───────────────────── │   · axum HTTP + optional MCP    │
                                            │   · auth / rate limit / SSRF    │
                                            │   · lease + pool                │
                                            │   · browser supervisor          │
                                            │        │ CDP                    │
                                            │        ▼                        │
                                            │   Chromium (headed, :1)         │
                                            │   profile: chrome-automation    │
                                            └─────────────────────────────────┘
```

**Major modules** (under `rust/src/`):

| Module | Responsibility |
|--------|----------------|
| `main` | Startup, router, graceful shutdown, MCP vs HTTP mode |
| `config` | Env + TOML config, key layout, path resolution |
| `auth` | X-API-Key scopes (read/act/admin) |
| `middleware` | Rate limit, body cap, lease gate, request log |
| `browser/` | CDP attach, Chrome backend, supervise/relaunch, geometry, verify |
| `input/` | X11 mouse (human-like trajectories) |
| `interact/` | React fills, autocomplete, upload |
| `perception/` | AX snapshot, Set-of-Marks, DOM/screenshot diff |
| `pool/` | Parallel context pool |
| `leases` | Exclusive session leases |
| `routes/` | HTTP handlers (browser, pool, lease, ops, batch, advanced) |
| `mcp/` | MCP stdio tools |
| `watchdog` | Health probes + systemd notify |
| `vault` / `privy` | FormGuard-related (parked) |

The bridge **owns** the browser process: spawn at start, reconnect on death with capped backoff. Do not relaunch Chrome by hand.

---

## Project structure

```
.
├── README.md
├── LICENSE
├── rust/                       # Bridge binary
│   ├── Cargo.toml
│   ├── src/
│   ├── systemd/vm-bridge-rs.service
│   └── vendor/                 # Patched chromiumoxide 0.7.0
├── agent/                      # Orchestrator-side SSH / job / transfer tools
├── docs/
│   ├── chromebook-vm-setup.md  # Host hardware, display, services, secrets
│   ├── virtual-desktops.md
│   ├── agent-interaction.md
│   └── …
├── vm/                         # VM-side helpers (e.g. virtual desktops)
├── waves/                      # Work-stream notes
├── linkedin-outreach/          # Separate automation experiments
└── .github/workflows/vm-bridge-rs.yml
```

---

## Development

```bash
cd rust

cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
cargo test --locked
```

CI (`.github/workflows/vm-bridge-rs.yml`) runs format, clippy, release build, unit tests, `cargo audit`, and a check that the vendored chromiumoxide patch is still aligned with upstream 0.7.x. Agent shell scripts are ShellChecked.

Contract / live tests (navigate → snapshot against a real Chrome) run on the VM, not in GitHub Actions.

Release profile: `opt-level = 3`, LTO, single codegen unit, strip.

---

## Security and operational notes

- **Bind loopback only.** Do not expose the plain-HTTP API to a LAN or the internet without an authenticated tunnel.
- **API key** lives in an EnvironmentFile mode 600; never in source, chat, or argv.
- **Scoped keys** (`READ` / `ACT` / `ADMIN`) provide least privilege; `BRIDGE_API_KEY` grants full scope for compatibility.
- **Profile seed**: never copy `Login Data` into the automation profile.
- **SSRF firewall** on navigate; residual DNS-rebinding TOCTOU is accepted for single-operator use — do not point the bridge at untrusted DNS.
- **`cb-job.sh run/bg`** is an unsandboxed remote shell as the VM user (passwordless sudo on the reference host). Accepted only while the operator is the sole delegator; revisit before less-trusted parties can delegate work.
- **Graceful shutdown**: SIGTERM/Ctrl-C drains in-flight requests (~20s) so Chrome is not orphaned.
- Artifact directories are created mode 700 where the filesystem allows.

Treat bridge access like full control of the automation browser sessions.

---

## Limitations

- Linux + X11 only (no Wayland-first path documented).
- Several advanced endpoints remain stubs (`501`).
- MCP tool coverage is incomplete relative to the HTTP surface.
- Single-operator security model; multi-tenant use is out of scope without further hardening (sandbox user, fixed command dispatcher, non-loopback TLS, etc.).
- Virtual desktops (vd1–vd10) are managed outside the bridge binary; the bridge’s own Chrome lives on `:1` / CDP `9222`.

---

## Contributing

1. Open an issue describing the change.
2. Keep commits focused; do not commit secrets or profile data.
3. Run `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` in `rust/`.
4. Prefer updating the relevant docs under `docs/` or package READMEs.
5. Submit a PR against `main`.

Security findings can be reported via the repository issue tracker or privately to the maintainer.

---

## License

MIT License. Copyright (c) 2026 movahedi-ca. See [LICENSE](LICENSE).

---

## Support and references

| Resource | Link |
|----------|------|
| Source | https://github.com/movahedi-ca/chromebook-vm-bridge |
| Issues | https://github.com/movahedi-ca/chromebook-vm-bridge/issues |
| VM setup | [docs/chromebook-vm-setup.md](docs/chromebook-vm-setup.md) |
| Virtual desktops | [docs/virtual-desktops.md](docs/virtual-desktops.md) |
| Agent interaction | [docs/agent-interaction.md](docs/agent-interaction.md) |
| Agent tooling | [agent/README.md](agent/README.md) |
| systemd unit | [rust/systemd/vm-bridge-rs.service](rust/systemd/vm-bridge-rs.service) |

The live deployment details (Tailscale, VNC, mirror feeds, `~/jobs` layout) are documented in `docs/chromebook-vm-setup.md`. The code and those docs are the authoritative description of the shipped system.
