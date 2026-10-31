#!/usr/bin/env python3
"""bridge-cli: typed client for vm-bridge on the Chromebook VM.

Replaces hand-built curl invocations (and their quoting hell). Every call
goes through `cb-job.sh run` over the SSH tunnel; the JSON body is base64'd
into argv so no shell quoting is ever needed.

Auth: the API key is NOT held here. curl reads it on the VM from
~/.config/vm-bridge/env (mode 600) — the same EnvironmentFile the
vm-bridge-rs systemd unit uses — so the CLI and the server can never
disagree about which key file is authoritative. The key is extracted
with a remote-shell command substitution, never placed in argv, so it
never crosses SSH argv and never appears in the activity panel or logs.

Usage:
    bridge-cli.py health
    bridge-cli.py state
    bridge-cli.py navigate https://example.com
    bridge-cli.py text
    bridge-cli.py click "#submit"
    bridge-cli.py fill "#q" "hello"
    bridge-cli.py type "#q" "hello"
    bridge-cli.py press "#q" Enter
    bridge-cli.py eval "document.title"
    bridge-cli.py wait "#done" 10000
    bridge-cli.py order click "Save" button
    bridge-cli.py order fill "Email" textbox "a@b.c"
    bridge-cli.py order-smart click "checkout button"   # /order + Gemini fallback
    bridge-cli.py pool acquire [ttl_s] [trace]          # headless context
    bridge-cli.py pool navigate CTXID https://example.com
    bridge-cli.py pool text CTXID
    bridge-cli.py pool eval CTXID "document.title"
    bridge-cli.py pool click CTXID "#submit"
    bridge-cli.py pool fill CTXID "#q" "hello"
    bridge-cli.py pool order CTXID click "Save" button
    bridge-cli.py pool snapshot CTXID
    bridge-cli.py pool release CTXID
    bridge-cli.py pool status
    bridge-cli.py click-node 12345678                  # CDP click by backendNodeId
    bridge-cli.py script steps.json        # {"steps": [...]}
    bridge-cli.py tabs
    bridge-cli.py tab-new https://example.com
    bridge-cli.py tab-switch ab12cd34
    bridge-cli.py snapshot
    bridge-cli.py shot /tmp/shot.png       # full-page PNG screenshot
    bridge-cli.py call POST /browser/navigate '{"url":"https://example.com"}'
    bridge-cli.py call GET /browser/text
    bridge-cli.py lease acquire [client] [ttl_s]
    bridge-cli.py lease release LEASE_ID
    bridge-cli.py lease status
    bridge-cli.py --lease LEASE_ID click "#save"   # act under a lease
    bridge-cli.py --visible click "#save"          # visible XTest cursor

Leases: when a lease is held, mutating calls (POST/PUT/PATCH/DELETE) need
the matching X-Lease-Id header or the bridge answers 423. Reads never
need a lease. A lease expires on its own if the holder dies.
"""

import base64
import gzip
import json
import os
import re
import subprocess
import sys
import time
import shlex
import subprocess
import sys


# Optional: a Gemini CLI used by order-smart intent resolution.
# Set GEMINI_BIN to your own generator script, or leave unset to disable
# the AI-escalation commands (order-smart, move-ai).
_GEMINI_BIN = os.environ.get("GEMINI_BIN", "")


def _gemini_resolve(action, text):
    """Escalate an ambiguous intent to Gemini (round 2, gap 4; round 3:
    nodeId-first). Uses the accessibility snapshot (cheap), never a
    screenshot. Returns (backendNodeId|None, selector|None). The nodeId is
    ground truth from the snapshot; a selector is a guess. Runs
    agent-side: the API key never leaves this box."""
    try:
        code, body = call("GET", "/browser/snapshot")
        snap = json.loads(body)
    except Exception:
        return None, None
    snap_s = json.dumps(snap)[:6000]
    prompt = (
        "You are resolving a browser automation intent to a DOM node.\n"
        "Action: %s\nTarget description: %s\n\n"
        "Accessibility snapshot of the page (JSON, truncated). Each entry "
        "has a role, an accessible name, and a backendNodeId:\n%s\n\n"
        "TRAINING (Tier 1 - Workday widgets):\n"
        "- For dropdown/autocomplete options: look for role='option' entries. "
        "The accessible name is the visible option text.\n"
        "- For Workday multiselect items: they have role='option' with "
        "data-automation-id='promptLeafNode'. Match by the visible text.\n"
        "- Prefer EXACT text matches. If the target says 'Data Protection', "
        "pick the option whose name is exactly 'Data Protection', not "
        "'Data Protection Act' or similar.\n"
        "- If multiple options match, prefer the shortest exact match.\n\n"
        "Reply with ONLY a JSON object: "
        "{\"backendNodeId\": <number or null>, \"selector\": \"<selector or null>\"}. "
        "Pick the entry whose role and name best match the target and copy "
        "its backendNodeId EXACTLY. Also give a Playwright selector as "
        "role=<role>[name=\"<accessible name>\"] with the name copied "
        "exactly. If nothing matches, reply "
        "{\"backendNodeId\": null, \"selector\": null}."
        % (action, text, snap_s))
    if not _GEMINI_BIN:
        return None, None
    try:
        p = subprocess.run(
            [sys.executable, _GEMINI_BIN, "--prompt", prompt,
             "--model", "gemini-3.1-flash-lite", "--max-tokens", "256"],
            capture_output=True, text=True, timeout=90)
        out = (p.stdout or "").strip()
        m = re.search(r"\{[^{}]*\"backendNodeId\"[^{}]*\}", out, re.S)
        if not m:
            return None, None
        d = json.loads(m.group(0))
        node = d.get("backendNodeId")
        sel = d.get("selector")
        return (node if isinstance(node, int) else None,
                sel if isinstance(sel, str) else None)
    except Exception:
        return None, None


def _gemini_selector(action, text):
    """Legacy wrapper: selector only."""
    _, sel = _gemini_resolve(action, text)
    return sel

HERE = os.path.dirname(os.path.abspath(__file__))
CB_JOB = os.path.join(HERE, "cb-job.sh")
BASE = "http://localhost:8899"
# On the VM, mode 600. This is the SAME file the vm-bridge-rs systemd
# unit loads via EnvironmentFile (audit 42 High-6): the CLI must never
# read a different key file than the server, or auth breaks confusingly.
ENV_FILE = "~/.config/vm-bridge/env"  # on the VM
# Back-compat alias (the pre-audit-42 name); prefer ENV_FILE.
KEY_FILE = ENV_FILE


def _key_header_fragment():
    """Remote-shell fragment producing the X-API-Key header value.

    Extracts the key from the unit's EnvironmentFile on the VM:
    VM_BRIDGE_KEYS__ADMIN first (covers every scope), then the legacy
    BRIDGE_API_KEY (accepted for all scopes). Values are trimmed of
    whitespace/CR/LF on this side; the server trims again on its side.
    The key is NEVER placed in argv — it stays inside the remote shell's
    command substitution, so it never crosses SSH argv or lands in logs.
    """
    return (
        '"X-API-Key: $(f=$HOME/.config/vm-bridge/env;'
        "k=$(sed -n 's/^VM_BRIDGE_KEYS__ADMIN=//p' \"$f\"|head -n 1);"
        "[ -z \"$k\" ]&&k=$(sed -n 's/^BRIDGE_API_KEY=//p' \"$f\"|head -n 1);"
        "printf '%s' \"$k\"|tr -d ' \\t\\r\\n')\""
    )


def _inner(method, path, data=None, lease=None):
    """Build the remote bash command. Body travels base64'd in argv.

    The auth header is appended UNQUOTED on purpose: it needs $() and
    $HOME expansion on the VM. Everything else is shell-quoted.
    The key itself never appears here; curl reads it from the mode-600
    file on the VM.

    Response framing: curl appends a unique trailer line
    __BRIDGE_DONE__:<curl_exit>:<http_code> so the caller can tell a
    transport failure apart from an HTTP error, even when the body is
    empty or binary. (2026-10-31 fix: the old "\\n%{http_code}" suffix
    broke with ValueError whenever curl exited non-zero with no body.)"""
    parts = [
        "curl", "-s",
        "-w", r"\n__BRIDGE_DONE__:%{exitcode}:%{http_code}",
        "-X", method,
        "-H", "Content-Type: application/json",
    ]
    if lease:
        parts += ["-H", f"X-Lease-Id: {lease}"]
    body_b64 = None
    if data is not None:
        raw = json.dumps(data).encode()
        if len(raw) > 4096:
            raw = gzip.compress(raw)
            parts += ["-H", "Content-Encoding: gzip"]
        body_b64 = base64.b64encode(raw).decode()
        parts += ["--data-binary", "@-"]
    parts.append(BASE + path)
    cmd = " ".join(shlex.quote(p) for p in parts)
    # The auth header is appended UNQUOTED on purpose: it needs $() and
    # $HOME expansion on the VM. The key itself never appears here; the
    # remote shell extracts it from the unit's EnvironmentFile
    # (~/.config/vm-bridge/env, mode 600) via _key_header_fragment().
    cmd += " -H " + _key_header_fragment()
    if body_b64 is not None:
        cmd = f"echo {body_b64} | base64 -d | {cmd}"
    return cmd


_LEASE = os.environ.get("BRIDGE_LEASE", "")


_CURL_TRANSIENT = {7, 28, 52}  # connect failed, timeout, empty reply
_CURL_ERR = {
    7: "could not connect to the bridge (is it running?)",
    28: "bridge request timed out",
    52: "bridge closed the connection with an empty reply (transient)",
}


def call(method, path, data=None, lease=None, _retried=False):
    """Returns (http_code, body_text). Raises on transport failure.

    Retries once on transient curl failures (7/28/52) with a 2s backoff."""
    inner = _inner(method, path, data, lease or _LEASE or None)
    p = subprocess.run([CB_JOB, "run", inner],
                       capture_output=True, text=True, timeout=620)
    if p.returncode != 0:
        raise RuntimeError(f"cb-job.sh failed rc={p.returncode}: "
                           f"{p.stderr.strip() or p.stdout.strip()}")
    out = p.stdout
    m = re.search(r"__BRIDGE_DONE__:(\d+):(\d{3})\s*$", out)
    if not m:
        raise RuntimeError(f"could not parse bridge response: {out[:200]}")
    curl_rc, code = int(m.group(1)), int(m.group(2))
    body = out[:m.start()].rstrip("\n")
    if curl_rc != 0:
        if curl_rc in _CURL_TRANSIENT and not _retried:
            time.sleep(2)
            return call(method, path, data, lease, _retried=True)
        hint = _CURL_ERR.get(curl_rc, f"curl exit {curl_rc}")
        raise RuntimeError(f"bridge transport failed: {hint}")
    return code, body


def api(method, path, data=None):
    code, body = call(method, path, data)
    if code == 401:
        sys.exit("bridge: 401 unauthorized (API key missing or wrong)")
    if code == 429:
        sys.exit("bridge: 429 rate limited, back off and retry")
    if code == 413:
        sys.exit("bridge: 413 body too large")
    if code == 404:
        sys.exit(f"bridge: 404: {body[:200]}")
    if code == 423:
        sys.exit("bridge: 423 locked -- a lease is held by another client "
                 "(see: bridge-cli.py lease status)")
    if code >= 500:
        sys.exit(f"bridge: {code} server error: {body[:300]}")
    return code, body


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    # Global flags (may precede the command)
    global _LEASE
    visible = False
    rest = []
    it = iter(argv[1:])
    for a in it:
        if a == "--lease":
            _LEASE = next(it, "")
        elif a == "--visible":
            visible = True
        else:
            rest.append(a)
    argv = [argv[0]] + rest
    if len(argv) < 2:
        sys.exit(__doc__)
    c = argv[1]
    vis = {"visible": True} if visible else {}
    try:
        if c == "health":
            code, body = api("GET", "/health")
        elif c == "state":
            code, body = api("GET", "/browser/state")
        elif c == "navigate":
            code, body = api("POST", "/browser/navigate", {"url": argv[2]})
        elif c == "text":
            code, body = api("GET", "/browser/text")
        elif c == "click":
            code, body = api("POST", "/browser/click",
                             {"selector": argv[2], **vis})
        elif c == "move-to":
            # Move mouse to ABSOLUTE SCREEN coordinates without clicking.
            # Usage: bridge-cli.py move-to <x> <y>
            code, body = api("POST", "/browser/move_to",
                             {"x": int(argv[2]), "y": int(argv[3])})
        elif c == "move":
            # Move mouse to VIEWPORT coordinates without clicking (hover).
            # Usage: bridge-cli.py move <x> <y>
            code, body = api("POST", "/browser/move_to_viewport",
                             {"x": float(argv[2]), "y": float(argv[3])})
        elif c == "move-ai":
            # AI-powered mouse move: describe the target, Gemini (Tier 1)
            # finds it in the accessibility snapshot, mouse moves there.
            # Usage: bridge-cli.py move-ai "<description>"
            # Trained for: dropdown options, autocomplete suggestions,
            # Workday multiselect items.
            node, sel = _gemini_resolve("move mouse to", argv[2])
            if node:
                # Read-only locate via /browser/node_box (audit 42 High-7):
                # move-ai must NEVER click. The old code used click_node as
                # a "dry run to get position" — that dispatched a real click.
                code2, body2 = api("POST", "/browser/node_box",
                                   {"backendNodeId": node})
                # node_box returns viewport coords; move_to_viewport takes
                # viewport coords (the old code fed them to /browser/move_to,
                # which expects absolute screen coords).
                try:
                    d2 = json.loads(body2)
                    at = d2.get("at", {})
                    if at.get("x") is not None and at.get("y") is not None:
                        code, body = api("POST", "/browser/move_to_viewport",
                                         {"x": float(at["x"]),
                                          "y": float(at["y"])})
                    else:
                        code, body = 500, '{"ok":false,"error":"no-coords"}'
                except Exception:
                    code, body = 500, '{"ok":false,"error":"parse-failed"}'
            elif sel:
                # Fallback: use selector with move_to_viewport via eval
                code, body = api("POST", "/browser/eval",
                                 {"js": f"(()=>{{const e=document.querySelector({json.dumps(sel)});if(!e)return 'NOT-FOUND';const r=e.getBoundingClientRect();return Math.round(r.x+r.width/2)+','+Math.round(r.y+r.height/2);}})()"})
            else:
                code, body = 500, '{"ok":false,"error":"ai-no-match"}'
        elif c == "click-node":
            # Round 3: CDP click by backendDOMNodeId from snapshot.
            code, body = api("POST", "/browser/click_node",
                             {"backendNodeId": int(argv[2]), **vis})
        elif c == "fill":
            code, body = api("POST", "/browser/fill",
                             {"selector": argv[2], "text": argv[3], **vis})
        elif c == "type":
            code, body = api("POST", "/browser/type",
                             {"selector": argv[2], "text": argv[3]})
        elif c == "autocomplete":
            # Human-like autocomplete: click, type with variable delays,
            # wait for suggestions, click matching option.
            # Usage: bridge-cli.py autocomplete "<selector>" "<text>" "<option_text>"
            d = {"selector": argv[2], "text": argv[3], "option_text": argv[4]}
            if len(argv) > 5:
                d["timeout_ms"] = int(argv[5])
            code, body = api("POST", "/browser/autocomplete", d)
        elif c == "press":
            code, body = api("POST", "/browser/press",
                             {"selector": argv[2],
                              "key": argv[3] if len(argv) > 3 else "Enter"})
        elif c == "eval":
            code, body = api("POST", "/browser/eval", {"js": argv[2]})
        elif c == "wait":
            d = {}
            if len(argv) > 2:
                d["selector"] = argv[2]
            if len(argv) > 3:
                d["timeout_ms"] = int(argv[3])
            code, body = api("POST", "/browser/wait", d)
        elif c == "order":
            d = {"action": argv[2], "text": argv[3], **vis}
            if len(argv) > 4:
                d["role"] = argv[4]
            if len(argv) > 5:
                d["value"] = argv[5]
            code, body = api("POST", "/browser/order", d)
        elif c == "order-smart":
            # Round 2 hybrid intent (gap 4): deterministic /order first
            # (free, fast); on failure escalate to Gemini with the
            # accessibility snapshot for a selector, then retry once.
            d = {"action": argv[2], "text": argv[3], **vis}
            if len(argv) > 4:
                d["role"] = argv[4]
            if len(argv) > 5:
                d["value"] = argv[5]
            code, body = call("POST", "/browser/order", d)
            ok = False
            try:
                ok = bool(json.loads(body).get("ok"))
            except Exception:
                pass
            if not ok and code not in (401, 423, 429):
                # Round 3: nodeId-first escalation. The backendNodeId from
                # the snapshot is ground truth (CDP click, no guessing);
                # the selector is the fallback.
                node, sel = _gemini_resolve(d["action"], d["text"])
                act = d["action"]
                if node is not None and act == "click":
                    code, body = call(
                        "POST", "/browser/click_node",
                        {"backendNodeId": node, **vis})
                    if code == 200:
                        try:
                            ok = bool(json.loads(body).get("ok"))
                        except Exception:
                            pass
                if not ok and sel:
                    if act == "click":
                        code, body = call(
                            "POST", "/browser/click",
                            {"selector": sel, **vis})
                    elif act in ("fill", "type"):
                        code, body = call(
                            "POST", "/browser/" + act,
                            {"selector": sel, "text": d.get("value", ""),
                             **vis})
                    else:
                        code, body = 200, json.dumps(
                            {"ok": False,
                             "error": "escalation supports click/fill/type, "
                                      "got %s" % act})
                    if code != 200:
                        body = json.dumps(
                            {"ok": False,
                             "error": "escalated action failed "
                                      "(bridge %d): %s" % (code, body[:200]),
                             "backendNodeId": node, "selector": sel})
                        code = 200
                if not ok and node is None and sel is None:
                    body = json.dumps(
                        {"ok": False,
                         "error": "deterministic order failed and Gemini "
                                  "escalation produced no target",
                         "order_response": body[:300]})
            if code == 401:
                sys.exit("bridge: 401 unauthorized (API key missing or wrong)")
            if code == 423:
                sys.exit("bridge: 423 locked -- a lease is held by another "
                         "client (see: bridge-cli.py lease status)")
            if code == 429:
                sys.exit("bridge: 429 rate limited, back off and retry")
        elif c == "pool":
            # Round 2: headless ephemeral-context pool (browser #2).
            sub = argv[2] if len(argv) > 2 else "status"
            if sub == "acquire":
                d = {}
                if len(argv) > 3:
                    d["ttl_s"] = int(argv[3])
                if len(argv) > 4:
                    d["trace"] = argv[4].lower() in ("1", "true", "yes")
                code, body = api("POST", "/pool/acquire", d)
            elif sub == "navigate":
                code, body = api("POST", "/pool/navigate",
                                 {"context_id": argv[3], "url": argv[4]})
            elif sub == "text":
                code, body = api("POST", "/pool/text",
                                 {"context_id": argv[3]})
            elif sub == "eval":
                code, body = api("POST", "/pool/eval",
                                 {"context_id": argv[3], "js": argv[4]})
            elif sub == "click":
                code, body = api("POST", "/pool/click",
                                 {"context_id": argv[3],
                                  "selector": argv[4]})
            elif sub == "fill":
                code, body = api("POST", "/pool/fill",
                                 {"context_id": argv[3],
                                  "selector": argv[4], "text": argv[5]})
            elif sub == "order":
                d = {"context_id": argv[3], "action": argv[4],
                     "text": argv[5]}
                if len(argv) > 6:
                    d["role"] = argv[6]
                if len(argv) > 7:
                    d["value"] = argv[7]
                code, body = api("POST", "/pool/order", d)
            elif sub == "snapshot":
                code, body = api("POST", "/pool/snapshot",
                                 {"context_id": argv[3]})
            elif sub == "release":
                code, body = api("POST", "/pool/release",
                                 {"context_id": argv[3]})
            else:
                code, body = api("GET", "/pool/status")
        elif c == "script":
            with open(argv[2]) as f:
                steps = json.load(f)
            if visible:
                for s in steps.get("steps", []):
                    if isinstance(s, dict) and s.get("action") in (
                            "click", "fill", "order"):
                        s.setdefault("visible", True)
            code, body = api("POST", "/browser/script", steps)
        elif c == "lease":
            sub = argv[2] if len(argv) > 2 else "status"
            if sub == "acquire":
                d = {}
                if len(argv) > 3:
                    d["client"] = argv[3]
                if len(argv) > 4:
                    d["ttl_s"] = int(argv[4])
                code, body = api("POST", "/lease/acquire", d)
            elif sub == "release":
                code, body = api("POST", "/lease/release",
                                 {"lease_id": argv[3]})
            else:
                code, body = api("GET", "/lease/status")
        elif c == "tabs":
            code, body = api("GET", "/browser/tabs")
        elif c == "tab-new":
            code, body = api("POST", "/browser/tab/new", {"url": argv[2]})
        elif c == "tab-switch":
            code, body = api("POST", "/browser/tab/switch",
                             {"selector": argv[2]})
        elif c == "snapshot":
            code, body = api("POST", "/browser/snapshot")
        elif c == "shot":
            code, body = call("GET", "/browser/screenshot")
            if code != 200:
                sys.exit(f"bridge: screenshot failed: {code} {body[:200]}")
            # body is raw PNG bytes? No: text mode mangled it. Re-fetch binary.
            sys.exit("shot: binary fetch not supported via text transport; "
                     "use cb-job.sh run 'curl -s localhost:8899/browser/screenshot -H "
                     "\"X-API-Key: $(sed -n 's/^BRIDGE_API_KEY=//p' "
                     "$HOME/.config/vm-bridge/env|head -n 1|tr -d ' \\t\\r\\n')\" "
                     "-o /tmp/shot.png' + cb-xfer pull")
        elif c == "call":
            method, path = argv[2], argv[3]
            data = None
            if len(argv) > 4:
                raw = argv[4]
                if raw.startswith("@"):
                    with open(raw[1:]) as f:
                        data = json.load(f)
                else:
                    data = json.loads(raw)
            code, body = api(method, path, data)
        else:
            sys.exit(f"unknown command: {c}\n{__doc__}")
    except IndexError:
        sys.exit(f"missing argument for: {c}\n{__doc__}")
    print(body)


if __name__ == "__main__":
    main(sys.argv)
