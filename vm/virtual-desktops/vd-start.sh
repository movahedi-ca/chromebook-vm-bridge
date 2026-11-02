#!/bin/bash
# Start virtual desktops vd1..vdN on chromebook-vm. Usage: vd-start.sh [N] (default 2, max 10).
# Mapping for vdK: X display :(K+1), VNC localhost:(5902+K), CDP 127.0.0.1:(9332+K),
#                  Chrome profile ~/.config/chrome-vdK.
# Leaves :0 (physical mirror) and :1 (automation Chrome + bridge) untouched.
# Idempotent: already-running displays are skipped, never duplicated or killed.
set -u
N="${1:-2}"
case "$N" in ''|*[!0-9]*) echo "usage: vd-start.sh [N]  (1..10)"; exit 1;; esac
[ "$N" -lt 1 ] && N=1
[ "$N" -gt 10 ] && { echo "vd-start.sh: max 10 desktops"; exit 1; }
LOGDIR="$HOME/jobs/virtual-desktops/logs"
mkdir -p "$LOGDIR"

start_one() {
  local k="$1" n="$((k+1))" disp=":$((k+1))" vncport="$((5902+k))"
  local name="vd${k}"
  if [ -S "/tmp/.X11-unix/X${n}" ]; then
    echo "$name: display $disp already running, skipping"
    return 0
  fi
  echo "$name: starting Xvfb on $disp ..."
  nohup Xvfb "$disp" -screen 0 1920x1080x24 >"$LOGDIR/xvfb-${n}.log" 2>&1 &
  for i in $(seq 1 20); do
    [ -S "/tmp/.X11-unix/X${n}" ] && break
    sleep 0.5
  done
  if [ ! -S "/tmp/.X11-unix/X${n}" ]; then
    echo "$name: Xvfb failed to come up on $disp, aborting $name"
    return 1
  fi
  echo "$name: starting openbox ..."
  DISPLAY="$disp" nohup openbox >"$LOGDIR/openbox-${n}.log" 2>&1 &
  echo "$name: starting x11vnc on port $vncport ..."
  x11vnc -display "$disp" -rfbport "$vncport" -localhost -nopw -forever -shared \
    -noxdamage -wait 50 -cursor -cursor_drag -bg -o "$LOGDIR/x11vnc-${n}.log"
  echo "$name: up (display $disp, vnc localhost:$vncport)"
}

for k in $(seq 1 "$N"); do start_one "$k"; done
echo "done"
