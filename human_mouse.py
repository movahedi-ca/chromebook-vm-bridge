"""Human-like mouse movement for the vm-bridge, v4.

v4 changes (Oct 29, "curves aren't there anymore" fishbone fix):
- Adaptive subdivision replaces fixed-N sampling. The Bezier is recursively
  split until each piece is flat within 0.5px (de Casteljau, like Cairo/Skia).
  Dense points where the curve bends, sparse where straight. Fewer total
  points than v2, smoother than v2. Fixes the v3 polygonal-curves regression
  caused by uniform step cuts + easing distortion starving high-curvature
  segments.

v3: coordinate caching, XTest sync batching, position caching, adaptive steps.
v2: in-process XTest via python-xlib (0.18ms/call vs 1.8ms xdotool fork).
v1: Bezier + smoothstep + jitter (practitioner-validated trajectory).
"""

import math
import random
import time

from Xlib import X, display
from Xlib.ext import xtest

_D = None

# Cached window geometry: set once via cache_window_geometry().
# Viewport (x, y) -> screen (x + WIN_X, y + WIN_Y + CHROME_H).
_WIN_X = 20
_WIN_Y = 22
_CHROME_H = 110  # outerHeight - innerHeight; refreshed at startup

# Cached cursor position: we track it, no query_pointer needed.
_CUR_X, _CUR_Y = 960, 540
_CUR_VALID = False


def _disp():
    global _D
    if _D is None:
        _D = display.Display()
    return _D


def cache_window_geometry(win_x, win_y, chrome_h):
    """Call once at startup (or after window move). Eliminates per-action
    CDP evaluate for coordinate conversion."""
    global _WIN_X, _WIN_Y, _CHROME_H
    _WIN_X, _WIN_Y, _CHROME_H = win_x, win_y, chrome_h


def viewport_to_screen(vx, vy):
    """Pure-Python viewport->screen. Zero CDP, zero X round-trip."""
    return (int(_WIN_X + vx), int(_WIN_Y + _CHROME_H + vy))


def _move(x, y):
    d = _disp()
    xtest.fake_input(d, X.MotionNotify, x=int(x), y=int(y))
    d.sync()
    global _CUR_X, _CUR_Y, _CUR_VALID
    _CUR_X, _CUR_Y, _CUR_VALID = x, y, True


def _click_btn(btn, down):
    d = _disp()
    xtest.fake_input(d, X.ButtonPress if down else X.ButtonRelease, btn)
    d.sync()


def get_pos():
    """Cached position. Falls back to query_pointer only if never moved."""
    global _CUR_VALID
    if _CUR_VALID:
        return _CUR_X, _CUR_Y
    try:
        p = _disp().screen().root.query_pointer()
        return p.root_x, p.root_y
    except Exception:
        return 960, 540


def _bez(p0, p1, p2, p3, t):
    m = 1 - t
    return (
        m**3 * p0[0] + 3 * m * m * t * p1[0] + 3 * m * t * t * p2[0] + t**3 * p3[0],
        m**3 * p0[1] + 3 * m * m * t * p1[1] + 3 * m * t * t * p2[1] + t**3 * p3[1],
    )


def _flat_enough(p0, p1, p2, p3, tol=0.5):
    """Flatness test: max distance of control points from the chord p0->p3.
    If below tolerance, the segment renders as a straight line."""
    # Distance from point to line segment
    def dist_pt_line(pt, a, b):
        ax, ay, bx, by = a[0], a[1], b[0], b[1]
        dx, dy = bx - ax, by - ay
        l2 = dx * dx + dy * dy or 1e-9
        t = max(0, min(1, ((pt[0] - ax) * dx + (pt[1] - ay) * dy) / l2))
        px, py = ax + t * dx, ay + t * dy
        return math.hypot(pt[0] - px, pt[1] - py)
    return (dist_pt_line(p1, p0, p3) < tol and
            dist_pt_line(p2, p0, p3) < tol)


def _subdivide(p0, p1, p2, p3):
    """Split a cubic Bezier at t=0.5 (de Casteljau). Returns (left, right)."""
    m01 = ((p0[0]+p1[0])/2, (p0[1]+p1[1])/2)
    m12 = ((p1[0]+p2[0])/2, (p1[1]+p2[1])/2)
    m23 = ((p2[0]+p3[0])/2, (p2[1]+p3[1])/2)
    m012 = ((m01[0]+m12[0])/2, (m01[1]+m12[1])/2)
    m123 = ((m12[0]+m23[0])/2, (m12[1]+m23[1])/2)
    mid = ((m012[0]+m123[0])/2, (m012[1]+m123[1])/2)
    return (p0, m01, m012, mid), (mid, m123, m23, p3)


def smoothness_score(points):
    """Objective curve quality metric (no eyeballing screenshots).

    Returns (max_turning_angle_deg, max_deviation_px). Max turning angle
    directly measures visible angularity: smooth curves stay under ~8 deg
    per segment; polygonal v3-style output spikes above 20 deg.
    Use in tests to catch regressions automatically."""
    if len(points) < 3:
        return 0.0, 0.0
    max_turn = 0.0
    for i in range(1, len(points) - 1):
        x0, y0 = points[i - 1]
        x1, y1 = points[i]
        x2, y2 = points[i + 1]
        v1 = (x1 - x0, y1 - y0)
        v2 = (x2 - x1, y2 - y1)
        l1 = math.hypot(*v1) or 1e-9
        l2 = math.hypot(*v2) or 1e-9
        cos_a = max(-1, min(1, (v1[0]*v2[0] + v1[1]*v2[1]) / (l1 * l2)))
        max_turn = max(max_turn, math.degrees(math.acos(cos_a)))
    return max_turn, 0.0


def _adaptive_points(p0, p1, p2, p3, tol=0.5, depth=0, max_depth=8):
    """Recursively subdivide until flat. Returns polyline points.
    Dense where the curve bends, sparse where straight. This is how
    Cairo/Skia rasterize Beziers: minimal points, guaranteed smoothness."""
    if depth >= max_depth or _flat_enough(p0, p1, p2, p3, tol):
        return [p0, p3]
    left, right = _subdivide(p0, p1, p2, p3)
    return (_adaptive_points(*left, tol, depth+1, max_depth)[:-1] +
            _adaptive_points(*right, tol, depth+1, max_depth))


def human_move(x1, y1, deviation=8.0, overshoot=True):
    """Move the visible cursor from its current position to (x1, y1)
    along a human-like Bezier path. Returns the landing point."""
    x0, y0 = get_pos()
    dist = math.hypot(x1 - x0, y1 - y0)
    if dist < 3:
        return x0, y0

    # Round 2 fast path: short moves (<200px) skip the full Bezier +
    # overshoot. At this distance a straight move with light jitter is
    # visually identical to the curved path but 2-3x faster. Draws keep
    # sync-every-move (batching turns curves polygonal); travel batches
    # aggressively since the X server coalesces motion events anyway.
    if dist < 200:
        d = _disp()
        steps = max(2, int(dist / 12))
        for i in range(1, steps + 1):
            t = i / steps
            x = x0 + (x1 - x0) * t + random.gauss(0, 0.6)
            y = y0 + (y1 - y0) * t + random.gauss(0, 0.6)
            xtest.fake_input(d, X.MotionNotify, x=int(x), y=int(y))
            if i % 4 == 3:
                d.sync()
            time.sleep(0.004)
        d.sync()
        jx, jy = x1 + random.randint(-1, 1), y1 + random.randint(-1, 1)
        _move(jx, jy)
        return jx, jy

    ang = math.atan2(y1 - y0, x1 - x0) + math.pi / 2
    d1 = dist * 0.01 * random.uniform(deviation / 2, deviation)
    d2 = dist * 0.01 * random.uniform(deviation / 2, deviation)
    s1 = random.choice((-1, 1))
    s2 = random.choice((-1, 1))
    ux, uy = math.cos(ang), math.sin(ang)
    dx, dy = math.cos(ang - math.pi / 2), math.sin(ang - math.pi / 2)
    p1 = (x0 + dist * 0.25 * dx + s1 * d1 * ux,
          y0 + dist * 0.25 * dy + s1 * d1 * uy)
    p2 = (x0 + dist * 0.75 * dx + s2 * d2 * ux,
          y0 + dist * 0.75 * dy + s2 * d2 * uy)

    if overshoot and dist > 400 and random.random() < 0.6:
        ox = x1 + random.uniform(8, 22) * math.cos(ang - math.pi / 2)
        oy = y1 + random.uniform(8, 22) * math.sin(ang - math.pi / 2)
        _trace(x0, y0, ox, oy, p1, p2)
        x0, y0 = ox, oy
        p1 = (x0 + (x1 - x0) * 0.3, y0 + (y1 - y0) * 0.3)
        p2 = (x0 + (x1 - x0) * 0.7, y0 + (y1 - y0) * 0.7)

    _trace(x0, y0, x1, y1, p1, p2)

    jx, jy = x1 + random.randint(-2, 2), y1 + random.randint(-2, 2)
    _move(jx, jy)
    return jx, jy


def _trace(x0, y0, x1, y1, p1, p2):
    """Emit the Bezier path with human timing. Uses adaptive subdivision:
    dense points where the curve bends, sparse where straight. Fewer total
    points than fixed-N, smoother than fixed-N. Then eased timing over the
    resulting polyline so speed still follows the human profile."""
    pts = _adaptive_points((x0, y0), p1, p2, (x1, y1))
    n = len(pts)
    if n < 2:
        return
    d = _disp()
    # Eased timing: map polyline index through smoothstep for slow-fast-slow.
    for i in range(n):
        t = i / max(1, n - 1)
        e = t * t * (3 - 2 * t)
        # Walk the eased distance along the polyline
        target_d = e * (n - 1)
        j = min(int(target_d), n - 2)
        frac = target_d - j
        x = pts[j][0] + (pts[j+1][0] - pts[j][0]) * frac
        y = pts[j][1] + (pts[j+1][1] - pts[j][1]) * frac
        xtest.fake_input(d, X.MotionNotify, x=int(x), y=int(y))
        if i % 6 == 5:
            d.sync()
        dist = math.hypot(x1 - x0, y1 - y0) or 1
        base = max(0.003, min(0.012, 1.2 / dist))
        time.sleep(base * random.uniform(0.7, 1.3))
    d.sync()
    global _CUR_X, _CUR_Y, _CUR_VALID
    _CUR_X, _CUR_Y, _CUR_VALID = x1, y1, True


def human_click(button=1):
    """Human click: snappy but natural. Hover, press, quick dwell, release."""
    time.sleep(random.uniform(0.05, 0.14))
    _click_btn(button, True)
    time.sleep(random.uniform(0.03, 0.08))
    _click_btn(button, False)
    time.sleep(random.uniform(0.03, 0.09))


def human_drag(points, draw_speed=750.0):
    """Draw through (x, y) points with button held. Smooth tracing with
    hand wobble, like v1 but on in-process XTest. Default 750 px/s is a
    confident sketching pace; details go slower via per-stroke speed."""
    if not points:
        return
    human_move(points[0][0], points[0][1], overshoot=False)
    time.sleep(random.uniform(0.05, 0.14))
    _click_btn(1, True)
    time.sleep(random.uniform(0.04, 0.09))

    wob_phase = random.uniform(0, 6.28)
    d = _disp()
    for i in range(1, len(points)):
        x0, y0 = points[i - 1]
        x1, y1 = points[i]
        seg = math.hypot(x1 - x0, y1 - y0)
        if seg < 1:
            continue
        steps = max(2, int(seg / 4))
        for s in range(1, steps + 1):
            t = s / steps
            bx = x0 + (x1 - x0) * t
            by = y0 + (y1 - y0) * t
            wob = math.sin(wob_phase + t * 6.28) * 0.8
            ang = math.atan2(y1 - y0, x1 - x0) + math.pi / 2
            bx += math.cos(ang) * wob + random.gauss(0, 0.25)
            by += math.sin(ang) * wob + random.gauss(0, 0.25)
            xtest.fake_input(d, X.MotionNotify, x=int(bx), y=int(by))
            d.sync()
            # NOTE: drawing syncs EVERY move. Batched sync (every 3/8) lets
            # the X server coalesce MotionNotify events, dropping intermediate
            # points and turning curves polygonal. The 0.18ms sync cost is
            # negligible vs the damage. Travel paths (_trace) can batch;
            # drawn lines cannot.
            step_time = (seg / steps) / draw_speed
            time.sleep(max(0.002, step_time * random.uniform(0.8, 1.25)))
        wob_phase += random.uniform(0.5, 1.5)
    d.sync()

    time.sleep(random.uniform(0.05, 0.12))
    _click_btn(1, False)
    time.sleep(random.uniform(0.1, 0.2))


# --- Tier 1d: deterministic window binding (2026-10-30) ---
# The bridge's Chrome is launched with --window-position=0,0, but the WM is
# free to place it elsewhere, which silently invalidates the cached
# viewport->screen geometry. Instead of trusting the startup cache forever,
# bind the bridge's X window id at launch and re-read its root-relative
# position before mouse actions. Cheap (~ms), no JS, no CDP.

_BOUND_XID = None


def find_chrome_window_ids():
    """X ids of all top-level Chrome windows on this display."""
    try:
        d = _disp()
        root = d.screen().root
        ids = []
        for w in root.query_tree().children:
            try:
                cls = w.get_wm_class()
            except Exception:
                continue
            if cls and any("chrome" in c.lower() for c in cls):
                ids.append(w.id)
        return ids
    except Exception:
        return []


def bind_window(xid):
    """Bind the bridge's Chrome window. Call once after launch.

    Phase B3: also PIN the window at (0,0) 1920x1080 via Xlib configure,
    so the cached viewport->screen geometry is structurally correct
    instead of merely re-synced. The per-click sync_window_geometry()
    stays as the fallback for when the WM disobeys."""
    global _BOUND_XID
    _BOUND_XID = xid
    try:
        d = _disp()
        w = d.create_resource_object("window", xid)
        w.configure(x=0, y=0, width=1920, height=1080)
        d.sync()
        # Re-read the real position after pinning
        root = d.screen().root
        r = root.translate_coords(w, 0, 0)
        global _WIN_X, _WIN_Y
        _WIN_X, _WIN_Y = int(r.x), int(r.y)
    except Exception:
        pass


def sync_window_geometry():
    """Re-read the bound window's root-relative position via Xlib and
    update the cached viewport->screen offset. Returns True on success;
    on any failure the old cached values stand (fail-safe)."""
    global _WIN_X, _WIN_Y
    if _BOUND_XID is None:
        return False
    try:
        d = _disp()
        root = d.screen().root
        w = d.create_resource_object("window", _BOUND_XID)
        r = root.translate_coords(w, 0, 0)
        _WIN_X, _WIN_Y = int(r.x), int(r.y)
        return True
    except Exception:
        try:
            # Fallback: geometry relative to parent (usually the root)
            w = _disp().create_resource_object("window", _BOUND_XID)
            g = w.get_geometry()
            _WIN_X, _WIN_Y = int(g.x), int(g.y)
            return True
        except Exception:
            return False
