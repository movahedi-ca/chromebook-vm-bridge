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

    /// Scoped API keys. Server refuses to start if any required key is unset.
    /// Falls back to `BRIDGE_API_KEY` when unset (see `Config::load`).
    #[serde(default)]
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
    #[allow(dead_code)] // retention enforcement not built yet
    pub max_artifacts: usize,
    #[allow(dead_code)] // retention enforcement not built yet
    pub artifact_ttl_hours: u64,

    /// CDP click timing profile: "normal" (30/40 ms — current behavior) or
    /// "fast" (5/10 ms). Env: VM_BRIDGE_INTERACTION_PROFILE.
    #[serde(default = "default_interaction_profile")]
    pub interaction_profile: String,

    /// Opt-in URL patterns blocked inside Chromium's network service via
    /// `Network.setBlockedURLs` (e.g. ad/tracker hosts for faster page
    /// loads). Empty (default) = off. Env:
    /// `VM_BRIDGE_BLOCKED_RESOURCE_PATTERNS` (comma-separated).
    #[serde(default)]
    pub blocked_resource_patterns: Vec<String>,

    /// Upload staging allowlist
    pub upload_dirs: Vec<PathBuf>,

    /// `FormGuard` vault path
    #[allow(dead_code)] // vault is parked (audit HIGH-2)
    pub vault_path: PathBuf,

    /// Playbook dir
    #[allow(dead_code)] // playbook engine not built yet
    pub playbook_dir: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiKeys {
    /// Read-only: GET endpoints
    #[serde(default)]
    pub read: String,
    /// Interactions: clicks, fills, navigation
    #[serde(default)]
    pub act: String,
    /// Admin: /browser/eval, /pool/eval, /ops/*
    #[serde(default)]
    pub admin: String,
}

/// Default `interaction_profile`: "normal" preserves the historical
/// 30/40 ms CDP click sleeps exactly.
fn default_interaction_profile() -> String {
    "normal".to_string()
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let mut cfg: Config = Self::build_base_config()?;
        Self::resolve_key_layout(&mut cfg)?;
        Self::check_bind_addr(&cfg)?;
        Ok(cfg)
    }

    /// Assemble defaults, resolved paths, the optional TOML file, and
    /// `VM_BRIDGE__*` env overrides into a deserialized `Config`.
    fn build_base_config() -> anyhow::Result<Self> {
        let cfg_dir =
            dirs::config_dir().map_or_else(|| PathBuf::from("."), |d| d.join("vm-bridge"));

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
            .set_default("artifact_ttl_hours", 24)?
            .set_default("interaction_profile", "normal")?
            .set_default("blocked_resource_patterns", Vec::<String>::new())?;

        // Resolve paths (no hardcoded /home/auto)
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        builder = builder
            .set_default(
                "profile_dir",
                home.join(".config/chrome-automation")
                    .to_string_lossy()
                    .to_string(),
            )?
            .set_default(
                "failure_dir",
                home.join("jobs/vm-bridge/failures")
                    .to_string_lossy()
                    .to_string(),
            )?
            .set_default(
                "trace_dir",
                home.join("jobs/vm-bridge/traces")
                    .to_string_lossy()
                    .to_string(),
            )?
            .set_default(
                "vault_path",
                cfg_dir.join("form_vault.db").to_string_lossy().to_string(),
            )?
            .set_default(
                "playbook_dir",
                cfg_dir.join("playbooks").to_string_lossy().to_string(),
            )?
            .set_default(
                "upload_dirs",
                vec![home.join("jobs/uploads").to_string_lossy().to_string()],
            )?;

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

        Ok(builder.build()?.try_deserialize::<Config>()?)
    }

    /// Resolve the API-key layout (audit 42 Medium-8): reject mixed
    /// provenance, fall back to `BRIDGE_API_KEY` only when no scoped key is
    /// set, and refuse missing or weak (< 32 char) keys at startup.
    fn resolve_key_layout(cfg: &mut Config) -> anyhow::Result<()> {
        // If the operator sets ANY scoped key, the
        // legacy BRIDGE_API_KEY must NOT silently fill the other scopes.
        // Mixed provenance is a misconfiguration (one scope strong, two
        // scopes accidentally sharing the legacy key) — fail startup and
        // say so, instead of running with a key layout nobody asked for.
        let scoped_env = [
            "VM_BRIDGE_KEYS__READ",
            "VM_BRIDGE_KEYS__ACT",
            "VM_BRIDGE_KEYS__ADMIN",
        ];
        let any_scoped = scoped_env
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()));
        if any_scoped {
            for (env_name, key) in [
                ("VM_BRIDGE_KEYS__READ", cfg.keys.read.trim()),
                ("VM_BRIDGE_KEYS__ACT", cfg.keys.act.trim()),
                ("VM_BRIDGE_KEYS__ADMIN", cfg.keys.admin.trim()),
            ] {
                if key.is_empty() {
                    anyhow::bail!(
                        "{env_name} is unset while other VM_BRIDGE_KEYS__* are set — \
                         refusing to fill it from BRIDGE_API_KEY (audit 42 Medium-8). \
                         Set all three scoped keys explicitly."
                    );
                }
            }
        } else if cfg.keys.read.trim().is_empty()
            || cfg.keys.act.trim().is_empty()
            || cfg.keys.admin.trim().is_empty()
        {
            // Single-key fallback: if the scoped keys are unset, accept the
            // legacy BRIDGE_API_KEY value for all three scopes. This makes the
            // Rust bridge a drop-in replacement for existing clients that send
            // one X-API-Key for everything — no client changes needed.
            // (Operators who want real scope separation set the three
            // VM_BRIDGE_KEYS__* values instead; those take precedence.)
            if let Ok(legacy) = std::env::var("BRIDGE_API_KEY") {
                let legacy = legacy.trim().to_string();
                // legacy is fanned out to three independent scope keys; the clones are necessary.
                #[allow(clippy::assigning_clones)]
                if !legacy.is_empty() {
                    if cfg.keys.read.trim().is_empty() {
                        cfg.keys.read = legacy.clone();
                    }
                    if cfg.keys.act.trim().is_empty() {
                        cfg.keys.act = legacy.clone();
                    }
                    if cfg.keys.admin.trim().is_empty() {
                        cfg.keys.admin = legacy.clone();
                    }
                    tracing::info!("using BRIDGE_API_KEY as fallback for unset scoped keys");
                }
            }
        }

        // Fail fast on missing keys (audit SEC-4), and on weak keys
        // (audit 42 Medium-8): anything under 32 chars is refused at
        // startup instead of running with a guessable bearer token.
        // (The production key was length-verified before this bail was
        // enforced; a shorter key fails loudly here rather than silently.)
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
            if key.trim().len() < 32 {
                anyhow::bail!(
                    "API key '{name}' is shorter than 32 chars — refusing to start (audit 42 Medium-8)"
                );
            }
        }

        Ok(())
    }

    /// Fail closed on a non-loopback bind address (audit 42 Medium-7).
    /// The bridge speaks plain HTTP and its API keys are bearer tokens;
    /// binding 0.0.0.0 by accident would expose it to the whole LAN.
    /// Override explicitly with `VM_BRIDGE_ALLOW_NON_LOOPBACK=1` only.
    fn check_bind_addr(cfg: &Config) -> anyhow::Result<()> {
        let bind = cfg.bind_addr.trim().to_lowercase();
        let is_loopback = bind == "127.0.0.1" || bind == "::1" || bind == "localhost";
        let non_loopback_allowed =
            std::env::var("VM_BRIDGE_ALLOW_NON_LOOPBACK").is_ok_and(|v| v.trim() == "1");
        if !is_loopback && !non_loopback_allowed {
            anyhow::bail!(
                "bind_addr '{bind}' is not loopback — refusing to start (audit 42 Medium-7). \
                 Set VM_BRIDGE_ALLOW_NON_LOOPBACK=1 to override explicitly."
            );
        }

        Ok(())
    }
}
