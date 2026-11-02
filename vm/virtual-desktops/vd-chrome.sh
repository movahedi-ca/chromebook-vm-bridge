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
LOG="$HOME/jobs/virtual-desktops/logs/chrome-${1}.log"

# Memory warning (warn, never refuse): sustained fills stay at or under 6
# desktops; 10 is a burst. Stagger with vd-warm.sh.
# Count distinct vd profiles, not processes: one desktop spawns ~10 processes.
AVAIL_MB=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
VD_RUNNING=$(pgrep -a -f -- "--user-data-dir=" 2>/dev/null | grep -oE "chrome-vd(10|[1-9]) " | sort -u | wc -l)
if [ "$AVAIL_MB" -lt 1200 ] || [ "$VD_RUNNING" -ge 6 ]; then
  echo "warning: ${AVAIL_MB} MB available, ${VD_RUNNING} vd chromes running -"
  echo "sustained fills stay at or under 6; 10 is a burst; stagger via vd-warm.sh"
fi

# stop any existing chrome bound to this exact profile dir (safe: our own
# cmdline never contains the full --user-data-dir path)
for p in $(pgrep -f -- "--user-data-dir=${PROF}"); do kill "$p" 2>/dev/null; done
sleep 1
# Do NOT delete Default/Preferences here: the pinned --window-size /
# --window-position flags replaced the wipe that fixed the 10x10 window.
# If one profile is still 10x10, delete its Preferences once by hand.
mkdir -p "$PROF"

# One generation of log rotation: only when the log actually passes 10 MB.
if [ -f "$LOG" ] && [ "$(stat -c%s "$LOG")" -gt 10485760 ]; then
  mv "$LOG" "${LOG}.1"
fi

DISPLAY="$DISP" nohup google-chrome-stable \
  --user-data-dir="$PROF" \
  --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port="$PORT" \
  --password-store=basic \
  --window-position=0,0 \
  --window-size=1920,1080 \
  --disable-gpu \
  --disable-dev-shm-usage \
  --disable-background-timer-throttling \
  --disable-backgrounding-occluded-windows \
  --disable-renderer-backgrounding \
  --disable-ipc-flooding-protection \
  --disable-background-networking \
  --disable-component-update \
  --disable-sync \
  --disable-translate \
  --mute-audio \
  --no-first-run --no-default-browser-check \
  "$URL" >"$LOG" 2>&1 &
echo "chrome on $1 (display $DISP, debug port $PORT, profile $PROF)"
