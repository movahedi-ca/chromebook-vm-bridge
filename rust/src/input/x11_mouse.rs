//! X11/XTest human-like mouse driver.
//!
//! Port of `human_mouse.py` v4 with the audit findings fixed:
//!
//! - **One** `x11rb` connection behind `std::sync::Mutex`. This module is
//!   fully synchronous blocking X11 I/O: **NEVER hold the mutex across
//!   `.await`**. Async callers MUST wrap every method in
//!   `tokio::task::spawn_blocking`.
//! - `query_pointer()` runs **before every trajectory** (fixes the stale
//!   `_CUR_X`/`_CUR_Y` cache that desynced on the watched VNC display).
//! - Bezier curves for moves down to 15px (legacy skipped <200px).
//! - Adaptive subdivision terminates on flatness **and** a 12px chord cap
//!   (fixes straight-line teleportation).
//! - Easing is applied by **arc length**, not point index.
//! - Real `smoothness_score` returning `(score_deg, max_deviation_px)`
//!   (legacy returned a hardcoded `0.0` stub for the deviation).
//! - **SEC-1:** coordinates outside the verified Chrome window rect are
//!   REJECTED with an error. The driver never clicks the raw desktop.
//!   Rejection (not clamping) is deliberate: silently moving a click target
//!   is worse than refusing it.
#![allow(clippy::cast_possible_truncation)] // trajectory math: f64 px -> i32 screen coords; values are range-checked before conversion
#![allow(clippy::cast_possible_wrap)] // u32 -> i32 for X11 coords; display dimensions never exceed i32::MAX
#![allow(clippy::cast_precision_loss)] // usize counts -> f64 for easing math; sub-pixel precision is irrelevant
#![allow(clippy::cast_sign_loss)] // f64 distances -> usize indices; always non-negative by construction
#![allow(clippy::many_single_char_names)] // x/y/t coordinate math follows convention
#![allow(clippy::similar_names)] // e.g. `p`/`pts`, `x`/`xd` are distinct roles in trajectory code

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::AtomEnum;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xproto::Window;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

// ---------------------------------------------------------------------------
// X11 protocol event types (stable core-protocol constants).
// ---------------------------------------------------------------------------
const MOTION_NOTIFY: u8 = 6;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;

// ---------------------------------------------------------------------------
// Trajectory tuning (from the audit).
// ---------------------------------------------------------------------------
/// Moves shorter than this use a single linear step; Bezier above it.
const MIN_BEZIER_DIST_PX: f64 = 15.0;
/// No two consecutive trajectory points may be farther apart than this.
const MAX_CHORD_PX: f64 = 12.0;
/// de Casteljau flatness tolerance (Cairo/Skia-style).
const FLATNESS_TOL_PX: f64 = 0.5;
const MAX_SUBDIV_DEPTH: u32 = 10;
/// Uniform spacing after arc-length resampling.
const RESAMPLE_SPACING_PX: f64 = 4.0;

type Pt = (f64, f64);
/// A cubic Bezier leg: four control points.
type Cubic = (Pt, Pt, Pt, Pt);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Verified Chrome window rectangle on the root screen, in screen pixels.
/// Populated from a fresh JS geometry evaluation before each visible action
/// (the old Xlib `translate_coords` path was mathematically inverted and is
/// deleted; see playbook §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// SEC-1 gate: reject any coordinate outside the verified window rect.
///
/// Free function (no X11 needed) so it is unit-testable and so callers can
/// pre-validate before acquiring the mouse.
pub fn check_in_window(rect: Option<&WindowRect>, x: i32, y: i32) -> Result<()> {
    let r = rect
        .ok_or_else(|| anyhow!("SEC-1: window rect not set; refusing to drive the raw desktop"))?;
    let inside = x >= r.x && y >= r.y && x < r.x + r.w as i32 && y < r.y + r.h as i32;
    if inside {
        Ok(())
    } else {
        Err(anyhow!(
            "SEC-1: coordinate ({x},{y}) outside verified window rect \
             ({},{},{},{}) — rejected, not clamped",
            r.x,
            r.y,
            r.w,
            r.h
        ))
    }
}

/// Resolve the supervised Chrome window's rectangle from X11 ground truth
/// (audit 42 Critical-1).
///
/// Enumerates `_NET_CLIENT_LIST`, matches `_NET_WM_PID` against the
/// supervisor's child pid, and translates the window origin to root
/// coordinates. Page-reported geometry (`window.screenX`, `outerWidth`,
/// …) is NEVER consulted here: a hostile page controls those values and
/// could otherwise steer the SEC-1 gate rect — and with it root-equivalent
/// `XTest` input — onto another window on the shared display.
///
/// `translate_coords` direction (do not "simplify"): the RECEIVER is the
/// window and the destination is the root, so the reply is the window's
/// origin in root coordinates. The old inverted call (`root` as receiver)
/// answered "where is the root origin inside the window" and produced
/// BUG-1; it must never come back.
///
/// Blocking X11 I/O: callers MUST wrap this in `spawn_blocking`.
pub fn resolve_window_rect(display: Option<&str>, chrome_pid: u32) -> Result<WindowRect> {
    let (conn, screen_num) =
        RustConnection::connect(display).context("connecting to X11 display")?;
    let root = conn.setup().roots[screen_num].root;
    let client_list_atom = conn
        .intern_atom(false, b"_NET_CLIENT_LIST")
        .context("intern _NET_CLIENT_LIST")?
        .reply()
        .context("intern _NET_CLIENT_LIST reply")?
        .atom;
    let wm_pid_atom = conn
        .intern_atom(false, b"_NET_WM_PID")
        .context("intern _NET_WM_PID")?
        .reply()
        .context("intern _NET_WM_PID reply")?
        .atom;
    let list = conn
        .get_property(false, root, client_list_atom, AtomEnum::WINDOW, 0, u32::MAX)
        .context("read _NET_CLIENT_LIST")?
        .reply()
        .context("_NET_CLIENT_LIST reply")?;
    if list.format != 32 {
        anyhow::bail!("_NET_CLIENT_LIST has unexpected format {}", list.format);
    }
    for win_bytes in list.value.chunks_exact(4) {
        let win = u32::from_ne_bytes([win_bytes[0], win_bytes[1], win_bytes[2], win_bytes[3]]);
        let Ok(prop) = conn
            .get_property(false, win, wm_pid_atom, AtomEnum::CARDINAL, 0, 1)
            .context("read _NET_WM_PID")?
            .reply()
        else {
            continue;
        };
        if prop.format != 32 || prop.value.len() < 4 {
            continue;
        }
        let pid = u32::from_ne_bytes([prop.value[0], prop.value[1], prop.value[2], prop.value[3]]);
        if pid != chrome_pid {
            continue;
        }
        let pos = conn
            .translate_coordinates(win, root, 0, 0)
            .context("translate_coords")?
            .reply()
            .context("translate_coords reply")?;
        let geo = conn
            .get_geometry(win)
            .context("get_geometry")?
            .reply()
            .context("get_geometry reply")?;
        return Ok(WindowRect {
            x: i32::from(pos.dst_x),
            y: i32::from(pos.dst_y),
            w: u32::from(geo.width),
            h: u32::from(geo.height),
        });
    }
    Err(anyhow!(
        "no X11 window with _NET_WM_PID={chrome_pid} on this display"
    ))
}

// ---------------------------------------------------------------------------
// Minimal PRNG (xorshift64* + Box-Muller). Mouse jitter needs no crypto RNG,
// and this avoids adding a `rand` dependency for one module.
// ---------------------------------------------------------------------------

struct XorShift64(u64);

impl XorShift64 {
    fn seeded() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        Self(nanos | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn next_f64(&mut self) -> f64 {
        const DIV: f64 = (1u64 << 53) as f64;
        ((self.next_u64() >> 11) as f64) / DIV
    }

    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.next_f64() * (hi - lo)
    }

    /// Standard normal via Box-Muller.
    fn gauss(&mut self) -> f64 {
        let u1 = self.next_f64().max(f64::MIN_POSITIVE);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

// ---------------------------------------------------------------------------
// Pure geometry helpers (all unit-testable, no X11).
// ---------------------------------------------------------------------------

fn dist(a: Pt, b: Pt) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn lerp(a: Pt, b: Pt, t: f64) -> Pt {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn dist_point_to_segment(p: Pt, a: Pt, b: Pt) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let l2 = (dx * dx + dy * dy).max(1e-9);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
    dist(p, (a.0 + t * dx, a.1 + t * dy))
}

/// Cubic Bezier evaluation (de Casteljau form).
#[allow(dead_code)] // parked: used by the unwired smooth-drag path (audit LOW-1)
fn bez(p0: Pt, p1: Pt, p2: Pt, p3: Pt, t: f64) -> Pt {
    let m = 1.0 - t;
    (
        m * m * m * p0.0 + 3.0 * m * m * t * p1.0 + 3.0 * m * t * t * p2.0 + t * t * t * p3.0,
        m * m * m * p0.1 + 3.0 * m * m * t * p1.1 + 3.0 * m * t * t * p2.1 + t * t * t * p3.1,
    )
}

fn flat_enough(p0: Pt, p1: Pt, p2: Pt, p3: Pt, tol: f64) -> bool {
    dist_point_to_segment(p1, p0, p3) < tol && dist_point_to_segment(p2, p0, p3) < tol
}

fn subdivide(p0: Pt, p1: Pt, p2: Pt, p3: Pt) -> (Cubic, Cubic) {
    let m01 = lerp(p0, p1, 0.5);
    let m12 = lerp(p1, p2, 0.5);
    let m23 = lerp(p2, p3, 0.5);
    let m012 = lerp(m01, m12, 0.5);
    let m123 = lerp(m12, m23, 0.5);
    let mid = lerp(m012, m123, 0.5);
    ((p0, m01, m012, mid), (mid, m123, m23, p3))
}

/// Adaptive subdivision. Terminates when the segment is BOTH flat within
/// tolerance AND no longer than `MAX_CHORD_PX` (the audit fix: the legacy only
/// checked flatness, letting long straight runs collapse into teleports),
/// or at max depth. Appends points after `p0` into `out`.
fn adaptive_points(p0: Pt, p1: Pt, p2: Pt, p3: Pt, depth: u32, out: &mut Vec<Pt>) {
    let chord = dist(p0, p3);
    if depth >= MAX_SUBDIV_DEPTH
        || (flat_enough(p0, p1, p2, p3, FLATNESS_TOL_PX) && chord <= MAX_CHORD_PX)
    {
        out.push(p3);
        return;
    }
    let (l, r) = subdivide(p0, p1, p2, p3);
    adaptive_points(l.0, l.1, l.2, l.3, depth + 1, out);
    adaptive_points(r.0, r.1, r.2, r.3, depth + 1, out);
}

fn polyline_len(pts: &[Pt]) -> f64 {
    pts.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// Uniform arc-length resampling: walk the polyline emitting a point every
/// `spacing` px. Guarantees the chord cap survives to the emitted path.
fn resample_arc_length(points: &[Pt], spacing: f64) -> Vec<Pt> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let mut out = vec![points[0]];
    let mut carry = 0.0; // distance already covered since last emitted point
    let mut prev = points[0];
    for &cur in &points[1..] {
        let mut seg_start = prev;
        let mut seg_len = dist(seg_start, cur);
        while carry + seg_len >= spacing {
            let need = spacing - carry;
            let t = need / seg_len;
            let emit = lerp(seg_start, cur, t);
            out.push(emit);
            seg_start = emit;
            seg_len = dist(seg_start, cur);
            carry = 0.0;
        }
        carry += seg_len;
        prev = cur;
    }
    // `out` is provably non-empty here (seeded with points[0] above), but
    // avoid unwrap in non-test code (pre-emptive hardening): pattern-match.
    if let (Some(&last), Some(&o)) = (points.last(), out.last()) {
        if dist(o, last) > 1e-9 {
            out.push(last);
        }
    }
    out
}

/// Position at arc distance `d` along the polyline.
fn point_at_distance(pts: &[Pt], d: f64) -> Pt {
    let mut acc = 0.0;
    for w in pts.windows(2) {
        let seg = dist(w[0], w[1]);
        if acc + seg >= d {
            let t = if seg < 1e-9 { 0.0 } else { (d - acc) / seg };
            return lerp(w[0], w[1], t.clamp(0.0, 1.0));
        }
        acc += seg;
    }
    *pts.last().unwrap_or(&(0.0, 0.0))
}

/// Build one cubic Bezier leg p0 -> p3 with human-like control points.
/// Returns a uniformly resampled polyline (or capped linear steps for
/// short legs). The chord cap holds on every path.
fn cubic_leg(p0: Pt, p3: Pt, rng: &mut XorShift64) -> Vec<Pt> {
    let d = dist(p0, p3);
    if d < MIN_BEZIER_DIST_PX {
        if d <= MAX_CHORD_PX {
            return vec![p0, p3];
        }
        // Short leg but over the chord cap (e.g. an overshoot return):
        // split into capped linear steps rather than one long jump.
        let n = (d / MAX_CHORD_PX).ceil() as usize;
        return (0..=n).map(|i| lerp(p0, p3, i as f64 / n as f64)).collect();
    }
    // Perpendicular jitter for the control points (ported from human_mouse.py).
    let ang = (p3.1 - p0.1).atan2(p3.0 - p0.0) + std::f64::consts::FRAC_PI_2;
    let (ux, uy) = (ang.cos(), ang.sin());
    let along = ang - std::f64::consts::FRAC_PI_2;
    let (dxu, dyu) = (along.cos(), along.sin());
    let d1 = d * 0.01 * rng.uniform(4.0, 8.0);
    let d2 = d * 0.01 * rng.uniform(4.0, 8.0);
    let s1: f64 = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
    let s2: f64 = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
    let p1 = (
        p0.0 + d * 0.25 * dxu + s1 * d1 * ux,
        p0.1 + d * 0.25 * dyu + s1 * d1 * uy,
    );
    let p2 = (
        p0.0 + d * 0.75 * dxu + s2 * d2 * ux,
        p0.1 + d * 0.75 * dyu + s2 * d2 * uy,
    );
    let mut raw = vec![p0];
    adaptive_points(p0, p1, p2, p3, 0, &mut raw);
    resample_arc_length(&raw, RESAMPLE_SPACING_PX)
}

/// Full trajectory from (x0,y0) to (x1,y1), including the 60%-probability
/// overshoot-and-correct for long moves. Uniformly resampled.
fn build_trajectory(x0: f64, y0: f64, x1: f64, y1: f64, rng: &mut XorShift64) -> Vec<Pt> {
    let d = dist((x0, y0), (x1, y1));
    if d < MIN_BEZIER_DIST_PX {
        return vec![(x0, y0), (x1, y1)];
    }
    // Leg endpoints: optionally overshoot first, then correct.
    let mut legs: Vec<(Pt, Pt)> = Vec::new();
    let mut sx = x0;
    let mut sy = y0;
    if d > 400.0 && rng.next_f64() < 0.6 {
        let along = (y1 - y0).atan2(x1 - x0);
        let ox = x1 + rng.uniform(8.0, 22.0) * along.cos();
        let oy = y1 + rng.uniform(8.0, 22.0) * along.sin();
        legs.push(((sx, sy), (ox, oy)));
        sx = ox;
        sy = oy;
    }
    legs.push(((sx, sy), (x1, y1)));

    let mut pts = vec![(x0, y0)];
    for (a, b) in legs {
        let leg = cubic_leg(a, b, rng);
        pts.extend(leg.into_iter().skip(1)); // skip duplicated joint
    }
    pts
}

// ---------------------------------------------------------------------------
// X11Mouse
// ---------------------------------------------------------------------------

pub struct X11Mouse {
    conn: Mutex<RustConnection>,
    root: Window,
    window_rect: Option<WindowRect>,
    rng_salt: AtomicU64,
}

impl X11Mouse {
    /// Connect to the given X11 display. `None` falls back to the ambient
    /// $DISPLAY (manual runs); production callers pass the configured
    /// display so input injection can never diverge from the display the
    /// browser was spawned on (audit 41 LOW-6).
    pub fn new(display: Option<&str>) -> Result<Self> {
        let (conn, screen_num) =
            RustConnection::connect(display).context("connecting to X11 display")?;
        let root = conn.setup().roots[screen_num].root;
        Ok(Self {
            conn: Mutex::new(conn),
            root,
            window_rect: None,
            rng_salt: AtomicU64::new(1),
        })
    }

    /// Set the verified Chrome window rect (from fresh JS geometry).
    /// Must be called before any action; actions fail closed without it.
    pub fn set_window_rect(&mut self, rect: WindowRect) {
        self.window_rect = Some(rect);
    }

    fn rng(&self) -> XorShift64 {
        let mut r = XorShift64::seeded();
        // Mix in a per-instance counter so rapid successive calls differ.
        let salt = self.rng_salt.fetch_add(1, Ordering::Relaxed);
        r.0 ^= salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        r
    }

    fn check(&self, x: i32, y: i32) -> Result<()> {
        check_in_window(self.window_rect.as_ref(), x, y)
    }

    /// Reject a whole trajectory when ANY point would leave the window
    /// rect (audit 42 Medium): the overshoot leg and per-point jitter can
    /// push intermediate points outside even when both endpoints are
    /// inside. Rejection (not clamping) preserves the driver's contract —
    /// a clamped path would click somewhere the caller never asked for.
    fn check_trajectory(&self, pts: &[Pt]) -> Result<()> {
        for &(px, py) in pts {
            // Same rounding the emitter applies, so the check sees exactly
            // what XTest would receive.
            self.check(px.round() as i32, py.round() as i32)?;
        }
        Ok(())
    }

    /// Live pointer position from the X server. Called before EVERY
    /// trajectory — never trust a cached position on a watched display.
    fn query_pos(&self, conn: &RustConnection) -> Result<(i32, i32)> {
        let reply = conn
            .query_pointer(self.root)
            .context("query_pointer request")?
            .reply()
            .context("query_pointer reply")?;
        Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
    }

    fn motion(&self, conn: &RustConnection, x: i32, y: i32) -> Result<()> {
        conn.xtest_fake_input(
            MOTION_NOTIFY,
            0,
            CURRENT_TIME,
            self.root,
            x as i16,
            y as i16,
            0,
        )
        .context("XTest motion")?;
        Ok(())
    }

    fn button(&self, conn: &RustConnection, press: bool, btn: u8) -> Result<()> {
        let ev = if press { BUTTON_PRESS } else { BUTTON_RELEASE };
        conn.xtest_fake_input(ev, btn, CURRENT_TIME, self.root, 0, 0, 0)
            .context("XTest button")?;
        conn.flush().context("XTest flush")?;
        Ok(())
    }

    /// Emit a trajectory with slow-fast-slow easing applied by ARC LENGTH
    /// (the audit fix: the legacy eased by point index, distorting velocity on
    /// non-uniform polylines). Ends exactly on the final point.
    fn emit_trajectory(
        &self,
        conn: &RustConnection,
        pts: &[Pt],
        rng: &mut XorShift64,
    ) -> Result<()> {
        if pts.len() < 2 {
            return Ok(());
        }
        let total = polyline_len(pts).max(1.0);
        // Per-step pause scales inversely with distance (ported timing model).
        let base = (1.2 / total).clamp(0.003, 0.008);
        let n = pts.len();
        for i in 0..n {
            let t = i as f64 / (n - 1) as f64;
            let e = smoothstep(t);
            let (mut x, mut y) = point_at_distance(pts, e * total);
            if i + 1 < n {
                // Light perpendicular jitter on travel points only.
                x += rng.gauss() * 0.6;
                y += rng.gauss() * 0.6;
            }
            self.motion(conn, x.round() as i32, y.round() as i32)?;
            if i % 6 == 5 {
                conn.flush().context("XTest flush")?;
            }
            thread::sleep(Duration::from_secs_f64(base * rng.uniform(0.7, 1.3)));
        }
        conn.flush().context("XTest flush")?;
        Ok(())
    }

    /// Move the visible cursor to (x, y) along a human-like path.
    /// Synchronous blocking X11 I/O — call via `spawn_blocking`.
    #[allow(dead_code)] // parked: visible-move for a future visible-drag path (audit LOW-1)
    pub fn move_to(&self, x: i32, y: i32) -> Result<()> {
        self.check(x, y)?;
        let mut rng = self.rng();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("X11 mutex poisoned: {e}"))?;
        let (cx, cy) = self.query_pos(&conn)?;
        let pts = build_trajectory(
            f64::from(cx),
            f64::from(cy),
            f64::from(x),
            f64::from(y),
            &mut rng,
        );
        self.emit_trajectory(&conn, &pts, &mut rng)
    }

    /// Human click at (x, y): move, hover, press, dwell, release.
    pub fn click(&self, x: i32, y: i32) -> Result<()> {
        self.check(x, y)?;
        let mut rng = self.rng();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("X11 mutex poisoned: {e}"))?;
        let (cx, cy) = self.query_pos(&conn)?;
        let pts = build_trajectory(
            f64::from(cx),
            f64::from(cy),
            f64::from(x),
            f64::from(y),
            &mut rng,
        );
        self.check_trajectory(&pts)?;
        self.emit_trajectory(&conn, &pts, &mut rng)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.05, 0.14))); // hover
        self.button(&conn, true, 1)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.03, 0.08))); // dwell
        self.button(&conn, false, 1)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.03, 0.09)));
        Ok(())
    }

    /// Human double-click: single trajectory, then two presses spaced
    /// 45–75ms apart (inside the OS double-click threshold; the legacy
    /// 230–380ms total exceeded it and never registered as a double-click).
    #[allow(dead_code)] // parked: visible double-click for a future visible path (audit LOW-1)
    pub fn double_click(&self, x: i32, y: i32) -> Result<()> {
        self.check(x, y)?;
        let mut rng = self.rng();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("X11 mutex poisoned: {e}"))?;
        let (cx, cy) = self.query_pos(&conn)?;
        let pts = build_trajectory(
            f64::from(cx),
            f64::from(cy),
            f64::from(x),
            f64::from(y),
            &mut rng,
        );
        self.check_trajectory(&pts)?;
        self.emit_trajectory(&conn, &pts, &mut rng)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.05, 0.12)));
        self.button(&conn, true, 1)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.03, 0.06)));
        self.button(&conn, false, 1)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.045, 0.075))); // inter-click
        self.button(&conn, true, 1)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.03, 0.06)));
        self.button(&conn, false, 1)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.05, 0.10)));
        Ok(())
    }

    /// Human drag from (x1,y1) to (x2,y2): move, press, trace with hand
    /// wobble, release. Wobble is dropped per-point if it would leave the
    /// window rect (endpoints are strictly validated).
    #[allow(dead_code)] // parked: visible drag for a future visible path (audit LOW-1)
    pub fn drag(&self, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        self.check(x1, y1)?;
        self.check(x2, y2)?;
        let mut rng = self.rng();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("X11 mutex poisoned: {e}"))?;
        // Move to start.
        let (cx, cy) = self.query_pos(&conn)?;
        let to_start = build_trajectory(
            f64::from(cx),
            f64::from(cy),
            f64::from(x1),
            f64::from(y1),
            &mut rng,
        );
        self.check_trajectory(&to_start)?;
        self.emit_trajectory(&conn, &to_start, &mut rng)?;

        thread::sleep(Duration::from_secs_f64(rng.uniform(0.05, 0.14)));
        self.button(&conn, true, 1)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.04, 0.09)));

        // Trace the stroke with sinusoidal hand wobble.
        let stroke = build_trajectory(
            f64::from(x1),
            f64::from(y1),
            f64::from(x2),
            f64::from(y2),
            &mut rng,
        );
        self.check_trajectory(&stroke)?;
        let total = polyline_len(&stroke).max(1.0);
        let wob_phase = rng.uniform(0.0, std::f64::consts::TAU);
        let seg_ang = (f64::from(y2) - f64::from(y1)).atan2(f64::from(x2) - f64::from(x1))
            + std::f64::consts::FRAC_PI_2;
        let (wx, wy) = (seg_ang.cos(), seg_ang.sin());
        // Draw at ~750 px/s like the legacy driver.
        let step_time = (RESAMPLE_SPACING_PX / 750.0).max(0.002);
        for (i, &p) in stroke.iter().enumerate() {
            let t = i as f64 / (stroke.len() - 1).max(1) as f64;
            let wob = (wob_phase + t * std::f64::consts::TAU).sin() * 0.8;
            let mut bx = p.0 + wx * wob + rng.gauss() * 0.25;
            let mut by = p.1 + wy * wob + rng.gauss() * 0.25;
            // Keep the stroke inside the window: drop wobble per-point if needed.
            if check_in_window(self.window_rect.as_ref(), bx as i32, by as i32).is_err() {
                bx = p.0;
                by = p.1;
            }
            self.motion(&conn, bx.round() as i32, by.round() as i32)?;
            conn.flush().context("XTest flush")?; // every move: X coalesces otherwise
            let _ = total; // (kept for clarity of the timing model)
            thread::sleep(Duration::from_secs_f64(step_time * rng.uniform(0.8, 1.25)));
        }

        thread::sleep(Duration::from_secs_f64(rng.uniform(0.05, 0.12)));
        self.button(&conn, false, 1)?;
        thread::sleep(Duration::from_secs_f64(rng.uniform(0.10, 0.20)));
        Ok(())
    }

    /// Objective curve quality metric.
    ///
    /// Returns `(max_turning_angle_deg, max_deviation_px)`:
    /// - turning angle: max direction change between consecutive segments;
    ///   smooth curves stay under ~8°, polygonal output spikes past 20°.
    /// - deviation: max perpendicular distance of interior points from the
    ///   straight chord start→end (the metric the legacy left as a `0.0` stub).
    #[allow(dead_code)] // parked: trajectory-quality metric for a future visible path (audit LOW-1)
    pub fn smoothness_score(points: &[(f64, f64)]) -> (f64, f64) {
        if points.len() < 3 {
            return (0.0, 0.0);
        }
        let mut max_turn: f64 = 0.0;
        for w in points.windows(3) {
            let v1 = (w[1].0 - w[0].0, w[1].1 - w[0].1);
            let v2 = (w[2].0 - w[1].0, w[2].1 - w[1].1);
            let l1 = v1.0.hypot(v1.1);
            let l2 = v2.0.hypot(v2.1);
            if l1 < 1e-9 || l2 < 1e-9 {
                continue;
            }
            let cos_a = ((v1.0 * v2.0 + v1.1 * v2.1) / (l1 * l2)).clamp(-1.0, 1.0);
            max_turn = max_turn.max(cos_a.acos().to_degrees());
        }
        let p0 = points[0];
        let pn = points[points.len() - 1];
        let mut max_dev: f64 = 0.0;
        for &p in &points[1..points.len() - 1] {
            max_dev = max_dev.max(dist_point_to_segment(p, p0, pn));
        }
        (max_turn, max_dev)
    }
}

// ---------------------------------------------------------------------------
// Unit tests (headless: pure geometry + validation only, no X11).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> WindowRect {
        WindowRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        }
    }

    #[test]
    fn clamp_rejects_outside_window() {
        let r = rect();
        // Inside: corners and center pass.
        assert!(check_in_window(Some(&r), 0, 0).is_ok());
        assert!(check_in_window(Some(&r), 1919, 1079).is_ok());
        assert!(check_in_window(Some(&r), 960, 540).is_ok());
        // Outside: each edge fails.
        assert!(check_in_window(Some(&r), -1, 540).is_err());
        assert!(check_in_window(Some(&r), 1920, 540).is_err());
        assert!(check_in_window(Some(&r), 960, -1).is_err());
        assert!(check_in_window(Some(&r), 960, 1080).is_err());
        assert!(check_in_window(Some(&r), 5000, 5000).is_err());
        // No rect at all: fail closed.
        assert!(check_in_window(None, 960, 540).is_err());
    }

    #[test]
    fn chord_length_capped() {
        // Long diagonal: the audit's teleport case.
        let mut rng = XorShift64::seeded();
        for _ in 0..5 {
            let pts = build_trajectory(0.0, 0.0, 1500.0, 900.0, &mut rng);
            assert!(pts.len() >= 2);
            for w in pts.windows(2) {
                let c = dist(w[0], w[1]);
                assert!(
                    c <= MAX_CHORD_PX + 1.0,
                    "chord {c:.2}px exceeds cap {MAX_CHORD_PX}"
                );
            }
            // Ends exactly on target.
            let last = pts[pts.len() - 1];
            assert!((last.0 - 1500.0).abs() < 1e-6);
            assert!((last.1 - 900.0).abs() < 1e-6);
        }
    }

    #[test]
    fn arc_length_uniform() {
        // Resampled segments should have low variance: uniform spacing.
        let mut rng = XorShift64::seeded();
        let pts = build_trajectory(100.0, 100.0, 900.0, 700.0, &mut rng);
        let segs: Vec<f64> = pts.windows(2).map(|w| dist(w[0], w[1])).collect();
        assert!(!segs.is_empty());
        let mean = segs.iter().sum::<f64>() / segs.len() as f64;
        let var = segs.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / segs.len() as f64;
        let std = var.sqrt();
        // Coefficient of variation under 60%: roughly uniform (overshoot
        // legs and the final point add legitimate variance).
        assert!(
            std / mean < 0.6,
            "spacing not uniform: mean={mean:.2} std={std:.2}"
        );
        // And no degenerate zero-length pileups dominating the path.
        let tiny = segs.iter().filter(|s| **s < 0.5).count();
        assert!(
            tiny * 4 < segs.len(),
            "too many near-zero segments: {tiny}/{}",
            segs.len()
        );
    }

    #[test]
    fn short_moves_skip_bezier() {
        let mut rng = XorShift64::seeded();
        let pts = build_trajectory(100.0, 100.0, 105.0, 103.0, &mut rng);
        assert_eq!(pts.len(), 2, "sub-15px move must be a single linear step");
    }

    #[test]
    fn smoothness_score_sanity() {
        // Straight line: no turning, no deviation.
        let line: Vec<Pt> = (0..=10).map(|i| (f64::from(i) * 10.0, 0.0)).collect();
        let (turn, dev) = X11Mouse::smoothness_score(&line);
        assert!(turn < 1e-6, "straight line turn={turn}");
        assert!(dev < 1e-6, "straight line dev={dev}");

        // Right angle: ~90° turn, deviation = dist from corner to chord.
        let corner = vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
        let (turn, dev) = X11Mouse::smoothness_score(&corner);
        assert!((turn - 90.0).abs() < 1e-6, "corner turn={turn}");
        // Corner (100,0) is 100/sqrt(2) ≈ 70.7px from the chord (0,0)->(100,100).
        assert!((dev - 70.71).abs() < 0.1, "corner dev={dev}");

        // Gentle arc: small turn, positive deviation (the previously-stubbed metric).
        let mut rng = XorShift64::seeded();
        let pts = build_trajectory(0.0, 0.0, 400.0, 0.0, &mut rng);
        let (turn, dev) = X11Mouse::smoothness_score(&pts);
        assert!(turn < 20.0, "bezier should be smooth, turn={turn}");
        assert!(dev > 0.0, "bezier must deviate from chord, dev={dev}");

        // Degenerate inputs.
        assert_eq!(X11Mouse::smoothness_score(&[]), (0.0, 0.0));
        assert_eq!(X11Mouse::smoothness_score(&[(1.0, 2.0)]), (0.0, 0.0));
    }

    #[test]
    fn point_at_distance_eased_monotonic() {
        // Easing by arc length must be monotonic in distance.
        let pts = vec![(0.0, 0.0), (100.0, 0.0), (200.0, 0.0)];
        let total = polyline_len(&pts);
        let mut prev = -1.0;
        for i in 0..=20 {
            let t = f64::from(i) / 20.0;
            let p = point_at_distance(&pts, smoothstep(t) * total);
            assert!(p.0 >= prev - 1e-9, "eased distance went backwards");
            prev = p.0;
        }
        let end = point_at_distance(&pts, total);
        assert!((end.0 - 200.0).abs() < 1e-6);
    }
}
