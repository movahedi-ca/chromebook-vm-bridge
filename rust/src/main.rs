//! vm-bridge 2.0: Rust rewrite of chromebook-vm-bridge.
//!
//! Single static binary. Binds 127.0.0.1 by default; never expose without
//! an authenticated tunnel (audit ARCH-1).

mod auth;
mod browser;
mod config;
mod input;
mod interact;
mod leases;
mod mcp;
mod middleware;
mod perception;
mod pool;
mod privy;
mod routes;
mod state;
mod vault;
mod watchdog;

use crate::config::Config;
use crate::state::{BridgeState, SharedState};
use axum::{
    http::StatusCode,
    middleware as axum_mw,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[allow(clippy::too_many_lines)] // main() is linear startup wiring: config -> dirs -> watchdog -> supervisor -> routes
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // JSON logging to stdout (journald picks it up)
    // NOTE: in --mcp-stdio mode, logs go to stderr to keep stdout clean for JSON-RPC
    let mcp_stdio = std::env::args().any(|a| a == "--mcp-stdio");
    if mcp_stdio {
        tracing_subscriber::registry()
            .with(fmt::layer().json().with_writer(std::io::stderr))
            .with(EnvFilter::from_default_env())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(fmt::layer().json())
            .with(EnvFilter::from_default_env())
            .init();
    }

    let config = Config::load()?;
    let state: SharedState = Arc::new(BridgeState::new(config.clone()));

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        bind = format!("{}:{}", config.bind_addr, config.bind_port),
        "vm-bridge starting"
    );

    // Ensure artifact dirs exist with mode 700 (audit SEC-8).
    // Upload staging dirs ride along here so the HTTP upload route and the
    // MCP upload tool share one allowlist: config.upload_dirs (audit 41
    // MED-6). The HTTP route used to hardcode ~/jobs/uploads while MCP
    // read the config knob — a split-brain allowlist.
    {
        let mut dirs: Vec<&std::path::Path> =
            vec![config.failure_dir.as_path(), config.trace_dir.as_path()];
        dirs.extend(config.upload_dirs.iter().map(std::path::PathBuf::as_path));
        for dir in dirs {
            std::fs::create_dir_all(dir)?;
            // Best-effort mode 700 (audit SEC-8): under systemd's
            // ProtectHome=read-only + ReadWritePaths sandbox, chmod on the
            // writable overlay can return EROFS. The service must start;
            // a non-700 dir is logged, not fatal.
            #[cfg(unix)]
            {
                let mut perms = std::fs::metadata(dir)?.permissions();
                std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
                if let Err(e) = std::fs::set_permissions(dir, perms) {
                    tracing::warn!(path = %dir.display(), error = %e, "could not set 700 on artifact dir");
                }
            }
        }
    }

    // Spawn watchdog (audit BUG-3)
    let wd_state = state.clone();
    tokio::spawn(async move {
        watchdog::run(wd_state).await;
    });

    // Spawn the browser supervisor: the bridge owns exactly one browser
    // daemon. It spawns the engine at startup, connects the CDP driver,
    // watches the child, and relaunches with capped backoff when it dies.
    // The engine behind it is swappable via BrowserBackend (see
    // browser/backend.rs); construct a different backend here to swap.
    let sup_state = state.clone();
    tokio::spawn(async move {
        let backend: Box<dyn crate::browser::backend::BrowserBackend> =
            Box::new(crate::browser::backend_chrome::ChromeBackend::new());
        crate::browser::supervise::run_supervisor(backend, sup_state).await;
    });

    // MCP stdio mode: skip the HTTP server, serve MCP directly. The browser
    // supervisor + watchdog above MUST already be running: every browser MCP
    // tool delegates to the same route handlers as the HTTP API (audit 41
    // HIGH-1 — the early return used to sit above the spawns, so in
    // --mcp-stdio mode no browser ever launched and every browser tool
    // hung 60s in ensure_browser then died).
    if mcp_stdio {
        tracing::info!("starting in MCP stdio mode");
        // No HTTP listener in this mode; signal readiness directly (LOW-5).
        crate::watchdog::sd_notify("READY=1");
        return crate::mcp::serve_stdio(state).await;
    }

    // Build router. Layer order (outermost first): rate_limit → auth → body_cap.
    // NO OpenAPI docs exposed (audit: docs stay disabled).
    let app = Router::new()
        // Health + metrics
        .route("/health", get(routes::ops::health))
        .route("/metrics", get(routes::ops::metrics))
        // Browser routes
        .route("/browser/navigate", post(routes::browser::navigate))
        .route("/browser/text", get(routes::read::text))
        .route("/browser/html", get(routes::read::html))
        .route("/browser/snapshot", get(routes::read::snapshot))
        .route("/browser/state", get(routes::read::state))
        .route("/browser/tabs", get(routes::read::tabs))
        .route("/browser/screenshot", get(routes::read::screenshot))
        .route(
            "/browser/screenshot/hash",
            get(routes::read::screenshot_hash),
        )
        .route("/browser/click", post(routes::input::click))
        .route("/browser/click_at", post(routes::input::click_at))
        .route("/browser/click_node", post(routes::input::click_node))
        .route("/browser/click_screen", post(routes::input::click_screen))
        .route("/browser/double_click", post(routes::input::double_click))
        .route("/browser/drag", post(routes::input::drag))
        .route("/browser/move_to", post(routes::input::move_to))
        .route(
            "/browser/move_to_viewport",
            post(routes::input::move_to_viewport),
        )
        .route("/browser/scroll", post(routes::input::scroll))
        .route("/browser/fill", post(routes::input::fill))
        .route("/browser/type", post(routes::input::type_text))
        .route("/browser/type_verified", post(routes::input::type_verified))
        .route("/browser/press", post(routes::input::press))
        .route("/browser/wait", post(routes::input::wait))
        .route("/browser/eval", post(routes::input::eval))
        .route("/browser/script", post(routes::advanced::script))
        .route("/browser/order", post(routes::advanced::order))
        .route(
            "/browser/autocomplete",
            post(routes::advanced::autocomplete),
        )
        .route(
            "/browser/geometry/refresh",
            post(routes::read::geometry_refresh),
        )
        .route(
            "/browser/assert_visual",
            post(routes::advanced::assert_visual),
        )
        .route(
            "/browser/screenshot/compare",
            post(routes::advanced::screenshot_compare),
        )
        .route(
            "/browser/screenshot/delta",
            post(routes::advanced::screenshot_delta),
        )
        .route(
            "/browser/screenshot/region",
            post(routes::advanced::screenshot_region),
        )
        .route("/browser/tab/new", post(routes::read::tab_new))
        .route("/browser/tab/switch", post(routes::read::tab_switch))
        // New deterministic endpoints (Phase 3)
        .route("/browser/upload", post(routes::advanced::upload))
        .route("/browser/fill_react", post(routes::browser::fill_react))
        .route("/browser/fill_date", post(routes::browser::fill_date))
        .route("/browser/paste_atomic", post(routes::browser::paste_atomic))
        .route(
            "/browser/restore_checkpoint",
            post(routes::browser::restore_checkpoint),
        )
        .route(
            "/browser/som_screenshot",
            post(routes::browser::som_screenshot),
        )
        .route("/browser/diff_state", post(routes::browser::diff_state))
        .route("/browser/vault_login", post(routes::browser::vault_login))
        // Pool routes
        .route("/pool/acquire", post(routes::pool::acquire))
        .route("/pool/release", post(routes::pool::release))
        .route("/pool/navigate", post(routes::pool::navigate))
        .route("/pool/click", post(routes::pool::click))
        .route("/pool/fill", post(routes::pool::fill))
        .route("/pool/eval", post(routes::pool::eval))
        .route("/pool/order", post(routes::pool::order))
        .route("/pool/snapshot", post(routes::pool::snapshot))
        .route("/pool/text", get(routes::pool::text))
        .route("/pool/status", get(routes::pool::status))
        .route("/pool/speculate", post(routes::pool::speculate))
        // Lease routes
        .route("/lease/acquire", post(routes::lease::acquire))
        .route("/lease/renew", post(routes::lease::renew))
        .route("/lease/release", post(routes::lease::release))
        .route("/lease/status", get(routes::lease::status))
        // Ops routes
        .route("/ops/render_package", post(routes::ops::render_package))
        // Honest JSON 404 for unmatched routes (audit MED-6)
        .fallback(fallback_404)
        // Layers: registered innermost-first (axum applies the LAST
        // .layer() call as the outermost wrapper).
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::log_middleware,
        ))
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::body_cap_middleware,
        ))
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::lease_middleware,
        ))
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::rate_limit_middleware,
        ))
        // Outermost (registered last): hard bound on total request time
        // (audit HIGH-5). No request — handler, middleware, or supervisor
        // wait — may run longer than this; timeouts surface as HTTP 408.
        .layer(tower_http::timeout::TimeoutLayer::new(Duration::from_secs(
            120,
        )))
        .with_state(state.clone());

    // Graceful shutdown (audit BUG-4: no orphan Chrome on exit).
    // systemd sends SIGTERM on stop/restart — handle it alongside
    // Ctrl-C (audit MED-1), otherwise the 20s drain below is skipped.
    let shutdown_state = state.clone();
    let graceful = async move {
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        #[cfg(unix)]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutdown signal received");
        shutdown_state
            .shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Give in-flight requests 20s to finish
        tokio::time::sleep(Duration::from_secs(20)).await;
    };

    let addr = format!("{}:{}", config.bind_addr, config.bind_port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("listening on {}", addr);
    // Readiness is "serving", not "process started": notify systemd only
    // after the listener binds (audit 41 LOW-5). The watchdog keeps sending
    // periodic WATCHDOG=1 pings from its loop.
    crate::watchdog::sd_notify("READY=1");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(graceful)
    .await?;

    Ok(())
}

/// Fallback for unmatched routes: honest 404 JSON (not HTML).
async fn fallback_404() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"ok": false, "error": "not_found"})),
    )
}
