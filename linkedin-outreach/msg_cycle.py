#!/usr/bin/env python3
"""One recipient cycle: compose -> type name -> select 1st-conn match -> read thread.
Usage: python3 msg_cycle.py "Full Name"
Outputs JSON: {name, selected, thread: [{text}], error}
"""
import json, subprocess, sys, time

CLI = "/home/hatch/workspace/chromebook-vm/bridge-cli"
COMPOSE_JS = ("(function(){var els=Array.from(document.querySelectorAll('*')).filter("
              "function(e){return (e.className||'').toString().indexOf("
              "'msg-conversations-container__compose-btn')>-1;});"
              "var b=els[0];if(!b)return 'NO-BTN';b.click();return 'CLICKED';})()")
OPEN_JS = ("(function(){var h=document.documentElement.outerHTML;"
           "return h.indexOf('Type a name or multiple names')>-1?'OPEN':'CLOSED';})()")
CLEAR_JS = ("(function(){var btns=Array.from(document.querySelectorAll('button'));"
            "btns.forEach(function(b){var al=b.getAttribute('aria-label')||'';"
            "if(al.indexOf('Remove ')==0)b.click();});return 'CLEARED';})()")

def cli(*args):
    r = subprocess.run([CLI, *args], capture_output=True, text=True, timeout=120)
    out = r.stdout.strip()
    try:
        return json.loads(out).get("result", out)
    except Exception:
        return out

def main():
    name = sys.argv[1]
    res = {"name": name, "selected": None, "thread": [], "error": None}

    cli("eval", CLEAR_JS)
    time.sleep(1)

    opened = False
    for _ in range(6):
        if cli("eval", OPEN_JS) == "OPEN":
            opened = True
            break
        cli("eval", COMPOSE_JS)
        time.sleep(3)
    if not opened:
        res["error"] = "compose-open-failed"
        print(json.dumps(res))
        return

    js_type = ("(function(){var i=document.querySelector("
               "'input.msg-connections-typeahead__search-field');"
               "if(!i)return 'NO-INPUT';i.focus();"
               "var ns=Object.getOwnPropertyDescriptor("
               "window.HTMLInputElement.prototype,'value');"
               "if(ns&&ns.set)ns.set.call(i,%s);else i.value=%s;"
               "i.dispatchEvent(new Event('input',{bubbles:true}));"
               "return 'TYPED';})()") % (json.dumps(name), json.dumps(name))
    if cli("eval", js_type) != "TYPED":
        res["error"] = "type-failed"
        print(json.dumps(res))
        return
    time.sleep(4)

    js_sel = ("(function(){var m=document.querySelector('[role=listbox]');"
              "if(!m)return 'NO-MENU';"
              "var opts=Array.from(m.querySelectorAll('[role=option],li'));"
              "var target=%s;var fb=null;"
              "for(var k=0;k<opts.length;k++){var t=opts[k].innerText||'';"
              "if(t.indexOf('1st')<0)continue;"
              "if(t.toLowerCase().indexOf(target)>-1){opts[k].click();"
              "return 'SEL:'+t.slice(0,80);}"
              "if(!fb)fb=opts[k];}"
              "if(fb){fb.click();return 'SEL-FB:'+fb.innerText.slice(0,80);}"
              "return 'NO-1ST';})()") % json.dumps(name.lower())
    sel = cli("eval", js_sel)
    if sel.startswith("SEL:"):
        res["selected"] = sel[4:]
    elif sel.startswith("SEL-FB:"):
        res["selected"] = sel[7:]
    else:
        res["error"] = "select-failed:" + sel
        print(json.dumps(res))
        return
    time.sleep(3)

    js_thread = ("(function(){var items=Array.from("
                 "document.querySelectorAll('.msg-s-message-list__event'));"
                 "var out=[];"
                 "items.forEach(function(li){"
                 "var txt=(li.innerText||'').replace(/\\n+/g,' ').trim().slice(0,400);"
                 "out.push({text:txt});});"
                 "return JSON.stringify(out);})()")
    try:
        res["thread"] = json.loads(cli("eval", js_thread))
    except Exception as e:
        res["error"] = "thread-read:" + str(e)
    print(json.dumps(res))

if __name__ == "__main__":
    main()
