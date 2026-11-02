#!/bin/bash
# Read-only readiness check for vd1..vd10. Exit 0 when every desktop with an
# X socket also has a live CDP endpoint; exit 1 otherwise.
set -u
fail=0
for k in $(seq 1 10); do
  n="$((k+1))"; port="$((9332+k))"; vncport="$((5902+k))"
  [ -S "/tmp/.X11-unix/X${n}" ] || continue
  cdp="dead"; bind="?"
  if curl -sf -m 2 "http://127.0.0.1:${port}/json/version" >/dev/null 2>&1; then
    cdp="live"
    bind=$(ss -ltn "sport = :${port}" 2>/dev/null | awk 'NR>1 {print $4}' | head -1)
  else
    fail=1
  fi
  vnc="down"
  ss -ltn "sport = :${vncport}" 2>/dev/null | grep -q . && vnc="up"
  procs=$(pgrep -c -f -- "--remote-debugging-port=${port} " 2>/dev/null || echo 0)
  printf 'vd%-2s disp=:%-2s vnc=%-4s cdp=%-4s bind=%-15s procs=%s\n' "$k" "$n" "$vnc" "$cdp" "$bind" "$procs"
done
avail=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
load=$(cut -d' ' -f1-3 /proc/loadavg)
vdcount=$(pgrep -a -f -- "--user-data-dir=" 2>/dev/null | grep -oE "chrome-vd(10|[1-9]) " | sort -u | wc -l)
echo "box: ${avail} MB available, load ${load}, ${vdcount} vd chromes"
exit "$fail"
