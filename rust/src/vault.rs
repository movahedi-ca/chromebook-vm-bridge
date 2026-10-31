//! FormGuard: transactional form checkpoint vault (playbook §11.2).
//!
//! A tiny JS listener (installed via `Page.addScriptToEvaluateOnNewDocument`)
//! streams committed field states into this SQLite vault (mode 0600).
//! After a crash or reload, `/browser/restore_checkpoint` replays them.
//!
//! Schema:
//!   checkpoints(origin, path, step, field_key, label, value, updated_at)
//!   PRIMARY KEY (origin, path, step, field_key)

#![allow(dead_code)] // parked: FormGuard not yet wired to any route (audits HIGH-2, LOW-1)
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// JS installed on every new document. Captures change/blur on inputs,
/// textareas, selects, and ARIA comboboxes/listboxes. POSTs back to
/// the bridge via fetch to /internal/checkpoint (internal only).
pub const FORMGUARD_JS: &str = r#"
(function() {
  if (window.__formguard_installed) return;
  window.__formguard_installed = true;
  const SEL = 'input, textarea, select, [role="combobox"], [role="listbox"]';
  function key(el) {
    return el.getAttribute('aria-label') || el.name || el.id ||
           (el.className && el.className.baseVal === undefined ? el.className : '') || el.tagName;
  }
  function capture(el) {
    if (!el || !el.isConnected) return;
    // Never persist secret-classified controls, even if a selector
    // matches (audit HIGH-1). Passwords, OTP/PIN fields, and payment
    // card inputs are skipped entirely — they are never written to the
    // vault, so a failed login cannot be "replayed" from stored secrets.
    const t = (el.getAttribute('type') || '').toLowerCase();
    const ac = (el.getAttribute('autocomplete') || '').toLowerCase();
    if (t === 'password' ||
        (t === 'hidden' && el.name && /pass|secret|token|otp|pin/i.test(el.name)) ||
        ac.includes('password') || ac.startsWith('cc-') ||
        ac.includes('one-time-code')) return;
    const step = (document.querySelector('h1,h2') || {}).textContent || '';
    const payload = {
      origin: location.origin, path: location.pathname,
      step: step.trim().slice(0, 120),
      field_key: key(el).slice(0, 200),
      label: (el.getAttribute('aria-label') || el.name || '').slice(0, 200),
      value: (el.value !== undefined ? el.value : el.textContent || '').slice(0, 4000),
    };
    fetch('/internal/checkpoint', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify(payload),
    }).catch(() => {});
  }
  document.addEventListener('change', e => { if (e.target.matches(SEL)) capture(e.target); }, true);
  document.addEventListener('blur', e => { if (e.target.matches(SEL)) capture(e.target); }, true);
})();
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub origin: String,
    pub path: String,
    pub step: String,
    pub field_key: String,
    pub label: String,
    pub value: String,
}

pub struct Vault {
    conn: Mutex<Connection>,
}

impl Vault {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // Mode 0600: checkpoint data may contain PII
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(path)?.permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o600);
            std::fs::set_permissions(path, perms)?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS checkpoints(
               origin TEXT, path TEXT, step TEXT, field_key TEXT,
               label TEXT, value TEXT, updated_at TEXT DEFAULT (datetime('now')),
               PRIMARY KEY (origin, path, step, field_key)
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn save(&self, cp: &Checkpoint) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO checkpoints(origin,path,step,field_key,label,value)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(origin,path,step,field_key)
             DO UPDATE SET label=excluded.label, value=excluded.value,
                           updated_at=datetime('now')",
            params![
                cp.origin,
                cp.path,
                cp.step,
                cp.field_key,
                cp.label,
                cp.value
            ],
        )?;
        Ok(())
    }

    pub fn load(&self, origin: &str, path: &str, step: &str) -> anyhow::Result<Vec<Checkpoint>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT origin,path,step,field_key,label,value FROM checkpoints
             WHERE origin=?1 AND path=?2 AND step=?3 ORDER BY updated_at",
        )?;
        let rows = stmt.query_map(params![origin, path, step], |row| {
            Ok(Checkpoint {
                origin: row.get(0)?,
                path: row.get(1)?,
                step: row.get(2)?,
                field_key: row.get(3)?,
                label: row.get(4)?,
                value: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }

    /// Prune checkpoints older than `max_age_days` (default 30).
    pub fn prune(&self, max_age_days: i64) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM checkpoints WHERE updated_at < datetime('now', ?1)",
            params![format!("-{} days", max_age_days)],
        )?;
        Ok(n)
    }
}
