# agent/ — orchestrator-side tooling for vm-bridge

These scripts run on the **orchestrator** (the machine driving the work), not
on the Chromebook VM. They are the client side of the bridge: SSH transport,
a compact job protocol, file transfer, and a typed CLI for the bridge's HTTP
API. See `../docs/chromebook-vm-setup.md` for the VM side.

Adapted from a private deployment; host-specific values were replaced with
environment variables. No secrets are stored in these files.

## Files

| File | Purpose |
|---|---|
| `cb-ssh.sh` | Hardened SSH wrapper: multiplexing with stale-master recovery, failure classification into distinct exit codes (proxy-down vs host-down vs auth), retry with capped backoff on transient errors only, pinned host key, per-call audit log. |
| `cb-job.sh` | Compact job protocol over one SSH call per task: `ping`, `doctor`, `fetch` (URL to text/JSON), `run` (600s cap), `bg`/`status`/`get`/`kill`/`rm` (background jobs with flock-based state), `activity` (on-screen status panel). |
| `cb-xfer.sh` | `rsync` push/pull between orchestrator and VM with sha256 integrity verification after every transfer. |

## Setup

```bash
export CB_HOST="user@host"          # e.g. auto@100.x.y.z on your tailnet
export CB_DIR="$HOME/chromebook-vm" # where these scripts live (default)
# Optional:
export GEMINI_BIN="$HOME/bin/gemini_generate.py"  # enables order-smart / move-ai
```

1. Point `CB_HOST` at the VM. The original deployment reaches it over a
   Tailscale tailnet via a `ProxyCommand` helper; set `CB_PROXY_CMD` to your
   own helper, or leave it empty for direct SSH.
2. The SSH client key (`~/.ssh/id_ed25519` by default, `CB_KEY` to override)
   must exist with mode `600`, and its public half must be in the VM user's
   `~/.ssh/authorized_keys`.
3. Pin the VM's host key: `CB_PINNED_HOSTS` defaults to
   `~/.ssh/known_hosts.d/chromebook-vm-pinned`. Capture it from the live
   server once; a changed key fails loudly (possible MITM).
   it on the VM from `~/.config/vm-bridge/api-key` (mode 600), so the key
   never crosses SSH argv and never appears in logs.

## Design notes

- One SSH call per task; results come back as compact text/JSON. Token
  discipline is a feature: batch the work, cap the output.
- `cb-ssh.sh` never retries auth failures or host-key mismatches; it only
  retries transient network errors.
- Killing the local SSH wrapper does **not** stop the remote process. Use
  `cb-job.sh kill` (process-group kill on the VM) for background jobs.
  file transfer (`cb-xfer.sh pull`), not through the text transport.
