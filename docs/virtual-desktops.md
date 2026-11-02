# Scalable Virtual Desktops on chromebook-vm
Complete technical writeup for review. Written 2026-11-02.

## 1. Purpose

Run up to 10 independent, headful Chrome browser sessions in parallel on
Mohammad's chromebook-vm, each on its own virtual desktop, so parallel
browser jobs (e.g. 10 job applications at once, one per desktop) never fight
over the same screen, profile, or cookies. Built at his order on 2026-11-02:
first "create virtual desktops and then you can use them as virtual desktop
for browser sessions", then "make it scalable so if we want 10 jobs at once
we can open 10 virtual displays", then "I want to be able to scale up and
down".

## 2. Machine context

- Host: chromebook-vm, Asus Chromebook Flip C433T, Debian 13, Intel m3-8100Y,
  8GB RAM (7.8 usable), 116GB disk. Tailnet: <tailnet-ip>, ssh user `auto`
  (passwordless sudo). Reached from the agent VM via
  `~/workspace/chromebook-vm/cb-ssh.sh`.
- Display layout BEFORE this work (all preserved, never touched):
  - :0 = physical display mirror (Xorg + fullscreen vncviewer showing :1).
  - :1 = main automation desktop: headed Chrome driven by the Rust vm-bridge
    (port 127.0.0.1:8899, CDP 127.0.0.1:9222), VNC on localhost:5902,
    profile ~/.config/chrome-automation (copy of his signed-in profile).
- Everything new lives on displays :2 and up. Nothing here touches :0, :1,
  the bridge, or any existing service.

## 3. Architecture per virtual desktop (vdK, K = 1..10)

Each desktop is a stack of three processes plus Chrome:

1. Xvfb on display :(K+1), 1920x1080x24 (headless X server, no GPU needed).
2. openbox window manager (lightweight, installed via apt for this).
3. x11vnc serving that display on localhost:(5902+K), -nopw, -localhost,
   -forever -shared, -noxdamage, with cursor flags (-cursor -cursor_drag)
   so the cursor renders through VNC.
4. google-chrome headful on that display, isolated profile
   ~/.config/chrome-vdK, --start-maximized, --disable-dev-shm-usage
   (the VM is small; /dev/shm pressure crashes Chrome tabs),
   --remote-debugging-port=(9332+K), --no-first-run.

Deterministic mapping (no config files, pure arithmetic):

| desktop | X display | VNC (localhost) | CDP (127.0.0.1) | Chrome profile        |
|---------|-----------|-----------------|-----------------|-----------------------|
| vd1     | :2        | 5903            | 9333            | ~/.config/chrome-vd1  |
| vd2     | :3        | 5904            | 9334            | ~/.config/chrome-vd2  |
| vd3     | :4        | 5905            | 9335            | ~/.config/chrome-vd3  |
| vd4     | :5        | 5906            | 9336            | ~/.config/chrome-vd4  |
| vd5     | :6        | 5907            | 9337            | ~/.config/chrome-vd5  |
| vd6     | :7        | 5908            | 9338            | ~/.config/chrome-vd6  |
| vd7     | :8        | 5909            | 9339            | ~/.config/chrome-vd7  |
| vd8     | :9        | 5910            | 9340            | ~/.config/chrome-vd8  |
| vd9     | :10       | 5911            | 9341            | ~/.config/chrome-vd9  |
| vd10    | :11       | 5912            | 9342            | ~/.config/chrome-vd10 |

Formula: vdK -> display :(K+1), VNC (5902+K), CDP (9332+K), profile chrome-vdK.
Profiles are fully independent: sign-ins and cookies on one desktop never
leak to another or to :1.

## 4. Scripts (all on the VM, ~ = /home/auto)

### ~/jobs/vd-start.sh [N]  (default 2, max 10)
Starts vd1..vdN. Idempotent: displays already up are skipped, never
duplicated or killed. Waits up to 10s per Xvfb socket
(/tmp/.X11-unix/Xn). Logs to ~/jobs/virtual-desktops/logs/.
Rejects N > 10 and non-numeric input.

### ~/jobs/vd-stop.sh [N|all]  (default all)
Stops vd1..vdN, or every running vd desktop when given `all` (scans
/tmp/.X11-unix/X* for n >= 2). Kill strategy per display: first kills every
process whose /proc/<pid>/environ contains exactly DISPLAY=:<n> (openbox,
chrome), then `pkill -f "x11vnc -display :<n>"` and `pkill -f "Xvfb :<n>"`
(those two were started with -display flags and carry no DISPLAY env),
then removes the socket and lock files. Can never touch :0/:1 because it
only ever targets n >= 2, and its own cmdline ("vd-stop.sh ...") never
contains the pkill literals (they live inside the script file).

### ~/jobs/virtual-desktops/vd-chrome.sh vdK [url]  (K = 1..10)
Launches or relaunches headful Chrome on vdK (default URL about:blank).
Validates the name (vd1..vd10) and refuses unless that desktop's X socket
is up ("run vd-start.sh first"). Kills only Chrome bound to that exact
--user-data-dir, drops stale window geometry
(<profile>/Default/Preferences, fixes the 10x10-window stuck state),
then launches with the flags from section 3. Logs per desktop.

### Supporting files
- ~/jobs/virtual-desktops/README.md : operator docs (usage, mapping,
  viewing, bridge targeting, resource guidance, gotchas).
- ~/jobs/virtual-desktops/logs/ : per-component logs
  (xvfb-<n>.log, openbox-<n>.log, x11vnc-<n>.log, chrome-vdK.log).
- ~/jobs/virtual-desktops/legacy-2026-11-02/ : the original hardcoded
  2-desktop scripts, kept as rollback reference.

Run from the agent VM via cb-ssh.sh, e.g.:
  ./cb-ssh.sh "~/jobs/vd-start.sh 5"
  ./cb-ssh.sh "~/jobs/virtual-desktops/vd-chrome.sh vd3 https://example.com"
  ./cb-ssh.sh "~/jobs/vd-stop.sh all"

## 5. Usage recipes

Scale up:   ./cb-ssh.sh "~/jobs/vd-start.sh 8"     # vd1..vd8
Scale down: ./cb-ssh.sh "~/jobs/vd-stop.sh 3"      # leaves vd1..vd3
Full stop:  ./cb-ssh.sh "~/jobs/vd-stop.sh all"
Chrome:     ./cb-ssh.sh "~/jobs/virtual-desktops/vd-chrome.sh vd5 <url>"

View a desktop over VNC (VNC is localhost-bound on the VM, same as the
existing :5902 convention; tunnel first):
  ssh -L 5905:127.0.0.1:5905 auto@<tailnet-ip>   # then VNC to localhost:5905 for vd3
Screenshot without VNC:
  ./cb-ssh.sh "DISPLAY=:4 scrot -z /tmp/vd3.png"

## 6. Targeting a desktop from a bridge / automation client

Three equivalent handles, all per-desktop isolated:
1. DISPLAY env: prefix any X command with DISPLAY=:<K+1>.
2. CDP: each Chrome exposes remote debugging at 127.0.0.1:(9332+K).
   A bridge instance attaches via http://127.0.0.1:<port>/json/list exactly
   like the main bridge does on :9222 for :1. One bridge per desktop
   (separate port, separate profile, separate user-data-dir) keeps sessions
   fully isolated.
3. Profiles: ~/.config/chrome-vdK are independent Chrome profiles.

## 7. Resource measurements (taken 2026-11-02 on this 8GB box)

- One settled headful Chrome costs ~240MB RAM.
- 5 concurrent Chromes: 5.8GB/7.8GB used, ~2.0GB available, 5/5 CDP ports
  live, screenshots render fine. Comfortable.
- 10 concurrent Chromes: 6.8GB/7.8GB used, ~1.0GB available, load spiked to
  ~10.5 during startup then settled, ZERO OOM kills, 10/10 CDP ports live,
  example.com fully rendered on vd10. Works but tight.
- Practical ceiling (recorded in README): 10 works for short bursts (e.g. a
  10-job wave launch); for sustained parallel work keep it at 6 or fewer to
  leave ~2GB headroom for the :1 automation stack and the OS. Beyond 6
  steady sessions, stagger Chrome launches (startup is when load spikes).
- Idle steady state: vd1+vd2 running with Chrome, ~3.3GB available.

## 8. Verification history (2026-11-02)

- Phase 1 (2 desktops): screenshots visually inspected, vd1 fullscreen
  Chrome on about:blank, vd2 fullscreen with example.com fully rendered.
  Full stop/start cycle: vd-stop.sh removed :2/:3 only (X0/X1 sockets
  intact, :1 x11vnc untouched, 0 stray processes); restart brought back 2
  Xvfb, listeners on 5903/5904/9333/9334.
- Phase 2 (scaling): input validation (vd-start.sh 11/abc rejected,
  vd-chrome.sh vd11/bogus rejected, vd-chrome on down desktop gives clear
  "run vd-start.sh first"). 5-desktop cycle: start 5, Chrome on all 5,
  screenshots of :2 and :6 render, 5/5 CDP live, stop-all leaves only X0/X1
  with mirror services active. 10-desktop stress: 10/10 Xvfb+openbox+x11vnc
  up, 10/10 Chromes launched, 10/10 CDP live, vd10 screenshot fully
  rendered, no OOM. Steady state restored to vd1+vd2.

## 9. Gotchas learned (also in README)

1. Never `pkill -f` with a literal profile string from an ssh one-liner:
   the invoking shell's own cmdline matches and kills itself (hit twice
   during build). All pkill literals live inside script files; when probing
   from a one-liner use a self-excluding pattern like
   `pgrep -f "chrome-vd[0-9]"`.
2. Bash `set -u` pitfall: `local a="$1" b="$a"` fails with "unbound
   variable" because the RHS expands before the local takes effect. Split
   into two `local` lines (this exact bug shipped in vd-stop.sh and was
   fixed during testing).
3. Chrome sometimes maps at 10x10 first under openbox; --start-maximized
   plus a few seconds settles it. If stuck, vd-chrome.sh already deletes
   <profile>/Default/Preferences on relaunch.
4. Xvfb resolution is fixed at 1920x1080x24 in vd-start.sh.
5. x11vnc needs explicit -cursor -cursor_drag or the cursor does not render
   through VNC (same lesson as the existing :5902 mirror feed).

## 10. Open design questions for the reviewing agent

- Bridge-per-desktop vs one bridge multiplexing CDP ports: current docs
  recommend one bridge instance per desktop for full session isolation;
  a single client multiplexing 10 CDP endpoints is lighter but shares fate.
- Profile seeding: vd profiles start blank. If jobs need his signed-in
  sessions, decide per job whether to copy cookies in or sign in fresh
  (note: his Google sign-in on the VM is his own action; agents never
  create it).
- No systemd units were created: desktops are started on demand by script.
  If they should survive VM reboots, add user units (linger is already on
  for `auto`).
- VNC has no password (-nopw) but is localhost-bound; remote viewing goes
  through the SSH tunnel. If a password is wanted, add -rfbauth per port.
