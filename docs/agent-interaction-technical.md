# Agent interaction technical reference: chromebook-vm

Companion to `agent-interaction.md` (the conceptual overview). This file
is the mechanics: endpoints, auth, ports, profiles, and exact commands.
No secrets live here; placeholders are shown as `<...>`.

## Machine

- Host: Asus Chromebook Flip C433T, Debian 13, user `auto`
- Tailnet: `<tailnet-ip>` (agents only; never the public Funnel URL)
- SSH: `~/.ssh/id_ed25519`, multiplexed (ControlMaster/ControlPersist
  600s), host key pinned at `~/.ssh/known_hosts.d/chromebook-vm-pinned`
- Proxy: hatch-managed Tailscale egress; auth minted fresh per call by
  the proxy helper, never cached

## Display / port / profile map

| # | Display | VNC | CDP | Chrome profile | Owner |
|---|---|---|---|---|---|
| main | `:1` | 5901 (via noVNC 6080) | bridge-managed | `~/.config/chrome-automation` | vm-bridge (spawns + supervises) |
| mirror | `:0` | 5902 (localhost) | n/a | n/a | mirror-feed/mirror-display services |
| vd1 | `:2` | 5903 | `127.0.0.1:9333` | `~/.config/chrome-vd1` | vd-chrome.sh |
| vd2 | `:3` | 5904 | `127.0.0.1:9334` | `~/.config/chrome-vd2` | vd-chrome.sh |
| ... | ... | ... | ... | ... | ... |
| vd10 | `:11` | 5912 | `127.0.0.1:9342` | `~/.config/chrome-vd10` | vd-chrome.sh |

Deterministic mapping: vdK -> display `:(K+1)`, VNC `(5902+K)`, CDP
`127.0.0.1:(9332+K)`, profile `~/.config/chrome-vdK`. vd scripts:
`~/jobs/vd-start.sh [N]` (default 2, max 10), `~/jobs/vd-stop.sh [N|all]`,
`~/jobs/virtual-desktops/vd-chrome.sh vdK [url]`.

## vm-bridge API

- Base: `http://127.0.0.1:8899` (localhost-bound on the VM; reached via
  `cb-job.sh run` / `bridge-cli`, never directly over the network)
- Auth: `x-api-key` header, key read VM-side from
  `~/.config/vm-bridge/api-key` (mode 600). Missing/wrong key -> 401.
  `GET /health` is exempt.
- `POST /browser/eval`: **single-line JS only.** Multi-line payloads
  are rejected; keep evals to one line.
- Rate limits: 429 on excess. Body caps: 413 on oversize.
- Leases: `POST /browser/lease/acquire|release`, `GET
  /browser/lease/status`. When a lease is held, mutating calls need
  `X-Lease-Id`; reads never need one. Stale-lease acquire returns 423.
- Screenshots: `POST /browser/screenshot/delta` (inter-frame PNG
  patches; first call is a keyframe), `GET /browser/screenshot/hash`
  (perceptual hash, ~0.08s, 0 bytes), `POST
  /browser/screenshot/region`, `POST /browser/screenshot/compare`.
- Scripting: `POST /browser/script` accepts `{"steps": [...]}`; each
  step has a `verify` field (`blind` | `hash` | `auto`; auto = hash for
  mouse, blind for keyboard). Accepts `Content-Encoding: gzip`.
- Natural-language resolution: `POST /browser/order`
  (`{"action":"click","target":"Save button"}`); `/order-smart`
  escalates to Gemini with the accessibility snapshot on failure.
- CDP-direct: `POST /browser/click_node` (`backendNodeId`), no selector
  guessing.
- Headless pool: `POST /browser/pool/acquire|navigate|text|eval|click|
  fill|order|snapshot|release|status` (max 3 contexts, LRU, lease-exempt).
- Ops: `GET /metrics`, `POST /browser/assert_visual` (SSIM region
  asserts), main-browser failure screenshots land in
  `~/jobs/vm-bridge/failures/`.
- MCP: 10 tools over stdio via rmcp (Phase 5).

## bridge-cli command reference

Typed client; JSON built in code, auth handled VM-side (key never
crosses SSH argv). All calls route through `cb-job.sh run` over SSH.

```
bridge-cli health | state
bridge-cli navigate <url> | text | snapshot | shot /tmp/shot.png
bridge-cli click "<selector>" | fill "<selector>" "<text>"
bridge-cli type "<selector>" "<text>" | press "<selector>" <Key>
bridge-cli eval "<single-line JS>"
bridge-cli wait "<selector>" <timeout_ms>
bridge-cli order click "<target>" | order fill "<label>" textbox "<text>"
bridge-cli order-smart click "<target>"
bridge-cli script steps.json
bridge-cli tabs | tab-new <url> | tab-switch <tab_id>
bridge-cli click-node <backendNodeId>
bridge-cli pool acquire [ttl_s] [trace] | pool navigate|text|eval|click|fill|order|snapshot <ctx> ...
bridge-cli pool release <ctx> | pool status
bridge-cli lease acquire [client] [ttl_s] | lease release <id> | lease status
bridge-cli --lease <id> <command>     # act under a lease
bridge-cli --visible <command>        # drive the real OS cursor via XTest
bridge-cli call <METHOD> <path> '<json>'   # raw escape hatch
```

## cb-job.sh subcommands

```
cb-job.sh ping | preflight | doctor | top | wake [MIN]
cb-job.sh ls | bg NAME 'CMD' | status NAME | get NAME [LINES] | kill NAME | rm NAME
cb-job.sh fetch URL [MAXCHARS]        # -> JSON {url,title,description,text,links}
cb-job.sh fetch-lp URL                # Lightpanda isolated unsigned fetch (opt-in)
cb-job.sh run 'CMD'                   # synchronous, 600s cap
```

`preflight` (not bare `ping`) is the pre-flight for every flow. `bg`
refuses when disk is under 1G free (exit 98) or 4 jobs run (exit 97).
Job names: `[A-Za-z0-9_-]+`, starting alphanumeric.

## cb-ssh.sh exit codes

60 proxy-down, 61 host-down, 62 auth, 63 host-key-changed (loud),
64 keepalive timeout, 65 unknown, 70 proxy-helper missing, 71 bad key
perms. Transient errors retry with capped backoff + jitter (never
auth/host-key). Every call logs to
`~/workspace/chromebook-vm/conn-audit.log`.

## File transfer

`cb-xfer.sh` (rsync --checksum --partial --compress, sha256-verified).
Raw rsync over the tunnel is broken; the fallback is tar-over-ssh:

```sh
# agent -> VM
tar cz -C ~/workspace/linkedin-outreach . | ~/workspace/chromebook-vm/cb-ssh.sh 'mkdir -p ~/incoming/lo && tar xz -C ~/incoming/lo'
```

## Visual desktop (cbvnc.sh)

```sh
cbvnc.sh shot                 # -> ~/workspace/chromebook-vnc/shot.png (nudges past 3/10min)
cbvnc.sh click X Y            # 1920x1080 desktop pixels
cbvnc.sh key <spec>           # xdotool key spec
cbvnc.sh type "text"          # quoting-safe typing
cbvnc.sh sh 'CMD'             # run in the VNC session env
cbvnc.sh health               # tunnel:ok|fail vnc:ok|fail
```

Click precision: capture full-res
(`cb-ssh.sh 'DISPLAY=:1 scrot -z -o /tmp/x.png'`, download), locate with
visual grounding (0-1000 normalized), convert: `x/1000*1920`,
`y/1000*1080`. Batch xdotool sub-steps in one `sh` call.

## Troubleshooting

| Symptom | Check |
|---|---|
| `preflight` fails | Machine off or WiFi down: physical power button is the only fix |
| 401 from bridge | `x-api-key` missing/wrong; key lives VM-side only |
| 423 on mutating call | Lease held: acquire or pass `--lease <id>` |
| eval rejected | Multi-line JS: collapse to a single line |
| Send button stays disabled (LinkedIn) | execCommand fill, then keyboard-event burst (keydown/keypress/keyup + InputEvent), verify `disabled:false` before clicking |
| `shot` black | `:1` down: `systemctl --user is-active vnc.service` |
| Profile lock | Visual Chrome holds it: quit via `key Alt+F4` or use a separate `--user-data-dir` |
| DNS failures on fetches | `getent hosts example.com`; resolv.conf must list Control D resolvers then 1.1.1.1, immutable |
| Phantom lease (acquire hangs, status says unlocked) | Known bridge degradation mode; retry once, then report |
