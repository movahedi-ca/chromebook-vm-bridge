// portal-tool: CLI for the job-portal account registry + fresh reset-link checks.
// Usage:
//   portal-tool registry list
//   portal-tool registry get <portal-key>
//   portal-tool registry set <portal-key> [--status S] [--email E] [--entry ID] [--notes N]
//   portal-tool fresh-link <portal-key> [--max-age-min 90]
//   portal-tool next-action <portal-key> [--max-age-min 90]
// Registry path: $PORTAL_REGISTRY or ~/workspace/job-portal-accounts/registry.json
// fresh-link shells out to `hatch_gws_cli gmail users messages list` and reports
// metadata only (subject/date/age). It never unwraps link URLs: those stay
// behind the connector's protected credential markers by design.

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn registry_path() -> PathBuf {
    if let Ok(p) = env::var("PORTAL_REGISTRY") {
        return PathBuf::from(p);
    }
    let home = env::var("HOME").unwrap_or_else(|_| "/home/hatch".into());
    PathBuf::from(home).join("workspace/job-portal-accounts/registry.json")
}

fn load_registry() -> Value {
    let p = registry_path();
    let raw = fs::read_to_string(&p)
        .unwrap_or_else(|e| fail(&format!("cannot read {}: {}", p.display(), e)));
    serde_json::from_str(&raw).unwrap_or_else(|e| fail(&format!("bad JSON: {}", e)))
}

fn save_registry(v: &Value) {
    let p = registry_path();
    let s = serde_json::to_string_pretty(v).unwrap();
    fs::write(&p, s + "\n").unwrap_or_else(|e| fail(&format!("cannot write: {}", e)));
}

fn fail(msg: &str) -> ! {
    eprintln!("portal-tool: {}", msg);
    std::process::exit(1);
}

fn portals(v: &Value) -> &Map<String, Value> {
    v.get("portals")
        .and_then(|p| p.as_object())
        .unwrap_or_else(|| fail("registry has no portals object"))
}

fn now_utc_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  portal-tool registry list\n  portal-tool registry get <portal>\n  portal-tool registry set <portal> [--status S] [--email E] [--entry ID] [--notes N]\n  portal-tool fresh-link <portal> [--max-age-min 90]\n  portal-tool next-action <portal> [--max-age-min 90]"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    match args[0].as_str() {
        "registry" => cmd_registry(&args[1..]),
        "fresh-link" => cmd_fresh_link(&args[1..]),
        "next-action" => cmd_next_action(&args[1..]),
        _ => usage(),
    }
}

fn cmd_registry(args: &[String]) {
    if args.is_empty() {
        usage();
    }
    match args[0].as_str() {
        "list" => {
            let v = load_registry();
            let mut out = BTreeMap::new();
            for (k, p) in portals(&v) {
                out.insert(
                    k.clone(),
                    serde_json::json!({
                        "label": p.get("label"),
                        "email": p.get("email"),
                        "status": p.get("status"),
                        "last_verified": p.get("last_verified"),
                    }),
                );
            }
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
        }
        "get" => {
            if args.len() < 2 {
                usage();
            }
            let v = load_registry();
            match portals(&v).get(&args[1]) {
                Some(p) => println!("{}", serde_json::to_string_pretty(p).unwrap()),
                None => fail(&format!("unknown portal '{}'", args[1])),
            }
        }
        "set" => {
            if args.len() < 2 {
                usage();
            }
            let key = args[1].clone();
            let mut v = load_registry();
            let entry = portals(&v)
                .get(&key)
                .cloned()
                .unwrap_or_else(|| fail(&format!("unknown portal '{}'", key)));
            let mut obj = entry.as_object().cloned().unwrap_or_default();
            let mut i = 2;
            while i < args.len() {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                match args[i].as_str() {
                    "--status" => {
                        obj.insert("status".into(), Value::String(val));
                    }
                    "--email" => {
                        obj.insert("email".into(), Value::String(val));
                    }
                    "--entry" => {
                        let n: i64 = val
                            .parse()
                            .unwrap_or_else(|_| fail("entry must be numeric"));
                        obj.insert("command_center_entry".into(), Value::Number(n.into()));
                    }
                    "--notes" => {
                        obj.insert("notes".into(), Value::String(val));
                    }
                    _ => usage(),
                }
                i += 2;
            }
            obj.insert(
                "updated".into(),
                Value::String(format!("{}", now_utc_secs())),
            );
            v["portals"][&key] = Value::Object(obj);
            save_registry(&v);
            println!("ok: {}", key);
        }
        _ => usage(),
    }
}

// Parse Gmail list output: {"messages":[{"id":...}],...} then fetch metadata
// for up to 5 newest and return (id, date_epoch, subject).
fn gmail_newest(account: &str, query: &str) -> Option<(String, i64, String)> {
    let params = serde_json::json!({
        "userId": "me",
        "q": query,
        "maxResults": 5,
    })
    .to_string();
    let out = Command::new("hatch_gws_cli")
        .args([
            "gmail",
            "users",
            "messages",
            "list",
            "--account",
            account,
            "--params",
        ])
        .arg(params)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let msgs = v.get("messages")?.as_array()?;
    let mut best: Option<(String, i64, String)> = None;
    for m in msgs.iter().take(5) {
        let id = m.get("id")?.as_str()?.to_string();
        let mp = serde_json::json!({
            "userId": "me", "id": id,
            "format": "metadata",
            "metadataHeaders": ["Subject", "Date"],
        })
        .to_string();
        let gout = Command::new("hatch_gws_cli")
            .args([
                "gmail",
                "users",
                "messages",
                "get",
                "--account",
                account,
                "--params",
            ])
            .arg(mp)
            .output()
            .ok()?;
        if !gout.status.success() {
            continue;
        }
        let gv: Value = serde_json::from_slice(&gout.stdout).ok()?;
        let headers = gv.get("payload")?.get("headers")?.as_array()?;
        let mut subject = String::new();
        let mut date_s = String::new();
        for h in headers {
            match h.get("name")?.as_str()? {
                "Subject" => subject = h.get("value")?.as_str()?.to_string(),
                "Date" => date_s = h.get("value")?.as_str()?.to_string(),
                _ => {}
            }
        }
        let epoch = parse_email_date(&date_s);
        match &best {
            Some((_, e, _)) if *e >= epoch => {}
            _ => best = Some((id, epoch, subject)),
        }
    }
    best
}

// Parse "Mon, 02 Nov 2026 17:36:32 +0000" (also tolerates missing weekday).
fn parse_email_date(s: &str) -> i64 {
    let s = s.trim();
    let s = s.split_once(", ").map(|(_, r)| r).unwrap_or(s);
    // now: "02 Nov 2026 17:36:32 +0000"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 5 {
        return 0;
    }
    let day: i64 = parts[0].parse().unwrap_or(0);
    let mon = match parts[1] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => 0,
    };
    let year: i64 = parts[2].parse().unwrap_or(0);
    let t: Vec<i64> = parts[3]
        .split(':')
        .map(|x| x.parse().unwrap_or(0))
        .collect();
    if day == 0 || mon == 0 || year == 0 || t.len() < 3 {
        return 0;
    }
    // days since epoch (civil algorithm)
    let y = if mon <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let mp = ((mon + 9) % 12) as u64;
    let doy = (153 * mp + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    // timezone offset "+0000" / "-0500"
    let tz = parts[4];
    let off: i64 = if tz.len() == 5 && (tz.starts_with('+') || tz.starts_with('-')) {
        let sign = if tz.starts_with('-') { -1 } else { 1 };
        let hh: i64 = tz[1..3].parse().unwrap_or(0);
        let mm: i64 = tz[3..5].parse().unwrap_or(0);
        sign * (hh * 3600 + mm * 60)
    } else {
        0
    };
    days * 86400 + t[0] * 3600 + t[1] * 60 + t[2] - off
}

fn cmd_fresh_link(args: &[String]) {
    if args.is_empty() {
        usage();
    }
    let key = &args[0];
    let mut max_age: i64 = 90;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--max-age-min" {
            max_age = args
                .get(i + 1)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| fail("--max-age-min needs a number"));
            i += 2;
        } else {
            usage();
        }
    }
    let v = load_registry();
    let p = portals(&v)
        .get(key)
        .unwrap_or_else(|| fail(&format!("unknown portal '{}'", key)));
    let account = p
        .get("gmail_account")
        .and_then(|a| a.as_str())
        .unwrap_or_else(|| fail("portal has no gmail_account in registry"));
    let query = p
        .get("gmail_query")
        .and_then(|q| q.as_str())
        .unwrap_or_else(|| fail("portal has no gmail_query in registry"));
    let now = now_utc_secs();
    let result = match gmail_newest(account, query) {
        Some((id, epoch, subject)) => {
            let age_min = if epoch > 0 { (now - epoch) / 60 } else { -1 };
            serde_json::json!({
                "portal": key,
                "has_fresh": age_min >= 0 && age_min <= max_age,
                "newest_id": id,
                "newest_subject": subject,
                "newest_age_min": age_min,
                "max_age_min": max_age,
                "note": "link URLs stay behind the connector's protected markers; fetch the link via the coordinator, never unwrap here",
            })
        }
        None => serde_json::json!({
            "portal": key,
            "has_fresh": false,
            "newest_id": null,
            "note": "no matching emails found",
        }),
    };
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}

fn cmd_next_action(args: &[String]) {
    if args.is_empty() {
        usage();
    }
    // Decide: sign-in directly | reuse fresh link | request one reset.
    let key = &args[0];
    let v = load_registry();
    let p = portals(&v)
        .get(key)
        .unwrap_or_else(|| fail(&format!("unknown portal '{}'", key)));
    let status = p.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let auth = p.get("auth").and_then(|s| s.as_str()).unwrap_or("password");
    if status == "verified-working" {
        println!(
            "{}",
            serde_json::json!({"portal": key, "action": "sign-in-directly",
                "reason": "registry verified-working"})
        );
        return;
    }
    if auth.starts_with("magic-link") {
        println!(
            "{}",
            serde_json::json!({"portal": key, "action": "request-magic-link-once",
                "reason": "magic-link auth; coordinator fetches link from Gmail"})
        );
        return;
    }
    // password portal, not verified: defer to fresh-link check done by caller
    // (kept separate so crons can log the check independently).
    println!(
        "{}",
        serde_json::json!({"portal": key, "action": "run-fresh-link-check",
            "reason": "not verified; check Gmail before requesting a reset",
            "hint": format!("portal-tool fresh-link {}", key)})
    );
}
