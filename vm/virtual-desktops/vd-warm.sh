#!/bin/bash
# Staggered warm-up: vd-chrome.sh vd1..vdN, one at a time, with gates.
# Usage: vd-warm.sh N [url]   (N = 1..10)
# Gates before each next launch: MemAvailable >= 1200 MB AND 1-min load <= 4.
# Gives up waiting after ~60s and reports how many are up.
set -u
N="${1:?usage: vd-warm.sh N [url]  (N=1..10)}"
case "$N" in ''|*[!0-9]*) echo "usage: vd-warm.sh N [url]  (N=1..10)"; exit 1;; esac
[ "$N" -lt 1 ] && N=1
[ "$N" -gt 10 ] && N=10
URL="${2:-about:blank}"
VD_CHROME="$HOME/jobs/virtual-desktops/vd-chrome.sh"

up=0
for k in $(seq 1 "$N"); do
  port="$((9332+k))"
  if [ "$k" -gt 1 ]; then
    waited=0
    while [ "$waited" -lt 60 ]; do
      avail=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
      load1=$(cut -d' ' -f1 /proc/loadavg | cut -d. -f1)
      if [ "$avail" -ge 1200 ] && [ "$load1" -le 4 ]; then break; fi
      sleep 5; waited=$((waited+5))
    done
    if [ "$waited" -ge 60 ]; then
      echo "vd-warm: gates not met after 60s (${up} up) - stopping"
      break
    fi
  fi
  "$VD_CHROME" "vd${k}" "$URL" || { echo "vd-warm: vd${k} failed to launch"; break; }
  ok=0
  for i in $(seq 1 30); do
    curl -sf -m 2 "http://127.0.0.1:${port}/json/version" >/dev/null 2>&1 && { ok=1; break; }
    sleep 1
  done
  if [ "$ok" -ne 1 ]; then echo "vd-warm: vd${k} CDP not up after 30s - stopping"; break; fi
  up=$k
done
echo "vd-warm: ${up}/${N} desktops up"
