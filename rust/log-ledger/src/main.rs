//! log-ledger: file-locked, schema-validated writer for program-ledger.csv.
//!
//! Usage:
//!   log-ledger check <ledger.csv>
//!   log-ledger set <ledger.csv> --run <id> [--state <s>] [--notes <t>]
//!               [--seconds <n>] [--submit-time <t>] [--req-id <t>]
//!
//! Schema: header `run,company,role,req_id,portal,submit_time_utc,seconds,
//! confirmation_state,notes`; run ids are unique integers; confirmation_state
//! is one of `submitted|blocked|shortfall|not-applied`.
//!
//! Concurrency: an exclusive lock file (`<ledger>.lock`, created with
//! O_CREAT|O_EXCL) guards every mutation. A stale lock whose PID is dead
//! is stolen. Writes are atomic (temp file + rename).

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HEADER: [&str; 9] = [
    "run",
    "company",
    "role",
    "req_id",
    "portal",
    "submit_time_utc",
    "seconds",
    "confirmation_state",
    "notes",
];
const STATES: [&str; 4] = ["submitted", "blocked", "shortfall", "not-applied"];

const COL_RUN: usize = 0;
const COL_SECONDS: usize = 6;
const COL_STATE: usize = 7;
const COL_NOTES: usize = 8;
const COL_SUBMIT_TIME: usize = 5;
const COL_REQ_ID: usize = 3;

/// Minimal CSV parser: handles quoted fields, doubled quotes, and newlines
/// inside quotes. Returns rows of fields.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' {
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
        } else if c == '\r' {
            // swallow; \n handles the break
        } else {
            field.push(c);
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // drop a single trailing empty row from a final newline
    if let Some(last) = rows.last() {
        if last.len() == 1 && last[0].is_empty() {
            rows.pop();
        }
    }
    rows
}

fn needs_quote(s: &str) -> bool {
    s.contains([',', '"', '\n', '\r'])
}

fn write_csv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        for (i, f) in row.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if needs_quote(f) {
                out.push('"');
                out.push_str(&f.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(f);
            }
        }
        out.push('\n');
    }
    out
}

#[derive(Debug, Default)]
struct Summary {
    rows: usize,
    counts: HashMap<String, usize>,
}

fn validate(rows: &[Vec<String>]) -> Result<Summary, Vec<String>> {
    let mut errs: Vec<String> = Vec::new();
    if rows.is_empty() {
        return Err(vec!["empty file: no header row".to_string()]);
    }
    let header: Vec<&str> = HEADER.to_vec();
    if rows[0] != header {
        errs.push(format!("bad header: {:?}", rows[0]));
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut summary = Summary::default();
    for (i, r) in rows.iter().enumerate().skip(1) {
        let lineno = i + 1;
        if r.len() != HEADER.len() {
            errs.push(format!(
                "line {lineno}: {} cols, want {}",
                r.len(),
                HEADER.len()
            ));
            continue;
        }
        match r[COL_RUN].trim().parse::<u32>() {
            Ok(id) => {
                if !seen.insert(id) {
                    errs.push(format!("line {lineno}: duplicate run id {id}"));
                }
            }
            Err(_) => errs.push(format!("line {lineno}: bad run id {:?}", r[COL_RUN])),
        }
        let st = r[COL_STATE].trim();
        if STATES.contains(&st) {
            *summary.counts.entry(st.to_string()).or_insert(0) += 1;
        } else {
            errs.push(format!("line {lineno}: bad state {st:?}"));
        }
        summary.rows += 1;
    }
    if errs.is_empty() {
        Ok(summary)
    } else {
        Err(errs)
    }
}

/// Exclusive lock via atomic create_new on `<ledger>.lock`.
/// Steals the lock when the recorded PID is dead. Guard removes it on drop.
struct LockGuard {
    path: PathBuf,
}

fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn acquire_lock(ledger: &Path) -> Result<LockGuard, String> {
    let path = PathBuf::from(format!("{}.lock", ledger.display()));
    let me = std::process::id();
    for _ in 0..150 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                let _ = writeln!(f, "{me}\n{}", now_secs());
                return Ok(LockGuard { path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = fs::read_to_string(&path)
                    .ok()
                    .and_then(|c| c.lines().next().and_then(|l| l.trim().parse::<u32>().ok()))
                    .map(|pid| !pid_alive(pid))
                    .unwrap_or(true);
                if stale {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("cannot create lock {}: {e}", path.display())),
        }
    }
    Err(format!(
        "timed out waiting for lock {} (another writer active?)",
        path.display()
    ))
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn print_usage() -> ! {
    eprintln!("usage:");
    eprintln!("  log-ledger check <ledger.csv>");
    eprintln!("  log-ledger set <ledger.csv> --run <id> [--state <s>] [--notes <t>] [--seconds <n>] [--submit-time <t>] [--req-id <t>]");
    eprintln!("states: submitted|blocked|shortfall|not-applied");
    std::process::exit(2);
}

fn cmd_check(ledger: &str) -> i32 {
    let text = match fs::read_to_string(ledger) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read {ledger}: {e}");
            return 1;
        }
    };
    match validate(&parse_csv(&text)) {
        Ok(s) => {
            println!(
                "OK: {} rows | submitted={} blocked={} shortfall={} not-applied={}",
                s.rows,
                s.counts.get("submitted").copied().unwrap_or(0),
                s.counts.get("blocked").copied().unwrap_or(0),
                s.counts.get("shortfall").copied().unwrap_or(0),
                s.counts.get("not-applied").copied().unwrap_or(0),
            );
            0
        }
        Err(errs) => {
            for e in errs {
                eprintln!("invalid: {e}");
            }
            1
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_set(
    ledger: &str,
    run_id: u32,
    state: Option<&str>,
    notes: Option<&str>,
    seconds: Option<&str>,
    submit_time: Option<&str>,
    req_id: Option<&str>,
) -> i32 {
    if let Some(s) = state {
        if !STATES.contains(&s) {
            eprintln!("bad state {s:?}; want one of {}", STATES.join("|"));
            return 1;
        }
    }
    if let Some(n) = seconds {
        if n.trim().parse::<u64>().is_err() {
            eprintln!("bad --seconds {n:?}; want an integer");
            return 1;
        }
    }
    let ledger_path = Path::new(ledger);
    let _guard = match acquire_lock(ledger_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let text = match fs::read_to_string(ledger_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read {ledger}: {e}");
            return 1;
        }
    };
    let mut rows = parse_csv(&text);
    if let Err(errs) = validate(&rows) {
        for e in errs {
            eprintln!("invalid before write: {e}");
        }
        return 1;
    }
    let pos = rows
        .iter()
        .skip(1)
        .position(|r| r[COL_RUN].trim().parse::<u32>().ok() == Some(run_id));
    let idx = match pos {
        Some(p) => p + 1,
        None => {
            eprintln!("run id {run_id} not found");
            return 1;
        }
    };
    let mut changes: Vec<String> = Vec::new();
    let mut apply = |col: usize, name: &str, val: &str| {
        let old = rows[idx][col].clone();
        if old != val {
            changes.push(format!("{name}: {old:?} -> {val:?}"));
            rows[idx][col] = val.to_string();
        }
    };
    if let Some(s) = state {
        apply(COL_STATE, "confirmation_state", s);
    }
    if let Some(n) = notes {
        apply(COL_NOTES, "notes", n);
    }
    if let Some(n) = seconds {
        apply(COL_SECONDS, "seconds", n.trim());
    }
    if let Some(t) = submit_time {
        apply(COL_SUBMIT_TIME, "submit_time_utc", t);
    }
    if let Some(r) = req_id {
        apply(COL_REQ_ID, "req_id", r);
    }
    if changes.is_empty() {
        println!("run {run_id}: no changes");
        return 0;
    }
    if let Err(errs) = validate(&rows) {
        for e in errs {
            eprintln!("invalid after edit: {e}");
        }
        return 1;
    }
    let tmp = format!("{ledger}.tmp");
    if let Err(e) = fs::write(&tmp, write_csv(&rows)) {
        eprintln!("write {tmp}: {e}");
        return 1;
    }
    // re-read what we wrote, then atomic rename
    match fs::read_to_string(&tmp) {
        Ok(t) => {
            if validate(&parse_csv(&t)).is_err() {
                eprintln!("refusing to install: written temp file failed validation");
                let _ = fs::remove_file(&tmp);
                return 1;
            }
        }
        Err(e) => {
            eprintln!("re-read {tmp}: {e}");
            return 1;
        }
    }
    if let Err(e) = fs::rename(&tmp, ledger_path) {
        eprintln!("rename: {e}");
        return 1;
    }
    println!("run {run_id}:");
    for c in changes {
        println!("  {c}");
    }
    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        print_usage();
    }
    let code = match args[1].as_str() {
        "check" => {
            if args.len() != 3 {
                print_usage();
            }
            cmd_check(&args[2])
        }
        "set" => {
            let mut run_id: Option<u32> = None;
            let mut state: Option<&str> = None;
            let mut notes: Option<&str> = None;
            let mut seconds: Option<&str> = None;
            let mut submit_time: Option<&str> = None;
            let mut req_id: Option<&str> = None;
            let mut i = 3;
            while i < args.len() {
                let key = args[i].as_str();
                let val = args.get(i + 1).map(String::as_str).unwrap_or_else(|| {
                    eprintln!("missing value for {key}");
                    print_usage();
                });
                match key {
                    "--run" => {
                        run_id = val.parse::<u32>().ok().or_else(|| {
                            eprintln!("bad --run {val:?}");
                            print_usage();
                        })
                    }
                    "--state" => state = Some(val),
                    "--notes" => notes = Some(val),
                    "--seconds" => seconds = Some(val),
                    "--submit-time" => submit_time = Some(val),
                    "--req-id" => req_id = Some(val),
                    _ => {
                        eprintln!("unknown flag {key}");
                        print_usage();
                    }
                }
                i += 2;
            }
            let run_id = run_id.unwrap_or_else(|| {
                eprintln!("--run is required");
                print_usage();
            });
            cmd_set(&args[2], run_id, state, notes, seconds, submit_time, req_id)
        }
        other => {
            eprintln!("unknown command {other:?}");
            print_usage();
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trip_with_quotes_and_newlines() {
        let rows = vec![
            HEADER.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            vec![
                "1".into(),
                "Acme, Inc.".into(),
                "Role \"Senior\"".into(),
                "R1".into(),
                "Workday".into(),
                "2026-11-02T00:00:00Z".into(),
                "60".into(),
                "submitted".into(),
                "line one\nline two, continued".into(),
            ],
        ];
        let text = write_csv(&rows);
        let back = parse_csv(&text);
        assert_eq!(back, rows);
    }

    #[test]
    fn validate_accepts_clean_ledger() {
        let rows = vec![
            HEADER.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            vec!["1", "A", "R", "1", "W", "t", "5", "submitted", "n"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            vec!["2", "B", "R", "2", "W", "t", "5", "not-applied", "n"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ];
        let s = validate(&rows).expect("should validate");
        assert_eq!(s.rows, 2);
        assert_eq!(s.counts["submitted"], 1);
    }

    #[test]
    fn validate_rejects_bad_state_and_dup_id() {
        let rows = vec![
            HEADER.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            vec!["1", "A", "R", "1", "W", "t", "5", "Submitted", "n"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            vec!["1", "B", "R", "2", "W", "t", "5", "blocked", "n"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ];
        let errs = validate(&rows).expect_err("should fail");
        assert_eq!(errs.len(), 2);
    }

    #[test]
    fn lock_acquire_release_and_stale_steal() {
        let dir = std::env::temp_dir().join(format!("logledger-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("l.csv");
        fs::write(&ledger, "x").unwrap();
        {
            let _g = acquire_lock(&ledger).expect("first acquire");
            assert!(Path::new(&format!("{}.lock", ledger.display())).exists());
        }
        assert!(!Path::new(&format!("{}.lock", ledger.display())).exists());
        // stale lock with a dead pid gets stolen
        let lockp = format!("{}.lock", ledger.display());
        fs::write(&lockp, "999999999\n1\n").unwrap();
        let _g = acquire_lock(&ledger).expect("stale steal");
        fs::remove_dir_all(&dir).unwrap();
    }
}
