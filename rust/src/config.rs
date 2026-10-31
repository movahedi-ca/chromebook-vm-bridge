//! Typed configuration for vm-bridge.
//!
//! All values come from environment variables (with `VM_BRIDGE_` prefix)
//! or `~/.config/vm-bridge/config.toml`. The server **fails fast at startup**
//! if required API keys are unset (audit SEC-4). No hardcoded `/home/auto`
//! paths (audit §5.3.7); paths resolve via `dirs` with env overrides.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Bind address (default 127.0.0.1 — never expose without a tunnel)
    pub bind_addr: String,
    /// Bind port (default 8899; Rust dev instance uses 8900)
    pub bind_port: u16,
    /// Unix domain socket path (optional second listener)
    pub unix_socket: Option<PathBuf>,

    /// Scoped API keys. Server refuses to start if any required key is unset.
    pub keys: ApiKeys,

    /// X11 display for the headed browser (default :1)
    pub display: String,
    /// Chrome binary path
    pub chrome_bin: String,
    /// Dedicated automation profile dir (NOT the daily profile — audit SEC-2)
    pub profile_dir: PathBuf,

    /// Watchdog probe interval / timeout seconds
    pub watchdog_interval_s: u64,
    pub watchdog_probe_timeout_s: u64,

    /// Body limits
    pub max_body_bytes: usize,
    pub max_decompressed_bytes: usize,

    /// Artifact retention
    pub failure_dir: PathBuf,
    pub trace_dir: PathBuf,
    pub max_artifacts: usize,
    pub artifact_ttl_hours: u64,

    /// Upload staging allowlist
    pub upload_dirs: Vec<PathBuf>,

    /// FormGuard vault path
    pub vault_path: PathBuf,

    /// Playbook dir
    pub playbook_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeys {
    /// Read-only: GET endpoints
    pub read: String,
    /// Interactions: clicks, fills, navigation
    pub act: String,
    /// Admin: /browser/eval, /pool/eval, /ops/*
    pub admin: String,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let cfg_dir = dirs::config_dir()
            .map(|d| d.join("vm-bridge"))
            .unwrap_or_else(|| PathBuf::from("."));

        let mut builder = config::Config::builder();

        // Defaults
        builder = builder
            .set_default("bind_addr", "127.0.0.1")?
            .set_default("bind_port", 8899)?
            .set_default("display", ":1")?
            .set_default("chrome_bin", "google-chrome-stable")?
            .set_default("watchdog_interval_s", 10)?
            .set_default("watchdog_probe_timeout_s", 3)?
            .set_default("max_body_bytes", 8 * 1024 * 1024)?
            .set_default("max_decompressed_bytes", 8 * 1024 * 1024)?
            .set_default("max_artifacts", 50)?
            .set_default("artifact_ttl_hours", 24)?;

        // Resolve paths (no hardcoded /home/auto)
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        builder = builder
            .set_default("profile_dir", home.join(".config/chrome-automation").to_string_lossy().to_string())?
            .set_default("failure_dir", home.join("jobs/vm-bridge/failures").to_string_lossy().to_string())?
            .set_default("trace_dir", home.join("jobs/vm-bridge/traces").to_string_lossy().to_string())?
            .set_default("vault_path", cfg_dir.join("form_vault.db").to_string_lossy().to_string())?
            .set_default("playbook_dir", cfg_dir.join("playbooks").to_string_lossy().to_string())?
            .set_default("upload_dirs", vec![home.join("jobs/uploads").to_string_lossy().to_string()])?;

        // Optional TOML file
        let toml_path = cfg_dir.join("config.toml");
        if toml_path.exists() {
            builder = builder.add_source(config::File::from(toml_path));
        }

        // Env overrides: VM_BRIDGE_BIND_PORT, VM_BRIDGE_KEYS__READ, etc.
        builder = builder.add_source(
            config::Environment::with_prefix("VM_BRIDGE")
                .separator("__")
                .try_parsing(true),
        );

        let cfg: Config = builder.build()?.try_deserialize()?;

        // Fail fast on missing keys (audit SEC-4)
        for (name, key) in [
            ("read", &cfg.keys.read),
            ("act", &cfg.keys.act),
            ("admin", &cfg.keys.admin),
        ] {
            if key.trim().is_empty() {
                anyhow::bail!(
                    "VM_BRIDGE_KEYS__{} is unset or empty — refusing to start (audit SEC-4)",
                    name.to_uppercase()
                );
            }
            if key.len() < 32 {
                tracing::warn!("API key '{}' is shorter than 32 chars", name);
            }
        }

        Ok(cfg)
    }
}
