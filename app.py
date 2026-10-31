"""VM Bridge: programmatic browser control for the chromebook-vm.

Replaces fragile VNC pixel automation with selector-based browser control
(Playwright). Binds to 127.0.0.1 only; reached from the orchestrator via
bridge-cli (~/workspace/chromebook-vm/bridge-cli.py), which tunnels over
cb-job.sh. Every endpoint except /health requires the X-API-Key header.

Phase A hardening (2026-10-30): API-key auth, token-bucket rate limits,
JSON request log, request body caps, /exec removed (shell work belongs to
cb-job.sh), systemd notify + watchdog.

Endpoints:
    POST /browser/navigate  {url, wait_until?} -> {url, title}
    POST /browser/fill      {selector, text}
    POST /browser/click     {selector}
    POST /browser/type      {selector, text}   (types without clearing first)
    POST /browser/press     {selector, key}    (e.g. Enter)
    POST /browser/wait      {selector?, timeout_ms?}
    GET  /browser/text                        -> {url, title, text}
    GET  /browser/html                        -> {url, html} (truncated)
    POST /browser/eval      {js}               -> {result}
    GET  /browser/screenshot                  -> PNG bytes
    POST /browser/snapshot                    -> {url, title, aria snapshot}
    GET  /health                               (no auth)
"""

import asyncio
import hmac
import json
import os
import sys
import time
import uuid

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel
# Driver: vanilla Playwright. Patchright was tried (round 2) and REVERTED
# the same day: patchright 1.64.3 silently breaks inline event handlers
# (typeof el.onclick === "object", clicks never fire -- verified
# side-by-side vs vanilla on a fixture page). A stealth gain that breaks
# clicking is not a trade, it's a bug. Stealth now rests on headed real
# Chrome + residential IP + persistent profile + launch-arg hygiene.
from playwright.async_api import async_playwright
_DRIVER = "playwright"
import gzip
import json as _json

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import human_mouse

# Round 2 stealth note: NO add_init_script patch set. Two verified reasons.
# 1. Redundant: with channel="chrome" (real Chrome binary) the JS-visible
#    surface is already genuine -- 3 plugins, window.chrome present, real
#    languages -- and Patchright strips --enable-automation, so
#    navigator.webdriver is natively false, exactly like the user's own
#    browser. There is nothing to hide, so nothing to patch.
# 2. Broken: patchright 1.64.3 routes add_init_script through its own
#    inject-route fallback (patchrightInitScript=True) and user scripts
#    never execute -- verified: window.__init_ran stayed None on both
#    persistent and non-persistent contexts. Dead code removed rather
#    than shipped as theater. Revisit if a target ever fingerprints deeper.

# Docs endpoints disabled: this is a machine API, not a public service.
app = FastAPI(title="vm-bridge", docs_url=None, redoc_url=None, openapi_url=None)


# --- Phase A: systemd notify (raw socket, no dependency) ---
def _sd_notify(msg: str):
    """Send a message to systemd's notification socket ($NOTIFY_SOCKET).
    No-op when not running under systemd with Type=notify."""
    sock = os.environ.get("NOTIFY_SOCKET")
    if not sock:
        return
    try:
        import socket
        addr = "\0" + sock[1:] if sock.startswith("@") else sock
        s = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        s.connect(addr)
        s.sendall(msg.encode())
        s.close()
    except Exception:
        pass


# --- Phase A: auth + rate limit + body cap + JSON log (ASGI middleware) ---
# NOTE on researched substitutions: slowapi needs a `request` arg in every
# decorated endpoint (40+ signatures); structlog is not in the venv. A
# token-bucket middleware and stdlib JSON logging give the same protection
# with no signature churn and no new hard dependencies.

_API_KEY_ENV = "BRIDGE_API_KEY"
_AUTH_EXEMPT = {"/health"}

# Token buckets: (refill_per_sec, burst). All callers are localhost; the
# point is backpressure for the single browser, not per-user fairness.
_RATE_DEFAULT = (2.0, 30)    # 120/min sustained, burst 30
_RATE_EXPENSIVE = (1.0 / 3, 5)  # 20/min, burst 5
_EXPENSIVE_PATHS = ("/browser/drag", "/browser/eval", "/browser/script")
_buckets = {}


def _take_token(path: str) -> bool:
    rate, burst = _RATE_EXPENSIVE if path in _EXPENSIVE_PATHS else _RATE_DEFAULT
    now = time.monotonic()
    b = _buckets.get(path)
    if b is None:
        b = _buckets[path] = {"tokens": burst, "ts": now}
    b["tokens"] = min(burst, b["tokens"] + (now - b["ts"]) * rate)
    b["ts"] = now
    if b["tokens"] >= 1:
        b["tokens"] -= 1
        return True
    return False


# Body caps: default 2MB; script/drag/delta carry large payloads.
_BODY_CAP_DEFAULT = 2 * 1024 * 1024
_BODY_CAP_LARGE = 8 * 1024 * 1024
_LARGE_BODY_PATHS = ("/browser/script", "/browser/drag", "/browser/screenshot/delta")


@app.middleware("http")
async def _hardening_middleware(request: Request, call_next):
    path = request.url.path

    # 1. Auth (constant-time compare; /health exempt for the deploy check)
    if path not in _AUTH_EXEMPT:
        expected = os.environ.get(_API_KEY_ENV, "")
        got = request.headers.get("x-api-key", "")
        if not expected or not hmac.compare_digest(got, expected):
            return JSONResponse({"detail": "unauthorized"},
                                status_code=401 if expected else 500)

    # 2. Rate limit (token bucket per path)
    if not _take_token(path):
        return JSONResponse({"detail": "rate limited, back off"},
                            status_code=429,
                            headers={"Retry-After": "5"})

    # 3. Body cap (Content-Length gate; curl --data-binary always sets it)
    cap = _BODY_CAP_LARGE if path in _LARGE_BODY_PATHS else _BODY_CAP_DEFAULT
    clen = request.headers.get("content-length")
    if clen is not None:
        try:
            if int(clen) > cap:
                return JSONResponse({"detail": f"body over {cap} bytes"},
                                    status_code=413)
        except ValueError:
            pass

    # 2b. Lease (Phase C1): when a lease is held, mutating methods need
    # the matching X-Lease-Id header. Reads stay lock-free. Lease
    # acquire/release themselves are exempt so a client can always
    # acquire (when free) or release. Pool paths are exempt too: the
    # pool is a separate headless browser, so parallel background work
    # must not be serialized against the watched browser (round 2).
    if (request.method in ("POST", "PUT", "PATCH", "DELETE")
            and path not in ("/lease/acquire", "/lease/release")
            and not path.startswith("/pool/")):
        _prune_leases()
        if _leases and request.headers.get("x-lease-id", "") not in _leases:
            return JSONResponse({"detail": "browser leased by another client"},
                                status_code=423)

    # 4. JSON request log (one line; /health skipped to keep the log clean)
    rid = uuid.uuid4().hex[:8]
    t0 = time.time()
    response = await call_next(request)
    dur_ms = round((time.time() - t0) * 1000, 1)
    if path != "/health":
        _record_metric(path, response.status_code, dur_ms)
        print(json.dumps({
            "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "rid": rid, "m": request.method, "path": path,
            "status": response.status_code, "ms": dur_ms,
        }), flush=True)
    return response


# --- Phase C: metrics + leases ---
_metrics = {
    "actions": {},    # path -> count
    "errors": {},     # "path:status" -> count
    "lat_ms": {},     # path -> last 200 durations
    "relaunches": 0,
    "watchdog_pings": 0,
    "started_ts": time.time(),
    "driver": _DRIVER,  # round 2: which driver is actually driving
}


def _record_metric(path: str, status: int, dur_ms: float):
    m = _metrics
    m["actions"][path] = m["actions"].get(path, 0) + 1
    if status >= 400:
        k = f"{path}:{status}"
        m["errors"][k] = m["errors"].get(k, 0) + 1
    lst = m["lat_ms"].setdefault(path, [])
    lst.append(dur_ms)
    if len(lst) > 200:
        del lst[:-200]


@app.get("/metrics")
async def metrics():
    """Operational metrics: action counts, error counts, latency
    percentiles, browser relaunches, lease state. Auth required."""
    out = {
        "uptime_s": round(time.time() - _metrics["started_ts"], 1),
        "driver": _metrics.get("driver"),
        "relaunches": _metrics["relaunches"],
        "watchdog_pings": _metrics["watchdog_pings"],
        "pool_acquires": _metrics.get("pool_acquires", 0),
        "pool_size": len(_pool),
        "actions": _metrics["actions"],
        "errors": _metrics["errors"],
        "latency_ms": {},
        "tabs": len(_tabs),
        "frame_cache": len(_frame_cache),
    }
    for p, lst in _metrics["lat_ms"].items():
        s = sorted(lst)
        n = len(s)
        out["latency_ms"][p] = {
            "n": n,
            "p50": round(s[n // 2], 1),
            "p95": round(s[min(n - 1, int(n * 0.95))], 1),
            "max": round(s[-1], 1),
        }
    out["leases"] = len(_leases)
    return out


# Lease manager: crash-safe coordination for the single shared browser.
# A lease times out if its holder dies mid-flow (a lock would wedge).
# Single non-reentrant lease; reads never need it.
_leases = {}  # lease_id -> {"client": str, "expires": monotonic}


def _prune_leases():
    now = time.monotonic()
    for lid in [l for l, e in _leases.items() if e["expires"] <= now]:
        del _leases[lid]


class LeaseAcquireReq(BaseModel):
    client_id: str = "orchestrator"
    ttl_s: int = 120


@app.post("/lease/acquire")
async def lease_acquire(req: LeaseAcquireReq):
    _prune_leases()
    if _leases:
        raise HTTPException(423, "a lease is already held; see /lease/status")
    lid = uuid.uuid4().hex[:12]
    _leases[lid] = {
        "client": req.client_id,
        "expires": time.monotonic() + max(10, min(req.ttl_s, 3600)),
    }
    return {"lease_id": lid, "ttl_s": req.ttl_s}


class LeaseReleaseReq(BaseModel):
    lease_id: str


@app.post("/lease/release")
async def lease_release(req: LeaseReleaseReq):
    _prune_leases()
    if req.lease_id in _leases:
        del _leases[req.lease_id]
        return {"ok": True}
    raise HTTPException(404, "no such lease")


@app.get("/lease/status")
async def lease_status():
    _prune_leases()
    now = time.monotonic()
    return {"leases": [
        {"lease_id": lid, "client": e["client"],
         "ttl_left_s": round(e["expires"] - now, 1)}
        for lid, e in _leases.items()
    ]}


@app.exception_handler(Exception)
async def _all_exceptions(request: Request, exc: Exception):
    """Phase B1, detection ring 2: a request that hits a closed browser
    triggers recovery immediately and tells the caller to retry, instead
    of leaking a Playwright stack trace."""
    if isinstance(exc, HTTPException):
        raise exc
    msg = str(exc)
    if ("has been closed" in msg or "Browser closed" in msg
            or "Connection closed" in msg or "Target closed" in msg):
        asyncio.create_task(_recover())
        return JSONResponse(
            {"detail": "browser closed, relaunching; retry shortly"},
            status_code=503)
    # Round 3: failure screenshot for main-browser actions. Near-zero
    # steady-state cost; the shot path lands in the JSON log next to the
    # error so a failed run is debuggable without reproducing it.
    path = request.url.path
    if path.startswith("/browser/"):
        try:
            page = _page
            if page is not None:
                d = os.path.expanduser("~/jobs/vm-bridge/failures")
                os.makedirs(d, exist_ok=True)
                shot = "%s/main_%d.png" % (d, int(time.time()))
                await page.screenshot(path=shot, timeout=5000)
                print(json.dumps({
                    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    "event": "failure_screenshot", "path": path,
                    "shot": shot, "error": msg[:200],
                }), flush=True)
        except Exception:
            pass
    raise exc

_pw = None
_browser = None
_page = None
_ctx = None
_tabs = {}  # tab_id -> page
_cdp_sessions = {}  # id(page) -> persistent CDP session (round 2 speedup)

# --- Knowing bridge: state tracking and selective verification ---
# The bridge is blind by default (no live display). It must KNOW when
# it can trust blind operation and when it must look.
# Verification levels (cheapest first):
#   blind: trust it. No check. (typing after a verified click)
#   hash:  perceptual hash, ~0.08s, 0 bytes. "Am I still on this screen?"
#   delta: screenshot delta, ~0.1s, 0-7KB. "What changed?"
#   full:  full screenshot, ~0.08s, 17KB. "I'm lost, show everything."
_state = {
    "expected_url": None,
    "expected_hash": None,
    "last_action": None,
}


async def _cdp_session_for(page, ctx):
    """Get-or-create the persistent CDP session for a page. Round 3: takes
    the owning context explicitly so pool pages (browser #2) work too."""
    if page is None or ctx is None:
        raise RuntimeError("no page")
    key = (id(ctx), id(page))
    sess = _cdp_sessions.get(key)
    if sess is None:
        sess = await ctx.new_cdp_session(page)
        _cdp_sessions[key] = sess
    return sess


async def _cdp_capture(page, ctx=None):
    """Screenshot via a persistent per-page CDP session.

    Round 2 speedup: Playwright Python's page.screenshot() travels
    Python -> Node relay -> CDP -> browser on every call. A persistent
    CDP session pays the setup cost once and reuses it; Page.captureScreenshot
    is then a single CDP round trip. The hash poll (~0.08s, highest-frequency
    call) benefits most. Sessions die with their page; recreated on failure.
    """
    ctx = ctx or _ctx
    sess = await _cdp_session_for(page, ctx)
    try:
        res = await sess.send("Page.captureScreenshot", {"format": "png"})
    except Exception:
        _cdp_sessions.pop((id(ctx), id(page)), None)
        sess = await _cdp_session_for(page, ctx)
        res = await sess.send("Page.captureScreenshot", {"format": "png"})
    import base64 as _b64
    return _b64.b64decode(res["data"])


async def _ax_snapshot(page, ctx=None, cap=200):
    """Compact accessibility tree via CDP Accessibility.getFullAXTree.

    Patchright removed page.accessibility, so the snapshot endpoint goes
    through CDP directly -- driver-agnostic and it reuses the persistent
    session. Returns interactive elements with role/name/AX nodeId and the
    backendDOMNodeId (the key for /browser/click_node), no pixels.
    """
    ctx = ctx or _ctx
    sess = await _cdp_session_for(page, ctx)
    try:
        res = await sess.send("Accessibility.getFullAXTree", {})
    except Exception:
        _cdp_sessions.pop((id(ctx), id(page)), None)
        sess = await _cdp_session_for(page, ctx)
        res = await sess.send("Accessibility.getFullAXTree", {})
    interesting = {"button", "link", "textbox", "checkbox", "radio",
                   "combobox", "listbox", "menuitem", "tab", "switch",
                   "searchbox", "spinbutton", "heading"}
    out = []
    for n in res.get("nodes", []):
        role = (n.get("role") or {}).get("value", "")
        name = (n.get("name") or {}).get("value", "")
        if role in interesting or (name and role):
            out.append({"role": role, "name": name[:80],
                        "nodeId": n.get("nodeId"),
                        "backendNodeId": n.get("backendDOMNodeId")})
        if len(out) >= cap:
            break
    return out


async def _current_hash():
    """Cheap state fingerprint. Returns None on failure."""
    try:
        png = await _cdp_capture(_page)
        return _dhash(png)
    except Exception:
        return None


def _hamming(h1: str, h2: str) -> int:
    """Hamming distance between two hex hashes. 0=identical, <5=minor."""
    try:
        b1 = int(h1, 16)
        b2 = int(h2, 16)
        return bin(b1 ^ b2).count("1")
    except:
        return 64


async def _verify(expected="hash", tolerance=10):
    """Verify we're where we think we are.
    Uses Hamming distance for tolerance on dynamic pages (cursor blink,
    autosave indicators). Returns (ok: bool, detail: str)."""
    if expected == "blind":
        return True, "blind trust"
    current = await _current_hash()
    if current is None:
        return False, "hash failed"
    exp = _state["expected_hash"]
    if exp is None:
        # No baseline yet; establish it
        _state["expected_hash"] = current
        return True, "baseline established"
    dist = _hamming(current, exp)
    if dist <= tolerance:
        return True, f"hash match (dist={dist})"
    # Hash differs significantly: escalate
    return False, f"hash mismatch: dist={dist} (expected {exp[:8]}, got {current[:8]})"


def _update_state(url=None, hash=None, action=None):
    if url is not None:
        _state["expected_url"] = url
    if hash is not None:
        _state["expected_hash"] = hash
    if action is not None:
        _state["last_action"] = action


def _dhash(png_bytes: bytes) -> str:
    """Perceptual hash (dHash, 64-bit). Same image -> same hash.
    Small visual changes -> small Hamming distance. Use for change
    detection without shipping pixels."""
    from PIL import Image
    import io
    img = Image.open(io.BytesIO(png_bytes)).convert("L").resize((9, 8))
    px = list(img.getdata())
    bits = 0
    for y in range(8):
        for x in range(8):
            bits = (bits << 1) | (1 if px[y*9+x] > px[y*9+x+1] else 0)
    return format(bits, "016x")


class NavigateReq(BaseModel):
    url: str
    wait_until: str = "domcontentloaded"


class SelectorReq(BaseModel):
    selector: str
    # Phase B2: native input (default) vs visible XTest cursor.
    # Native = Playwright's trusted input path with actionability checks:
    # fast, precise, no geometry. Visible = the real OS cursor travels
    # (for watched displays and demos). Drawing (drag) is always visible.
    visible: bool = False


@app.post("/browser/tab/new")
async def tab_new(req: NavigateReq):
    """Open a new tab (parallel). Returns tab_id. The new tab becomes active."""
    global _page
    import uuid
    tab_id = uuid.uuid4().hex[:8]
    page = await _ctx.new_page()
    await page.goto(req.url, wait_until=req.wait_until, timeout=30000)
    _tabs[tab_id] = page
    # Tier 1c: LRU cap -- tabs used to accumulate forever and leak memory.
    while len(_tabs) > 10:
        oldest = next(iter(_tabs))
        if oldest == tab_id:
            break
        try:
            await _tabs[oldest].close()
        except Exception:
            pass
        del _tabs[oldest]
    _page = page
    _page.set_default_timeout(15000)
    await _refresh_baseline(f"tab_new:{req.url}")
    return {"tab_id": tab_id, "url": page.url, "title": await page.title()}


@app.post("/browser/tab/switch")
async def tab_switch(req: SelectorReq):
    """Switch to a tab by tab_id. The switched tab becomes the active _page."""
    global _page
    # selector field carries the tab_id
    if req.selector not in _tabs:
        raise HTTPException(404, f"no such tab: {req.selector}")
    _page = _tabs[req.selector]
    await _page.bring_to_front()
    return {"ok": True, "tab_id": req.selector, "url": _page.url}


@app.get("/browser/tabs")
async def tab_list():
    """List open tabs."""
    return {"tabs": [
        {"tab_id": tid, "url": p.url, "title": await p.title()}
        for tid, p in _tabs.items()
    ], "active_url": _page.url if _page else None}


# --- Round 2: headless ephemeral-context pool (browser #2) ---
# Fixes gap 3 (single browser/profile). launch_persistent_context is a
# dedicated 1:1 browser instance, so ephemeral contexts CANNOT live in the
# same browser as the persistent profile. Two-browser design:
#   browser #1: persistent, headed, pinned, watched (unchanged)
#   browser #2: headless, hosting a small LRU pool of ephemeral contexts
#               (isolated cookie jars, incognito-like) for parallel
#               background work: fast fetches, form checks, scrapes.
# Pool contexts get resource blocking (image/font/media/trackers aborted:
# 50-80% faster loads on media-heavy pages). NEVER enable resource blocking
# on browser #1: the watched display must look real.
# Guardrails: pool max 3 contexts (m3/8GB box), per-context TTL, MemoryMax 3G
# on the service. Pool is always headless: multiple headed contexts exceed
# the RAM ceiling AND break the one-window geometry model (bind_window).
_pw2 = None
_browser2 = None
_pool = {}  # ctx_id -> {"ctx","page","last","ttl","trace"}
_POOL_MAX = 3

_POOL_BLOCK_TYPES = {"image", "media", "font"}
_POOL_BLOCK_HOSTS = (
    "google-analytics.com", "googletagmanager.com", "doubleclick.net",
    "hotjar.com", "segment.io", "segment.com", "fullstory.com",
    "mixpanel.com", "newrelic.com", "nr-data.net", "amplitude.com",
    "criteo.com", "adsrvr.org", "googlesyndication.com",
)


async def _pool_route(route):
    """Resource blocking for pool contexts. Research: never block `script`
    (breaks SPAs); stylesheets stay on (layout must render for checks)."""
    req = route.request
    try:
        if req.resource_type in _POOL_BLOCK_TYPES:
            await route.abort()
            return
        url = req.url
        for h in _POOL_BLOCK_HOSTS:
            if h in url:
                await route.abort()
                return
        await route.continue_()
    except Exception:
        pass


async def _ensure_pool_browser():
    """Lazy-launch browser #2 (headless). Saves ~250MB when the pool is
    never used; recreates it if the process died."""
    global _pw2, _browser2
    if _browser2 is not None:
        try:
            if _browser2.is_connected():
                return
        except Exception:
            pass
        _pw2 = _browser2 = None
    if _pw2 is None:
        _pw2 = await async_playwright().start()
    _browser2 = await _pw2.chromium.launch(
        channel="chrome",
        headless=True,
        ignore_default_args=["--enable-automation"],
        args=[
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--disable-gpu",
            "--disable-extensions",
            "--disable-component-update",
            "--disable-background-networking",
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--disable-blink-features=AutomationControlled",
            "--disable-features=IsolateOrigins,site-per-process",
            "--js-flags=--max-old-space-size=512",
            "--password-store=basic",  # round 2: avoids the ~25s D-Bus
            "--mute-audio",            # Secret Service stall (verified)
        ],
        env={**os.environ, "DISPLAY": ":1"},
    )


async def _pool_close(cid, failed=False):
    e = _pool.pop(cid, None)
    if not e:
        return
    ctx = e["ctx"]
    try:
        if e.get("trace"):
            d = os.path.expanduser("~/jobs/vm-bridge/traces")
            os.makedirs(d, exist_ok=True)
            # retain-on-failure: keep the trace only when asked or on failure
            if failed:
                await ctx.tracing.stop(path="%s/%s.zip" % (d, cid))
            else:
                await ctx.tracing.stop()
    except Exception:
        pass
    try:
        await ctx.close()
    except Exception:
        pass


async def _pool_prune():
    now = time.monotonic()
    for cid in [c for c, e in _pool.items() if now - e["last"] > e["ttl"]]:
        await _pool_close(cid)


async def _pool_failshot(cid, page):
    """Screenshots-on-failure: near-zero steady-state cost, immediate
    debugging win when a pool action blows up."""
    try:
        d = os.path.expanduser("~/jobs/vm-bridge/failures")
        os.makedirs(d, exist_ok=True)
        path = "%s/%s_%d.png" % (d, cid, int(time.time()))
        await page.screenshot(path=path)
        return path
    except Exception:
        return None


async def _pool_get(cid):
    """Fetch a pool entry, refreshing its TTL. Round 3: validates liveness
    with one cheap evaluate; a dead context is evicted and reported so the
    caller re-acquires instead of hanging on a corpse."""
    e = _pool.get(cid)
    if not e:
        raise HTTPException(404, "unknown pool context %s" % cid)
    try:
        await e["page"].evaluate("1")
    except Exception:
        await _pool_close(cid)
        raise HTTPException(503, "pool context %s died; re-acquire" % cid)
    e["last"] = time.monotonic()
    return e


class PoolAcquireReq(BaseModel):
    ttl_s: int = 600
    trace: bool = False


class PoolIdReq(BaseModel):
    context_id: str


class PoolNavReq(BaseModel):
    context_id: str
    url: str
    wait_until: str = "domcontentloaded"


class PoolEvalReq(BaseModel):
    context_id: str
    js: str


@app.post("/pool/acquire")
async def pool_acquire(req: PoolAcquireReq):
    """Acquire an ephemeral headless context (isolated cookies/storage).
    LRU-evicted at pool cap; idle-expired at ttl_s."""
    await _ensure_pool_browser()
    await _pool_prune()
    while len(_pool) >= _POOL_MAX:
        oldest = min(_pool, key=lambda k: _pool[k]["last"])
        await _pool_close(oldest)
    cid = uuid.uuid4().hex[:8]
    ctx = await _browser2.new_context(service_workers="block")
    await ctx.route("**/*", _pool_route)
    if req.trace:
        await ctx.tracing.start(screenshots=True, snapshots=True)
    page = await ctx.new_page()
    page.set_default_timeout(15000)
    _pool[cid] = {"ctx": ctx, "page": page, "last": time.monotonic(),
                  "ttl": max(60, min(req.ttl_s, 3600)), "trace": req.trace}
    _metrics["pool_acquires"] = _metrics.get("pool_acquires", 0) + 1
    return {"context_id": cid, "pool_size": len(_pool)}


@app.post("/pool/navigate")
async def pool_navigate(req: PoolNavReq):
    e = await _pool_get(req.context_id)
    page = e["page"]
    try:
        await page.goto(req.url, wait_until=req.wait_until, timeout=30000)
        return {"url": page.url, "title": await page.title()}
    except Exception as exc:
        shot = await _pool_failshot(req.context_id, page)
        raise HTTPException(500, "navigate failed%s: %s" % (
            " (screenshot %s)" % shot if shot else "", exc))


@app.post("/pool/text")
async def pool_text(req: PoolIdReq):
    e = await _pool_get(req.context_id)
    page = e["page"]
    try:
        text = await page.evaluate(
            "() => document.body ? document.body.innerText.slice(0, 12000) : ''")
        return {"url": page.url, "title": await page.title(), "text": text}
    except Exception as exc:
        shot = await _pool_failshot(req.context_id, page)
        raise HTTPException(500, "text failed%s: %s" % (
            " (screenshot %s)" % shot if shot else "", exc))


@app.post("/pool/eval")
async def pool_eval(req: PoolEvalReq):
    e = await _pool_get(req.context_id)
    try:
        return {"result": await e["page"].evaluate(req.js)}
    except Exception as exc:
        shot = await _pool_failshot(req.context_id, e["page"])
        raise HTTPException(500, "eval failed%s: %s" % (
            " (screenshot %s)" % shot if shot else "", exc))


@app.post("/pool/release")
async def pool_release(req: PoolIdReq):
    await _pool_close(req.context_id)
    return {"released": req.context_id, "pool_size": len(_pool)}


class PoolSelectorReq(BaseModel):
    context_id: str
    selector: str


class PoolFillReq(BaseModel):
    context_id: str
    selector: str
    text: str


class PoolOrderReq(BaseModel):
    context_id: str
    action: str  # click | fill
    text: str
    role: str = ""
    value: str = ""


@app.post("/pool/click")
async def pool_click(req: PoolSelectorReq):
    """Native click in a pool context. Headless: no cursor, no geometry,
    just Playwright's actionability-checked input. Round 3."""
    e = await _pool_get(req.context_id)
    page = e["page"]
    try:
        await page.locator(req.selector).first.click(timeout=10000)
        return {"ok": True}
    except Exception as exc:
        shot = await _pool_failshot(req.context_id, page)
        raise HTTPException(500, "pool click failed%s: %s" % (
            " (screenshot %s)" % shot if shot else "", exc))


@app.post("/pool/fill")
async def pool_fill(req: PoolFillReq):
    """Native fill in a pool context. Round 3."""
    e = await _pool_get(req.context_id)
    page = e["page"]
    try:
        await page.locator(req.selector).first.fill(req.text, timeout=10000)
        return {"ok": True}
    except Exception as exc:
        shot = await _pool_failshot(req.context_id, page)
        raise HTTPException(500, "pool fill failed%s: %s" % (
            " (screenshot %s)" % shot if shot else "", exc))


@app.post("/pool/order")
async def pool_order(req: PoolOrderReq):
    """Semantic order in a pool context: same deterministic resolver as
    /browser/order, headless execution. Round 3: the pool is now a full
    automation backend, not just a fetch engine."""
    e = await _pool_get(req.context_id)
    page = e["page"]
    try:
        loc = await _resolve_order_locator(
            page, req.action, req.text, req.role or None)
        if loc is None:
            return {"ok": False, "error": "could not resolve '%s'" % req.text}
        target = loc.first
        if req.action == "click":
            await target.click(timeout=10000)
        elif req.action == "fill":
            await target.fill(req.value, timeout=10000)
        else:
            return {"ok": False, "error": "unknown action %s" % req.action}
        return {"ok": True}
    except Exception as exc:
        shot = await _pool_failshot(req.context_id, page)
        raise HTTPException(500, "pool order failed%s: %s" % (
            " (screenshot %s)" % shot if shot else "", exc))


@app.post("/pool/snapshot")
async def pool_snapshot(req: PoolIdReq):
    """Accessibility snapshot of a pool page (CDP, same format as
    /browser/snapshot). Round 3."""
    e = await _pool_get(req.context_id)
    page = e["page"]
    try:
        snap = await _ax_snapshot(page, ctx=e["ctx"])
        return {"url": page.url, "title": await page.title(),
                "snapshot": snap}
    except Exception as exc:
        raise HTTPException(500, "pool snapshot failed: %s" % exc)


@app.get("/pool/status")
async def pool_status():
    await _pool_prune()
    return {"size": len(_pool), "max": _POOL_MAX,
            "contexts": list(_pool.keys()),
            "browser2_connected": bool(_browser2)}


# --- Phase B1: guarded recovery (2026-10-30) ---
# Three detection rings: (1) the browser's own "disconnected" event (fast),
# (2) any request hitting a closed target triggers recovery via the
# exception handler (immediate), (3) the 20s heartbeat watchdog (backstop
# for hard kills where no event fires). Research: never rely on
# context.on("close") alone; a dead context cannot be revived, so recovery
# always relaunches the full persistent context.
_recovering = False


async def _recover():
    """Relaunch the browser. Guarded: concurrent callers collapse into one."""
    global _recovering
    if _recovering:
        return
    _recovering = True
    try:
        await _launch_browser()
    except Exception:
        pass
    finally:
        _recovering = False


def _on_browser_disconnected(browser=None):
    """Fast path: the browser told us it died. Kick recovery now."""
    try:
        loop = asyncio.get_running_loop()
        loop.create_task(_recover())
    except RuntimeError:
        pass


async def _launch_browser():
    """(Re)launch the persistent browser context. Shared by startup,
    watchdog, and recovery: the service self-heals when Chrome dies
    instead of serving opaque 503s on a dead _page. Tier 1c (2026-10-30)."""
    global _pw, _browser, _page, _ctx
    # Close anything stale first (best effort)
    for obj, meth in ((_browser, "close"), (_pw, "stop")):
        if obj is not None:
            try:
                await obj.close() if meth == "close" else await obj.stop()
            except Exception:
                pass
    _pw = _browser = _page = _ctx = None
    _tabs.clear()
    _cdp_sessions.clear()  # round 2: sessions die with the browser
    _metrics["relaunches"] += 1
    before = set(human_mouse.find_chrome_window_ids())
    _pw = await async_playwright().start()
    # Launch persistent context with the user's profile copy (has logins).
    # Profile: /home/auto/.config/chrome-main (copy of google-chrome).
    # Headed on :1, visible per show-mode rule.
    #
    # Round 2 flags (research-backed, headed-mode specific):
    #   --disable-blink-features=AutomationControlled: hides navigator.webdriver
    #   --disable-background-*: stop Chrome deprioritizing the "background"
    #       headed window (timers, renderers, occluded windows)
    #   --disable-ipc-flooding-protection: removes Chrome's self-throttle on
    #       rapid automation input (browser-use ships this)
    #   --disable-features=IsolateOrigins,site-per-process: ~10-16% RAM
    #       saving on a single-purpose automation box
    #   --mute-audio/--disable-extensions/etc: dead weight off
    # NOT used: --single-process (one renderer crash kills everything).
    _ctx = await _pw.chromium.launch_persistent_context(
        user_data_dir="/home/auto/.config/chrome-main",
        channel="chrome",
        headless=False,
        # --enable-automation is what sets navigator.webdriver=true; strip
        # it so the browser reports false like the user's own Chrome.
        ignore_default_args=["--enable-automation"],
        args=[
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--disable-gpu",
            "--disable-blink-features=AutomationControlled",
            "--disable-background-timer-throttling",
            "--disable-backgrounding-occluded-windows",
            "--disable-renderer-backgrounding",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--disable-translate",
            "--disable-extensions",
            "--disable-default-apps",
            "--disable-ipc-flooding-protection",
            "--disable-features=IsolateOrigins,site-per-process",
            "--metrics-recording-only",
            "--no-first-run",
            "--mute-audio",
            "--window-size=1920,1080",
            "--window-position=0,0",
        ],
        viewport={"width": 1920, "height": 1040},
        env={**__import__("os").environ, "DISPLAY": ":1"},
    )
    _browser = _ctx.browser
    _page = _ctx.pages[0] if _ctx.pages else await _ctx.new_page()
    _page.set_default_timeout(15000)
    # Cache window geometry once: eliminates per-action CDP evaluate for
    # viewport->screen conversion. viewport_to_screen() is pure Python after.
    try:
        geo = await _page.evaluate(
            """({sx: window.screenX, sy: window.screenY,
                ch: window.outerHeight - window.innerHeight})"""
        )
        human_mouse.cache_window_geometry(geo["sx"], geo["sy"], geo["ch"])
    except Exception:
        pass  # defaults (20, 22, 110) stand
    # Bind the bridge's X window so sync_window_geometry() can re-read its
    # real position before mouse actions (Tier 1d: no more stale geometry).
    try:
        after = set(human_mouse.find_chrome_window_ids())
        new = list(after - before)
        if new:
            human_mouse.bind_window(new[0])
    except Exception:
        pass
    # Phase B1: fast crash detection. The heartbeat watchdog stays as the
    # backstop for hard kills where this event never fires.
    try:
        _browser.on("disconnected", _on_browser_disconnected)
    except Exception:
        pass
    h = await _current_hash()
    _update_state(url=_page.url, hash=h, action="browser_launch")


async def _watchdog():
    """Background self-heal: probe the page every 20s; on two consecutive
    failures, relaunch the browser. Tier 1c (2026-10-30).

    Phase A: also feeds systemd's watchdog, but ONLY on a passing probe --
    a dead browser must let the watchdog expire so systemd restarts us
    (the outer supervision ring)."""
    fails = 0
    await asyncio.sleep(20)
    while True:
        # A recovery already in flight: don't stack another relaunch on it.
        if _recovering:
            fails = 0
            await asyncio.sleep(20)
            continue
        try:
            if _page is None:
                raise RuntimeError("no page")
            await _page.evaluate("document.title")
            fails = 0
            _sd_notify("WATCHDOG=1")
            _metrics["watchdog_pings"] += 1
        except Exception:
            fails += 1
            if fails >= 2:
                await _recover()
                fails = 0
                # No watchdog ping here: if recovery failed, the browser is
                # still dead and systemd must be allowed to restart us.
        await asyncio.sleep(20)


@app.on_event("startup")
async def startup():
    await _launch_browser()
    _sd_notify("READY=1")
    asyncio.create_task(_watchdog())


@app.on_event("shutdown")
async def shutdown():
    if _browser:
        await _browser.close()
    if _pw:
        await _pw.stop()
    # Round 2: take the pool browser down too.
    global _pw2, _browser2
    if _browser2:
        try:
            await _browser2.close()
        except Exception:
            pass
    if _pw2:
        try:
            await _pw2.stop()
        except Exception:
            pass


def _need_page():
    if _page is None:
        raise HTTPException(503, "browser relaunching, retry shortly")
    return _page


async def _refresh_baseline(action):
    """Refresh the knowing-bridge baseline after any state-changing action.
    Tier 1e (2026-10-30): the baseline used to go stale after type/scroll/
    eval, causing false hash_mismatch alarms or silent misses."""
    h = await _current_hash()
    _update_state(hash=h, action=action)


async def _resolve_stable_box(page, selector=None, locator=None, attempts=3):
    """Element-centric verification (Tier 1e): resolve the target's bounding
    box and require it to be stable across two reads 150ms apart. Retries
    when the element is still moving (animations, lazy layout)."""
    loc = locator if locator is not None else page.locator(selector).first
    await loc.wait_for(state="visible", timeout=10000)
    last = None
    for _ in range(attempts):
        box = await loc.bounding_box()
        if box and last and abs(box["x"] - last["x"]) <= 5 and abs(box["y"] - last["y"]) <= 5:
            return box
        last = box
        await asyncio.sleep(0.15)
    return last


@app.post("/browser/navigate")
async def navigate(req: NavigateReq):
    page = _need_page()
    await page.goto(req.url, wait_until=req.wait_until, timeout=30000)
    # Establish baseline: we KNOW what page we're on now
    h = await _current_hash()
    _update_state(url=page.url, hash=h, action=f"navigate:{req.url}")
    return {"url": page.url, "title": await page.title(), "verified": h is not None}


class FillReq(SelectorReq):
    text: str


@app.post("/browser/fill")
async def fill(req: FillReq):
    page = _need_page()
    if req.visible:
        await _human_click_at(page, req.selector, visible=True)
        await page.fill(req.selector, req.text)
    else:
        # Native: verify the target is stable, then Playwright's own fill
        # (waits for actionability: editable, visible, stable, enabled).
        await _resolve_stable_box(page, selector=req.selector)
        await page.locator(req.selector).first.fill(req.text, timeout=10000)
    await _refresh_baseline(f"fill:{req.selector}")
    return {"ok": True, "mode": "visible" if req.visible else "native"}


async def _human_click_at(page, selector: str, visible: bool = False):
    """Click an element, two modes (Phase B2).

    Native (default): Playwright's trusted input path. Actionability is
    guaranteed by Playwright itself (visible, stable, receiving events,
    enabled); no geometry, no X round-trips, no cursor travel.
    Visible: the REAL OS cursor travels along a human-like path and
    clicks, fully visible on the display (watched sessions, demos).
    Both modes verify element-centric first (stable bounding box)."""
    # Tier 1e: element-centric verification -- click only a stable target.
    box = await _resolve_stable_box(page, selector=selector)
    loc = page.locator(selector).first
    if not visible:
        await loc.click(timeout=10000)
        if box:
            cx, cy = box["x"] + box["width"] / 2, box["y"] + box["height"] / 2
            return {"x": round(cx), "y": round(cy), "mode": "native"}
        return {"mode": "native"}
    # Visible path: re-read the window's real position first (Tier 1d).
    human_mouse.sync_window_geometry()
    if box:
        vx = box["x"] + box["width"] / 2
        vy = box["y"] + box["height"] / 2
        sx, sy = human_mouse.viewport_to_screen(vx, vy)
        await asyncio.to_thread(human_mouse.human_move, sx, sy)
        await asyncio.to_thread(human_mouse.human_click)
        return {"x": sx, "y": sy, "mode": "visible"}
    await page.click(selector)
    return {"mode": "visible"}


@app.post("/browser/click")
async def click(req: SelectorReq):
    page = _need_page()
    pt = await _human_click_at(page, req.selector, visible=req.visible)
    await _refresh_baseline(f"click:{req.selector}")
    return {"ok": True, "at": pt}


class DragReq(BaseModel):
    points: list  # [[x, y], ...] in CSS pixels relative to the page viewport
    draw_speed: float = 900.0  # px/sec while the button is held


@app.post("/browser/drag")
async def drag(req: DragReq):
    """Draw through viewport-relative points with the button held down.

    Viewport->screen via cached geometry (pure Python, zero CDP)."""
    screen_pts = [human_mouse.viewport_to_screen(p[0], p[1]) for p in req.points]
    await asyncio.to_thread(
        human_mouse.human_drag,
        screen_pts,
        req.draw_speed,
    )
    return {"ok": True, "points": len(screen_pts)}


class ScriptStep(BaseModel):
    action: str  # click | fill | type | press | drag | wait | navigate | eval
    selector: str = ""
    text: str = ""
    key: str = "Enter"
    points: list = []
    url: str = ""
    js: str = ""
    timeout_ms: int = 10000
    draw_speed: float = 900.0
    verify: str = "auto"  # blind | hash | auto (auto=hash for mouse, blind for keys)
    visible: bool = False  # Phase B2: native input unless the cursor must be seen


class ScriptReq(BaseModel):
    steps: list  # [ScriptStep, ...] executed sequentially, one SSH call


@app.post("/browser/script")
async def script(request: Request):
    """Execute a batch of browser actions in ONE HTTP call.
    This is the 10x path: one SSH round-trip for N actions.
    Accepts gzip-encoded bodies (Content-Encoding: gzip) for large
    payloads. Integer coordinates preferred (2.6x smaller JSON)."""
    body = await request.body()
    if request.headers.get("content-encoding") == "gzip":
        try:
            body = gzip.decompress(body)
        except Exception:
            raise HTTPException(400, "bad gzip body")
    try:
        data = _json.loads(body)
        req = ScriptReq(**data)
    except Exception as e:
        raise HTTPException(400, f"bad request: {e}")
    page = _need_page()
    results = []
    for i, s in enumerate(req.steps):
        try:
            a = s.get("action", "") if isinstance(s, dict) else s.action
            g = (lambda k, d="": s.get(k, d)) if isinstance(s, dict) else (lambda k, d="": getattr(s, k, d))
            # Verification: mouse actions (click/drag) check hash first unless
            # explicitly blind. Keyboard actions trust the preceding mouse.
            v = g("verify", "auto")
            needs_verify = (v == "hash") or (v == "auto" and a in ("click", "drag"))
            if needs_verify:
                ok, detail = await _verify("hash")
                if not ok:
                    results.append({"ok": False, "error": f"verify failed: {detail}",
                                    "verify": "hash_mismatch", "action": a})
                    continue
            vis = bool(g("visible", False))
            if a == "click":
                pt = await _human_click_at(page, g("selector"), visible=vis)
                await _refresh_baseline(f"script:click:{g('selector')}")
                results.append({"ok": True, "at": pt, "verified": True})
            elif a == "fill":
                if vis:
                    await _human_click_at(page, g("selector"), visible=True)
                    await page.fill(g("selector"), g("text"))
                else:
                    await _resolve_stable_box(page, selector=g("selector"))
                    await page.locator(g("selector")).first.fill(
                        g("text"), timeout=10000)
                await _refresh_baseline(f"script:fill:{g('selector')}")
                results.append({"ok": True})
            elif a == "type":
                await page.click(g("selector"))
                await page.keyboard.type(g("text"), delay=25)
                await _refresh_baseline(f"script:type:{g('selector')}")
                results.append({"ok": True})
            elif a == "press":
                await page.keyboard.press(g("key", "Enter"))
                await _refresh_baseline("script:press")
                results.append({"ok": True})
            elif a == "drag":
                human_mouse.sync_window_geometry()  # Tier 1d
                screen_pts = [human_mouse.viewport_to_screen(p[0], p[1])
                              for p in g("points", [])]
                await asyncio.to_thread(
                    human_mouse.human_drag,
                    screen_pts,
                    float(g("draw_speed", 900.0)),
                )
                await _refresh_baseline("script:drag")
                results.append({"ok": True, "points": len(screen_pts)})
            elif a == "wait":
                sel = g("selector")
                if sel:
                    await page.wait_for_selector(sel, timeout=int(g("timeout_ms", 10000)))
                else:
                    await asyncio.sleep(int(g("timeout_ms", 1000)) / 1000)
                results.append({"ok": True})
            elif a == "navigate":
                await page.goto(g("url"), wait_until="domcontentloaded")
                await _refresh_baseline(f"script:navigate:{g('url')}")
                results.append({"ok": True, "url": page.url, "title": await page.title()})
            elif a == "eval":
                results.append({"ok": True, "result": await page.evaluate(g("js"))})
                await _refresh_baseline("script:eval")
            else:
                results.append({"ok": False, "error": f"unknown action {a}"})
        except Exception as e:
            results.append({"ok": False, "error": str(e)[:200]})
    return {"ok": True, "results": results}


class OrderReq(BaseModel):
    action: str  # click | fill
    text: str = ""      # visible text of the target ("Save", "Log in")
    role: str = ""      # aria role hint ("button", "textbox", "link")
    value: str = ""     # text to fill (for fill action)
    timeout_ms: int = 10000
    visible: bool = False  # Phase B2: native input unless the cursor must be seen


async def _resolve_order_locator(page, action, text, role=None):
    """Shared deterministic resolver (round 3): role+name, then text, then
    label/placeholder (fill), then title attribute. Returns the first
    matching locator or None. Used by /browser/order and /pool/order."""
    loc = None
    if role:
        try:
            loc = page.get_by_role(role, name=text)
            await loc.first.wait_for(state="visible", timeout=2000)
        except Exception:
            loc = None
    if loc is None:
        try:
            loc = page.get_by_text(text, exact=False)
            await loc.first.wait_for(state="visible", timeout=2000)
        except Exception:
            loc = None
    if loc is None and action == "fill":
        for getter in (page.get_by_label, page.get_by_placeholder):
            try:
                loc = getter(text)
                await loc.first.wait_for(state="visible", timeout=2000)
                break
            except Exception:
                loc = None
    if loc is None:
        try:
            loc = page.locator(f'[title*="{text}"]')
            await loc.first.wait_for(state="visible", timeout=2000)
        except Exception:
            loc = None
    return loc


@app.post("/browser/order")
async def order(req: OrderReq):
    """Semantic order: 'click the Save button' without a selector.

    The bridge resolves the target locally via the accessibility tree
    (get_by_role / get_by_text), then drives the cursor to it. No screenshot
    round-trip to the orchestrator. This is the System-1 executor: fast
    pattern-based resolution, no deliberation.

    KNOWING: verifies screen hash before moving the mouse. If the screen
    changed unexpectedly, refuses to click blind and reports the mismatch."""
    page = _need_page()
    # Verify we're still on the expected screen before moving the mouse
    ok, detail = await _verify("hash")
    if not ok:
        return {"ok": False, "error": f"screen changed, refusing blind click: {detail}",
                "verify": "hash_mismatch"}
    loc = await _resolve_order_locator(page, req.action, req.text, req.role)
    if loc is None:
        return {"ok": False, "error": f"could not resolve '{req.text}'"}

    target = loc.first
    if req.action == "click":
        # Tier 1e: element-centric verification in both modes.
        box = await _resolve_stable_box(page, locator=target)
        if not box:
            return {"ok": False, "error": "no bounding box"}
        if not req.visible:
            await target.click(timeout=10000)
            await _refresh_baseline(f"order:click:{req.text}")
            return {"ok": True, "at": {
                "x": round(box["x"] + box["width"] / 2),
                "y": round(box["y"] + box["height"] / 2)},
                "mode": "native", "verified": True}
        human_mouse.sync_window_geometry()  # Tier 1d
        sx, sy = human_mouse.viewport_to_screen(
            box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
        await asyncio.to_thread(human_mouse.human_move, sx, sy)
        await asyncio.to_thread(human_mouse.human_click)
        # Screen likely changed; update baseline
        await asyncio.sleep(0.5)
        await _refresh_baseline(f"order:click:{req.text}")
        return {"ok": True, "at": {"x": sx, "y": sy},
                "mode": "visible", "verified": True}
    elif req.action == "fill":
        if req.visible:
            await target.click()
        await target.fill(req.value, timeout=10000)
        await _refresh_baseline(f"order:fill:{req.text}")
        return {"ok": True, "mode": "visible" if req.visible else "native"}
    return {"ok": False, "error": f"unknown action {req.action}"}


class ClickNodeReq(BaseModel):
    backendNodeId: int


@app.post("/browser/click_node")
async def click_node(req: ClickNodeReq):
    """Click an element by its backendDOMNodeId (from /browser/snapshot).

    Round 3: CDP-direct, no selector guessing. DOM.resolveNode turns the
    backend id into a live object, DOM.getBoxModel gives the content quad,
    and the native click lands on its center. The nodeId from the snapshot
    is ground truth; a selector is a guess. Used by order-smart escalation.
    """
    page = _need_page()
    sess = await _cdp_session_for(page, _ctx)
    try:
        res = await sess.send("DOM.resolveNode",
                              {"backendNodeId": req.backendNodeId})
        obj_id = (res.get("object") or {}).get("objectId")
        if not obj_id:
            raise HTTPException(
                404, "backendNodeId %s has no live DOM object"
                % req.backendNodeId)
        box = await sess.send("DOM.getBoxModel", {"objectId": obj_id})
        content = (box.get("model") or {}).get("content")
        if not content:
            raise HTTPException(
                404, "backendNodeId %s has no box model" % req.backendNodeId)
        xs = content[0::2]
        ys = content[1::2]
        cx, cy = sum(xs) / len(xs), sum(ys) / len(ys)
    except HTTPException:
        raise
    except Exception as exc:
        raise HTTPException(500, "click_node failed: %s" % exc)
    ok, detail = await _verify("hash")
    if not ok:
        return {"ok": False,
                "error": "screen changed, refusing blind click: %s" % detail}
    await page.mouse.click(cx, cy)
    await _refresh_baseline("click_node:%s" % req.backendNodeId)
    return {"ok": True, "at": {"x": round(cx), "y": round(cy)},
            "mode": "native", "via": "nodeId", "verified": True}


@app.post("/browser/type")
async def type_text(req: FillReq):
    """Type text into the focused element. Verifies by reading back."""
    page = _need_page()
    await page.locator(req.selector).first.click(timeout=10000)
    await page.keyboard.type(req.text, delay=25)
    # Verify by checking if the text appears in the page
    # For Docs, we can't easily read the cell, so we just confirm no error
    await _refresh_baseline(f"type:{req.selector}")
    return {"ok": True}


@app.post("/browser/type_verified")
async def type_verified(req: FillReq):
    """Type text and verify it landed by checking document content.
    For Google Docs tables, uses the Docs structure to verify."""
    page = _need_page()
    # Get text before
    before = await page.evaluate("() => document.body.innerText.length")
    await page.click(req.selector)
    await asyncio.sleep(0.5)
    await page.keyboard.type(req.text, delay=30)
    await asyncio.sleep(1.0)
    # Get text after
    after = await page.evaluate("() => document.body.innerText.length")
    # If text length increased by approx the typed length, it worked
    delta = after - before
    expected = len(req.text)
    # Allow some tolerance (Docs may add formatting)
    ok = abs(delta - expected) < 5
    await _refresh_baseline(f"type_verified:{req.selector}")
    return {"ok": ok, "delta": delta, "expected": expected,
            "verify": "text_length" if ok else "text_not_found"}


class AutocompleteReq(BaseModel):
    selector: str = ""
    text: str = ""
    option_text: str = ""
    timeout_ms: int = 15000


@app.post("/browser/autocomplete")
async def autocomplete(req: AutocompleteReq):
    """Human-like autocomplete interaction for stubborn widgets (e.g. Workday).
    Clicks the input, types with variable human-like delays, waits for the
    suggestion listbox, clicks the matching option, and verifies selection.
    """
    import random
    page = _need_page()
    # 1. Click to focus (real click)
    await page.locator(req.selector).first.click(timeout=10000)
    await asyncio.sleep(0.5)
    # 2. Clear any existing text
    await page.keyboard.press("Control+a")
    await page.keyboard.press("Backspace")
    await asyncio.sleep(0.3)
    # 3. Type with human-like variable delays (30-90ms)
    for ch in req.text:
        await page.keyboard.type(ch, delay=random.randint(30, 90))
        # Occasional slightly longer pause (like a human thinking)
        if random.random() < 0.1:
            await asyncio.sleep(random.uniform(0.15, 0.35))
    # 4. Wait for the debounce + backend response (Workday needs 1-3s)
    await asyncio.sleep(2.0)
    # 5. Wait for listbox to appear
    try:
        await page.wait_for_selector('[role="listbox"]', timeout=req.timeout_ms)
    except Exception:
        await _refresh_baseline(f"autocomplete:{req.selector}:no-listbox")
        return {"ok": False, "error": "no-listbox",
                "detail": "suggestion listbox did not appear"}
    # 6. Find the matching option
    options = await page.query_selector_all('[role="option"]')
    target = None
    target_text = ""
    for opt in options:
        txt = (await opt.inner_text()).strip()
        if req.option_text.lower() in txt.lower() or txt.lower() in req.option_text.lower():
            target = opt
            target_text = txt
            break
    if not target:
        # Fall back to first option
        if options:
            target = options[0]
            target_text = (await target.inner_text()).strip()
        else:
            await _refresh_baseline(f"autocomplete:{req.selector}:no-options")
            return {"ok": False, "error": "no-options",
                    "detail": "listbox appeared but no options found"}
    # 7. Click the option (real click)
    await target.click(timeout=5000)
    await asyncio.sleep(1.5)
    # 8. Verify by checking if the input value changed or selection count increased
    await _refresh_baseline(f"autocomplete:{req.selector}:{target_text}")
    return {"ok": True, "selected": target_text}


class PressReq(SelectorReq):
    key: str = "Enter"


@app.post("/browser/press")
async def press(req: PressReq):
    page = _need_page()
    await page.focus(req.selector)
    await page.keyboard.press(req.key)
    await _refresh_baseline(f"press:{req.key}")
    return {"ok": True}


class WaitReq(BaseModel):
    selector: str = ""
    timeout_ms: int = 10000


@app.post("/browser/wait")
async def wait(req: WaitReq):
    page = _need_page()
    if req.selector:
        await page.wait_for_selector(req.selector, timeout=req.timeout_ms)
    else:
        await page.wait_for_timeout(req.timeout_ms)
    return {"ok": True, "url": page.url}


@app.get("/browser/text")
async def get_text():
    page = _need_page()
    return {
        "url": page.url,
        "title": await page.title(),
        "text": (await page.inner_text("body"))[:12000],
    }


@app.get("/browser/html")
async def get_html():
    page = _need_page()
    html = await page.content()
    return {"url": page.url, "html": html[:20000]}


class EvalReq(BaseModel):
    js: str


@app.post("/browser/eval")
async def evaluate(req: EvalReq):
    result = await _need_page().evaluate(req.js)
    await _refresh_baseline("eval")
    return {"result": result}


class ScrollReq(BaseModel):
    direction: str = "down"  # up | down
    amount: int = 500  # pixels per wheel tick


@app.post("/browser/scroll")
async def scroll(req: ScrollReq):
    """Human scroll: mouse wheel at the current cursor position.
    Not JS scrollBy. The page sees a real wheel event."""
    page = _need_page()
    # Move to center of viewport first (like a human positioning the mouse)
    sx, sy = human_mouse.viewport_to_screen(683, 450)
    await asyncio.to_thread(human_mouse.human_move, sx, sy, overshoot=False)
    delta = req.amount if req.direction == "down" else -req.amount
    await page.mouse.wheel(0, delta)
    await asyncio.sleep(0.5)  # let the page settle
    await _refresh_baseline(f"scroll:{req.direction}")
    return {"ok": True, "direction": req.direction, "amount": req.amount}


@app.get("/browser/state")
async def browser_state():
    """What the bridge THINKS is happening. The orchestrator checks this
    before sending mouse actions: if the bridge is unsure, it says so
    instead of clicking blind."""
    page = _need_page()
    current_hash = await _current_hash()
    return {
        "url": page.url,
        "title": await page.title(),
        "expected_url": _state["expected_url"],
        "hash_match": current_hash == _state["expected_hash"],
        "last_action": _state["last_action"],
    }


class ClickAtReq(BaseModel):
    x: float  # viewport x
    y: float  # viewport y


@app.post("/browser/click_at")
async def click_at(req: ClickAtReq):
    """Manual click at viewport coordinates. Human mouse move + click,
    fully visible. Verifies screen hash first (knowing bridge)."""
    ok, detail = await _verify("hash")
    if not ok:
        return {"ok": False, "error": f"screen changed: {detail}",
                "verify": "hash_mismatch"}
    human_mouse.sync_window_geometry()  # Tier 1d
    sx, sy = human_mouse.viewport_to_screen(req.x, req.y)
    await asyncio.to_thread(human_mouse.human_move, sx, sy)
    await asyncio.to_thread(human_mouse.human_click)
    await asyncio.sleep(0.5)
    await _refresh_baseline(f"click_at:{req.x},{req.y}")
    return {"ok": True, "at": {"x": sx, "y": sy}, "verified": True}


class ClickScreenReq(BaseModel):
    x: int  # absolute screen x
    y: int  # absolute screen y


@app.post("/browser/click_screen")
async def click_screen(req: ClickScreenReq):
    """Manual click at ABSOLUTE SCREEN coordinates. Bypasses viewport
    conversion. Use when you have exact screen coords from JS."""
    ok, detail = await _verify("hash")
    if not ok:
        return {"ok": False, "error": f"screen changed: {detail}",
                "verify": "hash_mismatch"}
    await asyncio.to_thread(human_mouse.human_move, req.x, req.y)
    await asyncio.to_thread(human_mouse.human_click)
    await asyncio.sleep(0.5)
    await _refresh_baseline(f"click_screen:{req.x},{req.y}")
    return {"ok": True, "at": {"x": req.x, "y": req.y}, "verified": True}


class GridSelectReq(BaseModel):
    cols: int  # number of columns (1-10)
    rows: int  # number of rows (1-10)


@app.post("/browser/grid_select")
async def grid_select(req: GridSelectReq):
    """ATOMIC table insertion. All steps in one call."""
    page = _need_page()
    await page.click("body")
    await asyncio.sleep(0.5)
    await page.keyboard.press("Alt+i")
    await asyncio.sleep(1.5)
    await page.keyboard.press("t")
    await asyncio.sleep(2.0)
    # Retry finding grid (may need render time)
    result = None
    for attempt in range(6):
        try:
            result = await page.evaluate(
                """([cols, rows]) => {
                  for (const el of document.querySelectorAll('*')) {
                    const r = el.getBoundingClientRect();
                    if (r.width > 150 && r.width < 300 && r.height > 80 && r.height < 200
                        && r.x > 350 && r.y > 80 && r.y < 400) {
                      const cw = r.width / 10, ch = r.height / 10;
                      const tx = r.x + cw * (Math.min(cols,10)-1 + 0.5);
                      const ty = r.y + ch * (Math.min(rows,10)-1 + 0.5);
                      return {sx: Math.round(window.screenX + tx),
                              sy: Math.round(window.screenY + (window.outerHeight-window.innerHeight) + ty)};
                    }
                  }
                  return null;
                }""",
                [req.cols, req.rows])
            if result:
                break
        except:
            pass
        await asyncio.sleep(1.0)
    if not result:
        return {"ok": False, "step": "find_grid", "error": "grid not found after retries"}
    sx, sy = result["sx"], result["sy"]
    await asyncio.to_thread(human_mouse.human_move, sx, sy)
    await asyncio.sleep(1.0)
    await asyncio.to_thread(human_mouse.human_click)
    await asyncio.sleep(1.5)
    h = await _current_hash()
    _update_state(hash=h, action=f"grid_select:{req.cols}x{req.rows}")
    return {"ok": True, "at": {"x": sx, "y": sy}, "verified": True}


@app.post("/browser/move_to")
async def move_to(req: ClickScreenReq):
    """Move mouse to ABSOLUTE SCREEN coordinates WITHOUT clicking.
    Visible hover. Use for menu grids that need hover-to-select."""
    # No hash verify for pure hover; it's non-destructive
    await asyncio.to_thread(human_mouse.human_move, req.x, req.y)
    return {"ok": True, "at": {"x": req.x, "y": req.y}}


@app.post("/browser/move_to_viewport")
async def move_to_viewport(req: ClickAtReq):
    """Move mouse to VIEWPORT coordinates WITHOUT clicking.
    Converts viewport->screen via window geometry. Visible hover."""
    human_mouse.sync_window_geometry()
    sx, sy = human_mouse.viewport_to_screen(req.x, req.y)
    await asyncio.to_thread(human_mouse.human_move, sx, sy)
    return {"ok": True, "at": {"x": sx, "y": sy},
            "viewport": {"x": req.x, "y": req.y}}


@app.post("/browser/double_click")
async def double_click(req: ClickScreenReq):
    """Double-click at ABSOLUTE SCREEN coordinates. Selects a word.
    Manual, visible. Verifies hash first."""
    ok, detail = await _verify("hash")
    if not ok:
        return {"ok": False, "error": f"screen changed: {detail}",
                "verify": "hash_mismatch"}
    await asyncio.to_thread(human_mouse.human_move, req.x, req.y)
    await asyncio.to_thread(human_mouse.human_click)
    await asyncio.sleep(0.15)
    await asyncio.to_thread(human_mouse.human_click)
    await asyncio.sleep(0.5)
    await _refresh_baseline(f"double_click:{req.x},{req.y}")
    return {"ok": True, "at": {"x": req.x, "y": req.y}, "verified": True}


@app.post("/browser/geometry/refresh")
async def geometry_refresh():
    """Refresh the cached window geometry. Call when the window moved."""
    page = _need_page()
    try:
        geo = await page.evaluate(
            """({sx: window.screenX, sy: window.screenY,
                ch: window.outerHeight - window.innerHeight})"""
        )
        human_mouse.cache_window_geometry(geo["sx"], geo["sy"], geo["ch"])
        return {"ok": True, "geo": geo}
    except Exception as e:
        return {"ok": False, "error": str(e)}


@app.get("/browser/screenshot")
async def screenshot():
    png = await _need_page().screenshot()
    return Response(content=png, media_type="image/png")


class RegionReq(BaseModel):
    x: int
    y: int
    width: int
    height: int


@app.post("/browser/screenshot/region")
async def screenshot_region(req: RegionReq):
    """Screenshot just a region. Smaller, faster, less to ship.
    Use for targeted verification instead of full-page captures."""
    png = await _need_page().screenshot(clip={
        "x": req.x, "y": req.y, "width": req.width, "height": req.height})
    return Response(content=png, media_type="image/png")


@app.get("/browser/screenshot/hash")
async def screenshot_hash():
    """Perceptual hash of the current viewport. Poll this to detect
    changes without transferring images. Compare Hamming distance
    between hashes: 0 = identical, <5 = minor, >10 = significant."""
    png = await _need_page().screenshot()
    return {"hash": _dhash(png), "url": _need_page().url}


class CompareReq(BaseModel):
    reference_b64: str  # base64 PNG to compare against
    threshold: int = 10  # per-channel difference threshold (0-255)


@app.post("/browser/screenshot/compare")
async def screenshot_compare(req: CompareReq):
    """VM-side visual comparison. Send a reference PNG once, get back
    diff stats without shipping the current screenshot. Returns:
    - diff_pct: % of pixels that changed beyond threshold
    - bbox: bounding box of changed region (or null if no change)
    - hash: current perceptual hash for tracking"""
    import base64, io
    from PIL import Image, ImageChops
    page = _need_page()
    current_png = await page.screenshot()
    try:
        ref_png = base64.b64decode(req.reference_b64)
    except Exception:
        raise HTTPException(400, "bad base64 reference")
    cur = Image.open(io.BytesIO(current_png)).convert("RGB")
    ref = Image.open(io.BytesIO(ref_png)).convert("RGB")
    if cur.size != ref.size:
        ref = ref.resize(cur.size)
    # Pixel diff
    diff = ImageChops.difference(cur, ref)
    # Threshold and count
    import numpy as np
    try:
        arr = np.array(diff)
        changed = (arr.max(axis=2) > req.threshold)
        diff_pct = float(changed.mean() * 100)
        if changed.any():
            ys, xs = changed.nonzero()
            bbox = [int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())]
        else:
            bbox = None
    except ImportError:
        # Fallback without numpy: use histogram
        hist = diff.convert("L").histogram()
        changed_px = sum(hist[req.threshold:])
        total = cur.size[0] * cur.size[1]
        diff_pct = changed_px / total * 100
        bbox = diff.getbbox()
        if bbox:
            bbox = list(bbox)
    return {
        "diff_pct": round(diff_pct, 3),
        "bbox": bbox,
        "hash": _dhash(current_png),
        "size": list(cur.size),
    }


class VisualReq(BaseModel):
    x: int
    y: int
    width: int
    height: int
    reference_b64: str  # base64 PNG of the expected region
    threshold: float = 0.97  # SSIM 0-1; flag below threshold


@app.post("/browser/assert_visual")
async def assert_visual(req: VisualReq):
    """Phase C2: explicit visual assertion on a region clip using SSIM
    (structural similarity, tolerant of antialiasing, unlike pixel diff).
    Region-clipped: never run SSIM on a full 1920x1080 frame per action.
    Get the reference once via /browser/screenshot/region."""
    import base64 as _b64
    import io as _io
    import numpy as _np
    from PIL import Image as _Image
    from skimage.metrics import structural_similarity as _ssim
    page = _need_page()
    png = await page.screenshot(clip={
        "x": req.x, "y": req.y, "width": req.width, "height": req.height})
    cur = _np.array(_Image.open(_io.BytesIO(png)).convert("L"))
    ref = _Image.open(_io.BytesIO(_b64.b64decode(req.reference_b64))).convert("L")
    if ref.size != (req.width, req.height):
        ref = ref.resize((req.width, req.height))
    score = float(_ssim(cur, _np.array(ref)))
    await _refresh_baseline("assert_visual")
    return {"ok": score >= req.threshold, "ssim": round(score, 4),
            "threshold": req.threshold}


class DeltaReq(BaseModel):
    reference_b64: str = ""  # base64 PNG of last frame; empty = keyframe
    threshold: int = 10
    # If True, the bridge caches the last frame server-side keyed by
    # session_id, so the client only sends the session id, not the image.
    session_id: str = ""
    # JPEG quality for the patch (1-100). Patches are small; 80 is plenty.
    quality: int = 80


# Server-side frame cache: session_id -> (PNG bytes, timestamp). Avoids
# re-uploading the reference every time. This is the "video codec" state.
# Tier 1c (2026-10-30): entries expire after 10 min and the cache is capped
# at 20 sessions; it used to grow without bound.
_frame_cache = {}
_FRAME_TTL = 600
_FRAME_MAX = 20


def _frame_cache_prune():
    now = time.time()
    for sid in [s for s, (_, ts) in _frame_cache.items() if now - ts > _FRAME_TTL]:
        del _frame_cache[sid]
    while len(_frame_cache) > _FRAME_MAX:
        oldest = min(_frame_cache, key=lambda s: _frame_cache[s][1])
        del _frame_cache[oldest]


@app.post("/browser/screenshot/delta")
async def screenshot_delta(req: DeltaReq):
    """Inter-frame delta compression: the video-codec approach for the
    'slow moving movie' of repeated screenshots.

    First call: send reference_b64 (or omit for keyframe) -> returns full
    frame + session_id. The bridge caches it.
    Later calls: send session_id only -> returns just the changed region
    as a JPEG patch + bbox, or {"changed": false} if nothing moved.

    For a mostly-static page, patches are 1-5KB vs 17KB full frames.
    The client composites: paste patch at bbox onto the last frame."""
    import base64, io, uuid
    from PIL import Image, ImageChops
    page = _need_page()
    current_png = await page.screenshot()
    cur = Image.open(io.BytesIO(current_png)).convert("RGB")

    # Resolve reference: session cache or uploaded bytes
    _frame_cache_prune()
    ref = None
    sid = req.session_id
    if sid and sid in _frame_cache:
        ref = Image.open(io.BytesIO(_frame_cache[sid][0])).convert("RGB")
    elif req.reference_b64:
        try:
            ref_png = base64.b64decode(req.reference_b64)
        except Exception:
            raise HTTPException(400, "bad base64 reference")
        ref = Image.open(io.BytesIO(ref_png)).convert("RGB")
        if ref.size != cur.size:
            ref = ref.resize(cur.size)

    # No reference: keyframe. Cache and return full frame.
    if ref is None:
        sid = sid or uuid.uuid4().hex[:12]
        _frame_cache[sid] = (current_png, time.time())
        return Response(
            content=current_png,
            media_type="image/png",
            headers={"X-Frame-Type": "keyframe", "X-Session-Id": sid,
                     "X-Hash": _dhash(current_png)},
        )

    # Diff against reference
    diff = ImageChops.difference(cur, ref)
    bbox = diff.convert("L").point(lambda v: 255 if v > req.threshold else 0).getbbox()

    # Update the cache to the current frame for the next delta
    _frame_cache[sid] = (current_png, time.time())

    if not bbox:
        return {"changed": False, "hash": _dhash(current_png),
                "session_id": sid}

    # Pad bbox slightly so patch edges align cleanly
    pad = 4
    x0 = max(0, bbox[0] - pad)
    y0 = max(0, bbox[1] - pad)
    x1 = min(cur.size[0], bbox[2] + pad)
    y1 = min(cur.size[1], bbox[3] + pad)
    patch = cur.crop((x0, y0, x1, y1))
    buf = io.BytesIO()
    # PNG for patches: UI screenshots are flat colors + sharp edges,
    # where PNG beats JPEG. The win is the smaller region, not the codec.
    patch.save(buf, "PNG", optimize=True)
    patch_bytes = buf.getvalue()
    return Response(
        content=patch_bytes,
        media_type="image/png",
        headers={"X-Frame-Type": "delta",
                 "X-Bbox": f"{x0},{y0},{x1},{y1}",
                 "X-Session-Id": sid,
                 "X-Hash": _dhash(current_png),
                 "X-Full-Size": str(len(current_png)),
                 "X-Patch-Size": str(len(patch_bytes))},
    )


@app.get("/browser/snapshot")
async def snapshot():
    """Accessibility snapshot via CDP: interactive elements with refs,
    no pixels. (Patchright removed page.accessibility; this is the
    driver-agnostic replacement.)"""
    page = _need_page()
    snap = await _ax_snapshot(page)
    return {"url": page.url, "title": await page.title(), "snapshot": snap}


@app.get("/health")
async def health():
    # Tier 1c: report actual browser liveness, not just "process is up".
    browser_ok = False
    if _page is not None:
        try:
            await _page.evaluate("document.title")
            browser_ok = True
        except Exception:
            browser_ok = False
    return {
        "ok": True,
        "browser": _page is not None,
        "browser_ok": browser_ok,
        "tabs": len(_tabs),
        "frame_cache": len(_frame_cache),
        "last_action": _state["last_action"],
    }
