# Virtual desktops for parallel browser sessions (chromebook-vm)

Up to 10 extra headful desktops (vd1..vd10) for running independent Chrome
browser sessions in parallel with the main automation stack. Scalable:
`vd-start.sh [N]` spins up exactly vd1..vdN.

## Mapping (deterministic, for vdK with K = 1..10)

| desktop | X display | VNC (localhost) | CDP debug port | Chrome profile        |
|---------|-----------|-----------------|----------------|-----------------------|
| vd1     | :2        | 5903            | 9333           | ~/.config/chrome-vd1  |
| vd2     | :3        | 5904            | 9334           | ~/.config/chrome-vd2  |
| vd3     | :4        | 5905            | 9335           | ~/.config/chrome-vd3  |
| vd4     | :5        | 5906            | 9336           | ~/.config/chrome-vd4  |
| vd5     | :6        | 5907            | 9337           | ~/.config/chrome-vd5  |
| vd6     | :7        | 5908            | 9338           | ~/.config/chrome-vd6  |
| vd7     | :8        | 5909            | 9339           | ~/.config/chrome-vd7  |
| vd8     | :9        | 5910            | 9340           | ~/.config/chrome-vd8  |
| vd9     | :10       | 5911            | 9341           | ~/.config/chrome-vd9  |
| vd10    | :11       | 5912            | 9342           | ~/.config/chrome-vd10 |

Formula: vdK -> display :(K+1), VNC (5902+K), CDP (9332+K), profile chrome-vdK.
All desktops: Xvfb 1920x1080x24 + openbox + x11vnc (localhost-bound, -nopw).

Untouched by design: :0 (physical display mirror), :1 (automation Chrome +
Rust bridge on :8899, VNC 5902). vd-stop.sh only ever touches :2 and up.

## Scripts

- `~/jobs/vd-start.sh [N]` - start vd1..vdN (default 2, max 10). Idempotent:
  already-running displays are skipped, never duplicated or killed.
- `~/jobs/vd-stop.sh [N|all]` - stop vd1..vdN, or every running vd desktop
  with `all` (default; scans :2 and up only). Safe pkill patterns; the caller
  cmdline never matches them because literals live inside the script file.
- `~/jobs/virtual-desktops/vd-chrome.sh vdK [url]` - launch/relaunch headful
  Chrome on vdK (K = 1..10). Refuses unless that desktop is up. Kills only
  that desktop's chrome (by exact --user-data-dir), drops stale window
  geometry, --start-maximized, --disable-dev-shm-usage for the small VM.
- Logs: `~/jobs/virtual-desktops/logs/`
- Previous hardcoded 2-desktop scripts: `~/jobs/virtual-desktops/legacy-2026-11-02/`

Run from the agent VM via:
  ./cb-ssh.sh "~/jobs/vd-start.sh 5"
  ./cb-ssh.sh "~/jobs/virtual-desktops/vd-chrome.sh vd3 https://example.com"
  ./cb-ssh.sh "~/jobs/vd-stop.sh all"

## Viewing a desktop

VNC is localhost-bound on the VM (same as the existing :5902 setup).
From the agent VM, tunnel then connect, e.g. for vd3:
  ssh -L 5905:127.0.0.1:5905 auto@<tailnet-ip>
Screenshot without VNC:
  ./cb-ssh.sh "DISPLAY=:4 scrot -z /tmp/vd3.png"

## Targeting a desktop from a future bridge/client

1. DISPLAY env: prefix any X command with `DISPLAY=:<K+1>`.
2. CDP: each Chrome exposes remote debugging on its own port
   (vdK: 127.0.0.1:(9332+K)). A bridge instance can attach via
   `http://127.0.0.1:<port>/json/list` exactly like the main bridge does
   on :9222 for :1. One bridge per desktop (separate ports, separate
   profiles, separate user-data-dirs) keeps sessions fully isolated.
3. Profiles: ~/.config/chrome-vdK are independent; sign-ins/cookies on one
   never leak to another or to :1.

## Resource guidance (measured 2026-11-02 on this 8GB box)

- One headful Chrome costs ~240MB RAM once settled.
- 5 concurrent Chromes: 5.8GB/7.8GB used, ~2.0GB available, all 5 CDP ports
  live, screenshots render fine. Comfortable.
- 10 concurrent Chromes: 6.8GB/7.8GB used, ~1.0GB available, load spiked to
  ~10.5 at startup (settles), zero OOM kills, all 10 CDP ports live,
  example.com fully rendered on vd10. Works, but tight.
- Practical ceiling: 10 works for short bursts (e.g. a 10-job wave launch);
  for sustained parallel work keep it at 6 or fewer to leave ~2GB headroom
  for the :1 automation stack and the OS. If a run needs more than 6
  steady sessions, stagger Chrome launches instead of starting all at once
  (startup is when load spikes).
- Steady state when idle: vd1+vd2 running with Chrome (~3.3GB available).

## Gotchas learned

- Never `pkill -f` with a literal profile string from an ssh one-liner: the
  invoking shell's own cmdline matches and kills itself (hit twice).
  vd-chrome.sh and vd-stop.sh keep literals inside script files so caller
  cmdlines never match. When probing from a one-liner, use a self-excluding
  pattern like `pgrep -f "chrome-vd[0-9]"` (regex `[0-9]` does not match the
  literal `[0-9]` in your own cmdline).
- Bash `set -u` pitfall: `local a="$1" b="$a"` fails with "unbound variable"
  because the RHS expands before the local takes effect. Always split into
  two `local` lines (fixed in vd-stop.sh 2026-11-02).
- Chrome sometimes maps at 10x10 first; --start-maximized plus a few seconds
  settles it to fullscreen under openbox. If stuck, delete
  <profile>/Default/Preferences and relaunch via vd-chrome.sh.
- Xvfb resolution is 1920x1080x24 (set in vd-start.sh).

Verified 2026-11-02: parameterized scripts tested end to end — full stop,
vd-start.sh 5 + Chrome on all 5 (screenshots render, 5/5 CDP live),
vd-stop.sh all (:0/:1 untouched, services active), 10-desktop stress
(10/10 CDP live, no OOM, vd10 screenshot fully rendered), steady state
restored to vd1+vd2 with Chrome.
