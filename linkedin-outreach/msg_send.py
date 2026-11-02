#!/usr/bin/env python3
"""Send exact draft text in the open compose thread. Usage: msg_send.py "<message>"
Sequence (verified working 2026-11-02): execCommand paragraph fill -> keyboard
wake -> verify match + Send enabled -> click Send -> verify in thread.
Outputs JSON {sent, error}.
"""
import json, subprocess, sys, time

CLI = "/home/hatch/workspace/chromebook-vm/bridge-cli"

def cli(*args):
    r = subprocess.run([CLI, *args], capture_output=True, text=True, timeout=120)
    try:
        return json.loads(r.stdout.strip()).get("result", r.stdout.strip())
    except Exception:
        return r.stdout.strip()

JS_FILL = """(function(MSG){var e=document.querySelector('div.msg-form__contenteditable');if(!e)return 'NO-ED';e.focus();document.execCommand('selectAll',false,null);document.execCommand('delete',false,null);var paras=MSG.split('\\n\\n');for(var i=0;i<paras.length;i++){document.execCommand('insertText',false,paras[i]);if(i<paras.length-1){document.execCommand('insertParagraph',false,null);document.execCommand('insertParagraph',false,null);}}var ok=e.innerText.replace(/\\s+/g,' ')===MSG.replace(/\\s+/g,' ');return JSON.stringify({match:ok});})"""

JS_WAKE = """(function(){var e=document.querySelector('div.msg-form__contenteditable');if(!e)return 'NO-ED';e.focus();var r=document.createRange();r.selectNodeContents(e);r.collapse(false);var s=getSelection();s.removeAllRanges();s.addRange(r);document.execCommand('insertText',false,' ');document.execCommand('delete',false,null);['keydown','keypress','keyup'].forEach(function(t){e.dispatchEvent(new KeyboardEvent(t,{key:'a',bubbles:true,cancelable:true}));});e.dispatchEvent(new InputEvent('input',{bubbles:true}));var btn=Array.from(document.querySelectorAll('button')).find(function(b){return (b.textContent||'').trim()==='Send';});return 'disabled:'+(btn?btn.disabled:'no-btn');})()"""

def main():
    msg = sys.argv[1]
    res = {"sent": False, "error": None}
    f = cli("eval", JS_FILL + "(" + json.dumps(msg) + ")")
    try:
        fj = json.loads(f)
    except Exception:
        res["error"] = "fill-parse:" + str(f)[:100]; print(json.dumps(res)); return
    if not fj.get("match"):
        res["error"] = "text-mismatch"; print(json.dumps(res)); return
    w = cli("eval", JS_WAKE)
    if "disabled:false" not in w:
        time.sleep(1)
        w = cli("eval", JS_WAKE)
    if "disabled:false" not in w:
        res["error"] = "send-disabled"; print(json.dumps(res)); return
    s = cli("eval", "(function(){var b=Array.from(document.querySelectorAll('button')).find(function(x){return (x.textContent||'').trim()==='Send';});if(!b||b.disabled)return 'NO-SEND';b.click();return 'CLICKED';})()")
    if s != "CLICKED":
        res["error"] = "click-failed:" + str(s); print(json.dumps(res)); return
    time.sleep(4)
    c = cli("eval", "(function(){var items=Array.from(document.querySelectorAll('.msg-s-message-list__event'));if(!items.length)return 'NO-ITEMS';var t=(items[items.length-1].innerText||'');return JSON.stringify({n:items.length,mine:t.indexOf('Mohammad Movahedi')>-1});})()")
    try:
        res["sent"] = bool(json.loads(c).get("mine"))
        if not res["sent"]:
            res["error"] = "unconfirmed:" + str(c)[:100]
    except Exception:
        res["error"] = "confirm-parse:" + str(c)[:100]
    print(json.dumps(res))

if __name__ == "__main__":
    main()
