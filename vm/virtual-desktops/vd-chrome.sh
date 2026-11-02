#!/bin/bash
# Launch (or relaunch) headful Chrome on a virtual desktop.
# Usage: vd-chrome.sh vdK [url]   (K = 1..10)
# Mapping: vdK -> display :(K+1), CDP 127.0.0.1:(9332+K), profile ~/.config/chrome-vdK.
# The desktop must already be up (vd-start.sh). Validates before launching.
# The script file keeps profile literals out of the invoking command line,
# so pkill/pgrep patterns below can never match the caller itself.
set -u
case "${1:?usage: vd-chrome.sh vdK [url]  (K=1..10)}" in
  vd[1-9]|vd10)
    K="${1#vd}"
    DISP=":$((K+1))"; PROF="$HOME/.config/chrome-vd${K}"; PORT="$((9332+K))" ;;
  *) echo "unknown desktop: $1 (want vd1..vd10)"; exit 1 ;;
esac
if [ ! -S "/tmp/.X11-unix/X$((K+1))" ]; then
  echo "$1: display $DISP is not up - run vd-start.sh first"; exit 1
fi
URL="${2:-about:blank}"
# stop any existing chrome bound to this exact profile dir (safe: our own
# cmdline never contains the full --user-data-dir path)
for p in $(pgrep -f -- "--user-data-dir=${PROF}"); do kill "$p" 2>/dev/null; done
sleep 1
rm -f "${PROF}/Default/Preferences" 2>/dev/null  # drop stale window geometry
mkdir -p "$PROF"
DISPLAY="$DISP" nohup google-chrome \
  --user-data-dir="$PROF" \
  --no-first-run --no-default-browser-check \
  --disable-dev-shm-usage \
  --remote-debugging-port="$PORT" \
  --start-maximized \
  "$URL" >"$HOME/jobs/virtual-desktops/logs/chrome-${1}.log" 2>&1 &
echo "chrome on $1 (display $DISP, debug port $PORT, profile $PROF)"
