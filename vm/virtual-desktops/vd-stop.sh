#!/bin/bash
# Stop virtual desktops. Usage: vd-stop.sh [N|all]
#   N   - stop vd1..vdN (displays :2..:(N+1))
#   all - stop every running vd desktop (default; displays :2 and up only)
# Never touches :0, :1, or the bridge.
# Safe pkill patterns: our own cmdline is "vd-stop.sh ..." and never contains them.
set -u
ARG="${1:-all}"

kill_disp() {
  local dispnum="$1"
  local disp=":$dispnum"
  # kill processes whose DISPLAY env is exactly this display (openbox, chrome)
  for pid in $(ls /proc | grep -E "^[0-9]+$"); do
    if tr "\0" "\n" < "/proc/$pid/environ" 2>/dev/null | grep -qx "DISPLAY=$disp"; then
      kill "$pid" 2>/dev/null
    fi
  done
  # x11vnc and Xvfb were started with -display flags, no DISPLAY env
  pkill -f "x11vnc -display $disp" 2>/dev/null
  pkill -f "Xvfb $disp" 2>/dev/null
  sleep 0.5
  rm -f "/tmp/.X11-unix/X${dispnum}" "/tmp/.X${dispnum}-lock" 2>/dev/null
}

targets=""
if [ "$ARG" = "all" ]; then
  for s in /tmp/.X11-unix/X*; do
    n="${s##*/X}"
    case "$n" in ''|*[!0-9]*) continue;; esac
    [ "$n" -ge 2 ] && targets="$targets $n"
  done
else
  case "$ARG" in ''|*[!0-9]*) echo "usage: vd-stop.sh [N|all]"; exit 1;; esac
  [ "$ARG" -gt 10 ] && { echo "vd-stop.sh: max 10"; exit 1; }
  for k in $(seq 1 "$ARG"); do targets="$targets $((k+1))"; done
fi

[ -z "$targets" ] && { echo "no vd desktops running"; exit 0; }
for n in $targets; do
  kill_disp "$n"
  echo "vd$((n-1)) stopped (display :$n)"
done
echo "done"
