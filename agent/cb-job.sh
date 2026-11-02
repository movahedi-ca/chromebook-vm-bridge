#!/bin/bash
# shellcheck disable=SC2088 # `~/jobs/...` inside $SSH "..." strings is
# expanded by the REMOTE shell by design (remote user auto); the local
# shell must NOT expand it. Same rationale as the SC2029 disables below.
# cb-job.sh - compact job protocol for chromebook-vm.
# One SSH call per task. Results come back as compact text/JSON, never a
# running narration. Token discipline: batch the work, cap the output.
#
#   cb-job.sh ping                -> pong:<hostname>:<uptime> (pre-flight check)
#   cb-job.sh preflight           -> ping; wake via cb-wake.sh and retry if down
#   cb-job.sh doctor              -> one-call health: load, mem, disk, DNS, services
#   cb-job.sh top                 -> top 5 processes by CPU and memory, one call
#   cb-job.sh wake                -> network-wake attempt via cb-wake.sh
#                                    (2026-10-29: cannot wake from full
#                                    shutdown on this firmware; fails plainly)
#   cb-job.sh ls                  -> running|done per background job
#   cb-job.sh fetch URL [MAXCHARS]-> JSON {url,title,description,text,links}
#   cb-job.sh fetch-lp URL        -> Lightpanda pilot (A2): isolated unsigned
#                                    fetch via /usr/local/libexec/lightpanda/
#                                    lightpanda-fetch. stdout: markdown dump;
#                                    stderr: JSON result line. Explicit opt-in
#                                    only; allowlisted hosts; no auth/cookies.
#   cb-job.sh run 'CMD'           -> run CMD on the VM (600s cap), stdout back
#   cb-job.sh bg NAME 'CMD'       -> start background job; poll with status/get
#   cb-job.sh status NAME         -> running | done | unknown  (cheap poll)
#   cb-job.sh get NAME [LINES]    -> print the job's output (+ exit code);
#                                    capped at 500 lines unless LINES given,
#                                    with a "[N lines, M KB]" size header and
#                                    a "[truncated: K more lines omitted]"
#                                    marker whenever the cap cuts content
#   cb-job.sh kill NAME           -> stop a running background job
#   cb-job.sh rm NAME             -> clean up a finished job's files
#   cb-job.sh activity "msg"      -> show a working message on the VNC display
#   cb-job.sh activity idle       -> display back to idle
#
# Display: every bg/fetch/run updates the AUTO activity panel (top-right of
# the VNC display :1) so he can see what the agent is doing. For visual
# (cbvnc.sh) sessions, set it manually: activity "signing in to M365".
#
# Long-job pattern:
#   cb-job.sh bg crawl 'for u in URL1 URL2 URL3; do python3 ~/jobs/fetch.py "$u" 6000; done'
#   cb-job.sh status crawl          # cheap poll; repeat until done
#   cb-job.sh get crawl             # pull the result once
#   cb-job.sh rm crawl              # clean up
#
# Job names: letters, digits, _ and -, must start with a letter or digit.
set -e
CB_DIR="${CB_DIR:-$HOME/chromebook-vm}"
SSH="$CB_DIR/cb-ssh.sh"

_name_ok() {
  case "$1" in
    [A-Za-z0-9]* ) ;;
    *) echo "bad job name: '$1' (letters, digits, _ -, must start alnum)" >&2; return 2;;
  esac
  case "$1" in
    *[!A-Za-z0-9_-]* ) echo "bad job name: '$1' (letters, digits, _ -, must start alnum)" >&2; return 2;;
  esac
}

cmd="${1:-}"; shift || true
# Phase 2A item 8: label every ssh call from this dispatcher so conn-audit.log
# can attribute latency per subcommand. cb-ssh.sh reads CB_AUDIT_CMD.
export CB_AUDIT_CMD="cbjob:${cmd:-none}"
case "$cmd" in
  ping)
    # shellcheck disable=SC2029
    $SSH 'echo "pong:$(hostname):$(uptime -p)"'
    ;;
  wake)
    # Network-wake the VM (shutdown state) via cb-wake.sh. Implemented by
    # the wake subagent; fails plainly if the script is missing.
    WAKE="$CB_DIR/cb-wake.sh"
    if [ -x "$WAKE" ]; then
      "$WAKE"
    else
      echo "wake unavailable: $WAKE missing or not executable" >&2
      exit 3
    fi
    ;;
  preflight)
    # One command that guarantees the VM is up: ping, and on failure try the
    # network wake once with a SHORT poll (covers a reboot in progress),
    # then ping again. This is what the school-scan cron calls instead of
    # bare ping. 2026-10-29: wake from full shutdown is proven impossible on
    # this box (firmware ignores the RTC alarm), so if this fails the only
    # fix is a physical press of the Chromebook's power button.
    if "$CB_DIR/cb-job.sh" ping 2>/dev/null; then
      exit 0
    fi
    echo "ping failed, attempting short network wake..." >&2
    if "$CB_DIR/cb-job.sh" wake 2 2>/dev/null; then
      "$CB_DIR/cb-job.sh" ping
    else
      echo "preflight failed: VM is down and cannot be woken over the network." >&2
      echo "Press the Chromebook's physical power button, then retry." >&2
      exit 4
    fi
    ;;
  doctor)
    # One-call health snapshot: hostname, uptime, load, memory, disk, DNS,
    # key user services, Chrome version. Replaces 3-4 separate calls.
    # shellcheck disable=SC2029
    $SSH 'echo "host:$(hostname) up:$(uptime -p)"; echo "load:$(cut -d" " -f1-3 /proc/loadavg)"; free -m | awk "NR==2{printf \"mem:used=%dM free=%dM avail=%dM\\n\",\$3,\$4,\$7}"; df -h / /home | awk "NR>1{printf \"disk %s: %s used of %s (%s)\\n\",\$6,\$3,\$2,\$5}"; getent hosts example.com >/dev/null 2>&1 && echo "dns:ok" || echo "dns:FAIL"; for s in vnc.service mirror-feed.service mirror-display.service novnc-kiosk.service; do printf "%s:%s " "$s" "$(systemctl --user is-active $s 2>/dev/null)"; done; echo; google-chrome --version 2>/dev/null || echo "chrome:missing"'
    ;;
  top)
    # Top 5 processes by CPU and by memory, one call, compact.
    # shellcheck disable=SC2029
    $SSH 'echo "== cpu =="; ps -eo pid,pcpu,pmem,comm --sort=-pcpu | head -6; echo "== mem =="; ps -eo pid,pcpu,pmem,comm --sort=-pmem | head -6'
    ;;
  zswap)
    # zswap health: runs ~/jobs/zswap-check.sh on the VM. Exit 0 = healthy.
    # shellcheck disable=SC2029
    $SSH '~/jobs/zswap-check.sh'
    ;;
  ls)
    # shellcheck disable=SC2029
    $SSH 'for f in ~/jobs/out/*.log; do [ -e "$f" ] || continue; n=$(basename "$f" .log); if flock -n ~/jobs/out/"$n".lock true 2>/dev/null; then st=done; else st=running; fi; echo "$st $n"; done; true'
    ;;
  fetch)
    url="$1"; max="$(printf '%s' "${2:-8000}" | tr -cd '0-9')"; [ -n "$max" ] || max=8000
    b64=$(printf '%s' "$url" | base64 -w0)
    url_short=$(printf '%s' "$url" | cut -c1-60 | tr -d "'")
    # Consumer note: fetch.py exit codes are 0 ok, 2 usage, 3 chrome-timeout (retryable), 4 empty-DOM (retryable), 5 chrome-crash, 6 HTTP 4xx/5xx; JSON errors carry {"error","retryable"}.
    # shellcheck disable=SC2029
    $SSH "~/jobs/activity-set.sh working 'fetch' '$url_short'"
    # set -e would abort before the idle reset on fetch.py failure
    # (audit MED-3); capture the rc, always reset the panel, then exit it.
    # mktemp (audit LOW-4): no shared /tmp/cb-url.txt race under concurrency.
    set +e
    $SSH "u=\$(mktemp /tmp/cb-url.XXXXXX); echo $b64 | base64 -d > \"\$u\" && python3 ~/jobs/fetch.py \"\$(cat \"\$u\")\" $max; frc=\$?; rm -f \"\$u\"; exit \$frc"
    fetch_rc=$?
    set -e
    $SSH "~/jobs/activity-set.sh idle 'fetched'" || true
    exit "$fetch_rc"
    ;;
  fetch-lp)
    # Lightpanda pilot (playbook A2): isolated unsigned fetch. URL travels
    # base64-encoded like fetch; the wrapper validates, sandboxes, and fetches.
    # stdout: markdown dump. stderr: JSON result line {ok,category,...}.
    url="$1"; [ -n "$url" ] || { echo "usage: cb-job.sh fetch-lp URL" >&2; exit 2; }
    b64=$(printf '%s' "$url" | base64 -w0)
    url_short=$(printf '%s' "$url" | cut -c1-60 | tr -d "'")
    # shellcheck disable=SC2029
    $SSH "~/jobs/activity-set.sh working 'fetch-lp' '$url_short'"
    # cb-ssh swallows remote stderr on success, so the wrapper's JSON result
    # line is republished here on stdout as "LP_RESULT {...}" ahead of the
    # markdown. Parse the first line for metrics; the rest is the dump.
    # shellcheck disable=SC2029
    $SSH "echo $b64 | base64 -d > /tmp/cb-lp-url.txt && /usr/local/libexec/lightpanda/lightpanda-fetch \"\$(cat /tmp/cb-lp-url.txt)\" > /tmp/cb-lp-out.md 2> /tmp/cb-lp-out.err; rc=\$?; printf 'LP_RESULT '; cat /tmp/cb-lp-out.err | grep -m1 '^{\"ok\"' || echo '{\"ok\":false,\"category\":\"no-result\"}'; cat /tmp/cb-lp-out.md; rm -f /tmp/cb-lp-url.txt /tmp/cb-lp-out.md /tmp/cb-lp-out.err; exit \$rc"
    $SSH "~/jobs/activity-set.sh idle 'fetched-lp'"
    ;;
  run)
    b64=$(printf '%s' "$*" | base64 -w0)
    cmd_short=$(printf '%s' "$*" | cut -c1-60 | tr -d "'")
    # Speed optimization (2026-10-29): was 3 separate SSH calls
    # (activity working, command, activity idle) = ~1.1s. Now one call
    # that does all three sequentially: ~0.36s. The panel still updates,
    # just without the round-trip tax.
    # shellcheck disable=SC2029
    # set -e would abort on a non-zero remote exit before rc=$? runs
    # (audit MED-3); the if/else form is exempt from set -e.
    if $SSH "_tmp=\$(mktemp /tmp/cb-run.XXXXXX) || exit 96; chmod 600 \"\$_tmp\"; ~/jobs/activity-set.sh working 'run' '$cmd_short'; echo $b64 | base64 -d | timeout 600 bash > \"\$_tmp\" 2>&1; rc=\$?; cat \"\$_tmp\"; rm -f \"\$_tmp\"; ~/jobs/activity-set.sh idle 'run done' >/dev/null 2>&1 || true; exit \$rc"; then
      rc=0
    else
      rc=$?
    fi
    if [ "$rc" -eq 124 ]; then
      # Phase 2A item 9: timeout(1) exit 124 means the 600s cap killed it.
      # Say so explicitly instead of silently truncating. (A remote command
      # that itself exits 124 would also trip this; accepted, noted.)
      echo "run hit the 600s cap: timeout killed the command at 600s, output above is truncated. Split the work or use bg for long jobs." >&2
    fi
    exit "$rc"
    ;;
  bg)
    name="$1"; shift; _name_ok "$name" || exit 2
    b64=$(printf '%s' "$*" | base64 -w0)
    # flock on fd 9 is the running-state truth (survives PID reuse; a reboot
    # releases the lock, so a dead job reads done/unknown, never stuck).
    # set -m gives the worker its own process group so kill takes the whole
    # tree; the worker PID (= its PGID) is recorded for kill.
    # The subshell's own fds are detached (</dev/null >/dev/null 2>&1) so the
    # ssh session returns immediately instead of waiting for the job.
    # NOTE: the ';' before '(' is load-bearing: with '&&' there, the '&'
    # grabs the whole &&-chain, the job runs in the foreground of it, and
    # the ssh session blocks for the job's full duration.
    # On completion the subshell appends one line to ~/jobs/audit.log
    # (timestamp, name, exit code, duration) and refreshes the activity panel
    # via bg-done.sh (idle if no other job is running).
    # shellcheck disable=SC2029
    $SSH "mkdir -p ~/jobs/out && _av=\$(df -k ~/jobs/out | awk 'NR==2{print \$4}'); [ \"\$_av\" -ge 1048576 ] || { echo \"refused: disk <1G free (\${_av}K)\" >&2; exit 98; }; _rn=0; for _f in ~/jobs/out/*.lock; do [ -e \"\$_f\" ] || continue; flock -n \"\$_f\" true 2>/dev/null || _rn=\$((_rn+1)); done; [ \"\$_rn\" -lt 4 ] || { echo \"refused: \${_rn} jobs running (max 4)\" >&2; exit 97; }; echo $b64 | base64 -d > ~/jobs/out/$name.cmd && chmod +x ~/jobs/out/$name.cmd && rm -f ~/jobs/out/$name.log ~/jobs/out/$name.exit ~/jobs/out/$name.pgid; ( set -m; flock -n 9 || exit 99; _s=\$(date +%s); bash ~/jobs/out/$name.cmd > ~/jobs/out/$name.log 2>&1 & worker=\$!; echo \$worker > ~/jobs/out/$name.pgid; wait \$worker; ec=\$?; _sz=\$(stat -c%s ~/jobs/out/$name.log 2>/dev/null || echo 0); if [ \"\$_sz\" -gt 1048576 ]; then { echo \"[log exceeded 1MB: showing last 1MB]\"; tail -c 1048576 ~/jobs/out/$name.log; } > ~/jobs/out/$name.log.tmp && mv ~/jobs/out/$name.log.tmp ~/jobs/out/$name.log; fi; echo \$ec > ~/jobs/out/$name.exit; echo \"\$(date -Iseconds) $name ec=\$ec dur=\$(( \$(date +%s) - _s ))s\" >> ~/jobs/audit.log; ~/jobs/bg-done.sh $name \$ec ) 9>~/jobs/out/$name.lock </dev/null >/dev/null 2>&1 & echo started:$name"
    # shellcheck disable=SC2029
    $SSH "~/jobs/activity-set.sh working 'bg job: $name' ''"
    ;;
  status)
    name="$1"; _name_ok "$name" || exit 2
    # shellcheck disable=SC2029
    $SSH "if [ ! -e ~/jobs/out/$name.lock ] && [ ! -e ~/jobs/out/$name.log ]; then echo unknown; elif flock -n ~/jobs/out/$name.lock true 2>/dev/null; then echo done; else echo running; fi"
    ;;
  get)
    name="$1"; _name_ok "$name" || exit 2
    n="$(printf '%s' "${2:-}" | tr -cd '0-9')"
    # Default cap: 500 lines (was 2000; a chatty log dumped ~44k tokens).
    # The "[N lines, M KB]" header lets the orchestrator decide up front
    # whether to pull more with an explicit LINES arg; the trailing marker
    # makes a cut visible instead of silent. One SSH call for all of it.
    [ -n "$n" ] || n=500
    # shellcheck disable=SC2029
    $SSH "f=~/jobs/out/$name.log; if [ -f \"\$f\" ]; then tl=\$(wc -l < \"\$f\" | tr -d ' '); kb=\$(( \$(stat -c%s \"\$f\") / 1024 )); echo \"[\$tl lines, \$kb KB]\"; if [ \"\$tl\" -gt $n ]; then tail -n $n \"\$f\"; echo \"[truncated: \$((tl - $n)) more lines omitted]\"; else cat \"\$f\"; fi; else echo \"[no log for $name]\"; fi; echo \"[exit:\$(cat ~/jobs/out/$name.exit 2>/dev/null || echo ?)]\""
    ;;
  kill)
    name="$1"; _name_ok "$name" || exit 2
    # Kill the worker's whole process group, but only after verifying the
    # recorded PID is alive, leads its own group, and is our job's worker.
    # shellcheck disable=SC2029
    $SSH "g=\$(cat ~/jobs/out/$name.pgid 2>/dev/null); if [ -n \"\$g\" ] && [ \"\$g\" -gt 0 ] 2>/dev/null && [ \"\$(ps -o pgid= -p \$g 2>/dev/null | tr -d ' ')\" = \"\$g\" ] && tr '\0' ' ' < /proc/\$g/cmdline 2>/dev/null | grep -q \"jobs/out/$name.cmd\"; then kill -- -\$g 2>/dev/null && echo killed:$name || echo kill-failed:$name; else echo not-running:$name; fi"
    ;;
  rm)
    name="$1"; _name_ok "$name" || exit 2
    # NOTE: no $name.pid — only .pgid is ever written (audit 41 LOW-14).
    $SSH "rm -f ~/jobs/out/$name.pgid ~/jobs/out/$name.log ~/jobs/out/$name.exit ~/jobs/out/$name.cmd ~/jobs/out/$name.lock && echo removed:$name"
    ;;
  activity)
    # Show a working message on the VNC display panel (for visual sessions),
    # or reset it to idle. Usage: cb-job.sh activity "signing in to M365"
    if [ "${1:-}" = "idle" ] || [ -z "${1:-}" ]; then
      # shellcheck disable=SC2029
      $SSH "~/jobs/activity-set.sh idle ''"
    else
      msg=$(printf '%s' "$*" | cut -c1-80 | tr -d "'")
      # shellcheck disable=SC2029
      $SSH "~/jobs/activity-set.sh working 'agent session' '$msg'"
    fi
    ;;
  *)
    echo "usage: cb-job.sh ping|preflight|doctor|top|wake | ls | fetch URL [MAXCHARS] | run 'CMD' | bg NAME 'CMD' | status NAME | get NAME [LINES] | kill NAME | rm NAME | activity \"msg\" | activity idle" >&2
    exit 2
    ;;
esac
