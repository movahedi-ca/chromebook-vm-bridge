#!/bin/bash
# cb-ssh.sh - hardened SSH wrapper for chromebook-vm.
#
# Target: $CB_HOST (e.g. auto@100.x.y.z on your tailnet).
# Direct SSH is the default (CB_PROXY_CMD empty). If your network needs a
# proxy, set CB_PROXY_CMD to a helper script (the original deployment used
# a ts_proxy_cmd.py that minted fresh proxy auth per call).
#
# Phase 2A connection hardening (2026-10-29, Worker A):
#  1. Stale multiplex master auto-recovery: ssh -O check before every
#     attempt; a dead socket gets ssh -O exit and the call reconnects
#     transparently. The check/exit sequence runs under an flock so two
#     callers starting at the same moment cannot interleave it.
#  2. Failure classification: ssh exit code + stderr signatures map to human
#     messages and distinct CB_E_* exit codes (60-65), so callers can tell
#     proxy-down from host-down from auth-failed without parsing English.
#     Dead-master signatures ("mux_client_request_session: read from master
#     failed", "client_loop: send disconnect", "Connection closed by
#     <host>") classify as host-down AND are transient, so the retry
#     rebuilds the master via (1).
#  3. Retry with capped exponential backoff + jitter on TRANSIENT errors only
#     (timeouts, proxy hiccups, dead master). Auth failures and host-key
#     mismatches never retry. Schedule: after attempt 1 wait 1000-1999 ms, after attempt 2 wait
#     2000-2999 ms (base doubles per attempt, capped at 8000 ms, plus 0-999 ms
#     jitter). A doomed call adds under 5 s of backoff on top of the attempts.
#  4. known_hosts pinning: StrictHostKeyChecking=yes against
#     ~/.ssh/known_hosts.d/chromebook-vm-pinned (captured 2026-10-29 from the
#     live server's /etc/ssh/ssh_host_*_key.pub). A changed key fails LOUDLY
#     (exit 63). No SSH CA: one box, one user, a CA is overkill.
#  5. Proxy-down fast fail: "Connection closed by UNKNOWN port 65535" maps to
#     exit 60 "egress proxy refused the connection (proxy down or auth
#     failed)". Each attempt fails fast because the proxy answers CONNECT at
#     once (no 20 s wait). No keepalived/VIP failover: the proxy is
#     hatch-managed, not ours. Note: the same signature covers a transient
#     proxy hiccup, so it DOES get the 3-attempt backoff from (3); a truly
#     dead proxy still ends at exit 60 with the plain message.
#  6. ControlPath lives in ~/.ssh/cm (mode 700): cbv-%r@%h:%p.
#  8. Per-call audit line appended to
#     ~/chromebook-vm/conn-audit.log (CB_AUDIT_LOG):
#     "<ts> <subcmd> <dur_ms> <exit> <attempts> <remote-preview>".
# 11. The client key (~/.ssh/id_ed25519) must be mode 600 or we refuse to run.
# 12. ts_proxy_cmd.py must exist and be executable or we fail fast.
#
# Key rotation story (item 11): this link uses ONE static keypair, no agent
# forwarding, BatchMode=yes. To rotate: generate a new key, append the new
# .pub to $CB_HOST:~/.ssh/authorized_keys, verify a call with
# -i <newkey>, then delete the old .pub line and the old private key. The
# pinned known_hosts above is the SERVER's key and is unrelated to this
# client key; re-pin it only on his explicit word (e.g. Chromebook
# reinstall), because a changed host key is otherwise the MITM signal.
#
# Exit codes: 0 on success. When ssh itself failed, one of the CB_E_* codes
# below. When ssh succeeded but the remote command failed, the remote exit
# code passes through unchanged. Caveat: a remote command that exits 255
# while printing ssh-like text on stderr could be misclassified; accepted
# and documented (our remote commands are simple and this has not happened).
#
# Test hooks (env; all default to production values; used by the phase2-A
# sandbox tests, never set in production):
#   CB_CM_DIR         control socket dir (default ~/.ssh/cm)
#   CB_PINNED_HOSTS   pinned known_hosts file (default
#                     ~/.ssh/known_hosts.d/chromebook-vm-pinned)
#   CB_SSH_FLAGS      extra ssh flags, e.g. "-C" (default empty)
#   CB_AUDIT_CMD      subcommand label for the audit log (cb-job.sh exports
#                     cbjob:<cmd>; cb-xfer.sh exports xfer:<mode>)
#   CB_SSH_SOURCE_ONLY=1  source the functions below without running (tests)

# Distinct exit codes (item 2).
E_PROXY=60      # egress proxy refused the connection
E_HOSTDOWN=61   # host unreachable: timeout, refused, no route
E_AUTH=62       # Permission denied (publickey)
E_HOSTKEY=63    # host key changed: LOUD failure, possible MITM
E_TIMEOUT=64    # ssh keepalive detected a dead session
E_SSH=65        # ssh failed, unrecognized signature
E_PROXY_CMD=70  # item 12: ts_proxy_cmd.py missing/not executable
E_KEY=71        # item 11: client key missing or wrong permissions
E_PINNED=72     # item 4: pinned known_hosts file missing
E_MUXDIR=73     # item 6: control socket dir unusable

# Set CB_HOST in your environment, e.g. export CB_HOST="auto@100.x.y.z".
# (Validated after the test hook below so CB_SSH_SOURCE_ONLY=1 still works.)
CB_HOST="${CB_HOST:-}"
CB_KEY="${CB_KEY:-$HOME/.ssh/id_ed25519}"
CB_CM_DIR="${CB_CM_DIR:-$HOME/.ssh/cm}"
CB_PINNED="${CB_PINNED_HOSTS:-$HOME/.ssh/known_hosts.d/chromebook-vm-pinned}"
CB_PROXY_CMD="${CB_PROXY_CMD:-}"  # empty = direct SSH; set to a proxy helper for proxied networks
CB_AUDIT_LOG="${CB_AUDIT_LOG:-$HOME/chromebook-vm/conn-audit.log}"
CB_CP="$CB_CM_DIR/cbv-%r@%h:%p"

die() { # code message
  echo "cb-ssh: $2" >&2
  exit "$1"
}

audit_log() { # code attempts dur_ms
  local code="$1" attempt="$2" dur="$3"
  local sub="${CB_AUDIT_CMD:-ssh-direct}" preview ts
  # Fail loudly if the audit dir is missing (audit 40 MED-10): silently
  # dropping the security-relevant audit trail is worse than noise.
  if [ ! -d "$(dirname "$CB_AUDIT_LOG")" ]; then
    echo "cb-ssh: audit dir missing: $(dirname "$CB_AUDIT_LOG")" >&2
    return 1
  fi
  preview="$(printf '%s' "$REMOTE_PREVIEW" | tr '\n\r' '  ' | cut -c1-60)"
  ts="$(date +%Y-%m-%dT%H:%M:%S%z)"
  {
    flock -x 200
    printf '%s %s %s %s %s %s\n' \
      "$ts" "$sub" "$dur" "$code" "$attempt" "$preview" >>"$CB_AUDIT_LOG"
  } 200>"$CB_AUDIT_LOG.lock"
}

mux_recover() {
  # Item 1 + 6: check the master under an flock; a stale socket is exited
  # so the real call below transparently creates a fresh master. The lock is
  # held only for the check/exit, never for the ssh call itself, so long
  # calls do not serialize each other.
  local lock="$CB_CM_DIR/cbv-mux.lock"
  # NOTE: no 2>/dev/null on this exec line: with no command, exec applies
  # redirections permanently to this shell, so 2>/dev/null here would
  # silence ALL later stderr (including xtrace and error messages).
  exec 9>"$lock" || return 0
  flock -w 15 9 2>/dev/null || { exec 9>&-; return 0; }
  if ! timeout 10 ssh -o ControlPath="$CB_CP" -O check "$CB_HOST" >/dev/null 2>&1; then
    # Double-check before killing: never exit a master on a single flaky
    # check, or we could kill a healthy master out from under an in-flight
    # transfer (observed 2026-10-29: a mid-transfer master death coincided
    # with a concurrent call; root cause was the proxy EAGAIN crash, fixed
    # in ts_proxy_cmd.py, but the double-check stays as cheap insurance).
    sleep 1
    if ! timeout 10 ssh -o ControlPath="$CB_CP" -O check "$CB_HOST" >/dev/null 2>&1; then
      timeout 10 ssh -o ControlPath="$CB_CP" -O exit "$CB_HOST" >/dev/null 2>&1 || true
    fi
  fi
  flock -u 9 2>/dev/null || true
  exec 9>&- || true
}

classify() {
  # Item 2: map stderr text to CB_CODE/CB_MSG. Empty CB_CODE = no signature.
  local text="$1"
  CB_CODE=""; CB_MSG=""
  case "$text" in
    *"REMOTE HOST IDENTIFICATION HAS CHANGED"*|*"Host key verification failed"*)
      CB_CODE=$E_HOSTKEY
      CB_MSG="HOST KEY CHANGED for $CB_HOST: refusing to connect. If the Chromebook was reinstalled, re-pin the key on his explicit word; otherwise treat this as a possible MITM and do not proceed."
      ;;
    *"Permission denied"*)
      CB_CODE=$E_AUTH
      CB_MSG="ssh auth failed for $CB_HOST: the client key was rejected (Permission denied)."
      ;;
    *"Connection closed by UNKNOWN port 65535"*)
      CB_CODE=$E_PROXY
      CB_MSG="egress proxy refused the connection (proxy down or auth failed)."
      ;;
    *"Timeout, server not responding"*|*"Timeout, client not responding"*)
      CB_CODE=$E_TIMEOUT
      CB_MSG="ssh session timed out for $CB_HOST (keepalive detected a dead session)."
      ;;
    *"Connection closed by ${CB_HOST#*@}"*|*"mux_client_request_session: read from master failed"*|*"client_loop: send disconnect"*)
      CB_CODE=$E_HOSTDOWN
      CB_MSG="connection to chromebook-vm dropped mid-session (master died or remote end closed it). The call was retried; if this persists, the network path may be flapping."
      ;;
    *"Connection timed out"*|*"timed out"*|*"No route to host"*|*"Connection refused"*|*"Network is unreachable"*)
      CB_CODE=$E_HOSTDOWN
      CB_MSG="chromebook-vm unreachable ($CB_HOST): connection timed out, refused, or no route. The box may be off, asleep, or off the tailnet."
      ;;
  esac
}

is_hard_fail() {
  # Item 3: never retry these.
  case "$1" in
    *"REMOTE HOST IDENTIFICATION HAS CHANGED"*|*"Host key verification failed"*|*"Permission denied"*)
      return 0 ;;
    *) return 1 ;;
  esac
}

is_transient() {
  # Item 3: retry only these. The mux/dead-master signatures are transient
  # because mux_recover (item 1) cleans the dead socket before the retry,
  # so the retry transparently rebuilds the master.
  case "$1" in
    *"Connection closed by UNKNOWN port 65535"*|*"proxy closed during CONNECT"*|*"CONNECT failed"*|\
    *"timed out"*|*"Connection reset by peer"*|*"ssh_exchange_identification: read:"*|\
    *"mux_client_request_session: read from master failed"*|*"client_loop: send disconnect"*|\
    *"Connection closed by ${CB_HOST#*@}"*)
      return 0 ;;
    *) return 1 ;;
  esac
}

finish_classified() { # rc stderr_text attempts dur_ms
  local rc="$1" text="$2" attempt="$3" dur="$4"
  classify "$text"
  if [ -n "$CB_CODE" ]; then
    audit_log "$CB_CODE" "$attempt" "$dur"
    echo "cb-ssh: $CB_MSG" >&2
    [ -n "$text" ] && printf '%s\n' "$text" | head -5 | sed 's/^/cb-ssh [raw] /' >&2
    exit "$CB_CODE"
  fi
  audit_log "$rc" "$attempt" "$dur"
  [ -n "$text" ] && printf '%s\n' "$text" >&2
  exit "$rc"
}

# Test hook: source functions without running (item: test each).
if [ "${CB_SSH_SOURCE_ONLY:-0}" = "1" ]; then
  return 0 2>/dev/null || exit 0
fi

: "${CB_HOST:?cb-ssh: CB_HOST is not set (e.g. export CB_HOST=\"auto@100.x.y.z\")}" 

# Item 12: proxy helper pre-check, fail fast (only when a proxy is configured).
if [ -n "$CB_PROXY_CMD" ]; then
  [ -x "$CB_PROXY_CMD" ] || { audit_log "$E_PROXY_CMD" 0 0; die "$E_PROXY_CMD" "proxy helper missing or not executable: $CB_PROXY_CMD"; }
fi

# Item 11: client key hygiene.
[ -f "$CB_KEY" ] || { audit_log "$E_KEY" 0 0; die "$E_KEY" "client key missing: $CB_KEY"; }
_kperm="$(stat -c %a "$CB_KEY" 2>/dev/null || echo ?)"
[ "$_kperm" = "600" ] || { audit_log "$E_KEY" 0 0; die "$E_KEY" "refusing: client key $CB_KEY has permissions ${_kperm}, must be 600 (run: chmod 600 $CB_KEY)"; }

# Item 4: pinned known_hosts must exist.
[ -f "$CB_PINNED" ] || { audit_log "$E_PINNED" 0 0; die "$E_PINNED" "pinned known_hosts missing: $CB_PINNED (re-capture from the live server on his word)"; }

# Item 6: persistent control dir, 700.
mkdir -p "$CB_CM_DIR" 2>/dev/null
chmod 700 "$CB_CM_DIR" 2>/dev/null
[ -d "$CB_CM_DIR" ] || { audit_log "$E_MUXDIR" 0 0; die "$E_MUXDIR" "control socket dir unusable: $CB_CM_DIR"; }

# Audit trail dir (audit 40 MED-10): created alongside the control dir so
# audit lines never silently vanish on a fresh machine.
mkdir -p "$(dirname "$CB_AUDIT_LOG")" 2>/dev/null

SSH_BASE=(ssh -i "$CB_KEY"
  -o BatchMode=yes -o ConnectTimeout=20
  -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$CB_PINNED"
  -o ControlMaster=auto -o ControlPath="$CB_CP" -o ControlPersist=600
  -o ServerAliveInterval=30 -o ServerAliveCountMax=3
  -o ConnectionAttempts=2)
# ProxyCommand only when a helper is configured (audit HIGH-3: empty means direct SSH).
if [ -n "$CB_PROXY_CMD" ]; then
  SSH_BASE+=(-o ProxyCommand="python3 $CB_PROXY_CMD %h %p")
fi
if [ -n "${CB_SSH_FLAGS:-}" ]; then
  # shellcheck disable=SC2206
  _extra=($CB_SSH_FLAGS)
  SSH_BASE+=("${_extra[@]}")
fi

# rsync -e compatibility (item 7): rsync invokes the remote shell as
# "$rsh [-l user] host command". Strip the leading "-l user host" (or bare
# host); the destination host is fixed below.
if [ "${1:-}" = "-l" ] && [ $# -ge 3 ]; then
  case "$3" in
    "$CB_HOST"|"${CB_HOST#*@}") shift 3 ;;
  esac
fi
case "${1:-}" in
  "$CB_HOST"|"${CB_HOST#*@}") shift ;;
esac

attempt=1
max_attempts=3
REMOTE_PREVIEW="$*"
while [ "$attempt" -le "$max_attempts" ]; do
  mux_recover
  errf="$(mktemp /tmp/cb-ssh-err.XXXXXX)" || die "$E_MUXDIR" "mktemp failed"
  t0=$(( ${EPOCHREALTIME/./} / 1000 ))
  # stdin passes through untouched (deploy-jobs.sh and the documented
  # `cat | cb-ssh.sh 'cat >'` pattern depend on it).
  "${SSH_BASE[@]}" "$CB_HOST" "$@" 2>"$errf"
  rc=$?
  t1=$(( ${EPOCHREALTIME/./} / 1000 ))
  dur=$(( t1 - t0 ))
  [ "$dur" -lt 0 ] && dur=0
  if [ "$rc" -eq 0 ]; then
    audit_log 0 "$attempt" "$dur"
    rm -f "$errf"
    exit 0
  fi
  text="$(cat "$errf" 2>/dev/null)"
  rm -f "$errf"
  if is_hard_fail "$text"; then
    finish_classified "$rc" "$text" "$attempt" "$dur"
  fi
  if [ "$attempt" -lt "$max_attempts" ] && is_transient "$text"; then
    base=$(( 1000 * (1 << (attempt - 1)) ))
    [ "$base" -gt 8000 ] && base=8000
    sleep_ms=$(( base + RANDOM % 1000 ))
    sleep "$(awk -v ms="$sleep_ms" 'BEGIN{printf "%.3f", ms/1000}')"
    attempt=$(( attempt + 1 ))
    continue
  fi
  finish_classified "$rc" "$text" "$attempt" "$dur"
done
# Unreachable: the loop always exits via success or finish_classified.
exit "$E_SSH"
