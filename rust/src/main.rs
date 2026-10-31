//! vm-bridge 2.0: Rust rewrite of chromebook-vm-bridge.
//!
//! Single static binary. Binds 127.0.0.1 by default; never expose without
//! an authenticated tunnel (audit ARCH-1).

mod auth;
mod browser;
mod config;
mod hud;
mod input;
mod interact;
mod leases;
mod mcp;
mod middleware;
mod perception;
mod playbooks;
mod pool;
mod privy;
mod routes;
mod state;
mod vault;
mod watchdog;

use crate::config::Config;
use crate::state::{BridgeState, SharedState};
use axum::{
    extract::State,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // JSON logging to stdout (journald picks it up)
    tracing_subscriber::registry()
        .with(fmt::layer().json())
        .with(EnvFilter::from_default_env())
        .init();

    let config = Config::load()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        bind = format!("{}:{}", config.bind_addr, config.bind_port),
        "vm-bridge starting"
    );

    let state: SharedState = Arc::new(BridgeState::new(config.clone()));

    // Ensure artifact dirs exist with mode 700 (audit SEC-8)
    for dir in [&config.failure_dir, &config.trace_dir] {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(dir)?.permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
            std::fs::set_permissions(dir, perms)?;
        }
    }

    // Spawn watchdog (audit BUG-3)
    let wd_state = state.clone();
    tokio::spawn(async move {
        watchdog::run(wd_state).await;
    });

    // Build router. Layer order (outermost first): rate_limit → auth → body_cap.
    // NO OpenAPI docs exposed (audit: docs were disabled in Python too).
    let app = Router::new()
        // Health + metrics
        .route("/health", get(routes::ops::health))
        .route("/metrics", get(routes::ops::metrics))
        // Browser routes
        .route("/browser/navigate", post(routes::browser::navigate))
        .route("/browser/text", get(routes::browser::text))
        .route("/browser/html", get(routes::browser::html))
        .route("/browser/snapshot", get(routes::browser::snapshot))
        .route("/browser/state", get(routes::browser::state))
        .route("/browser/tabs", get(routes::browser::tabs))
        .route("/browser/screenshot", get(routes::browser::screenshot))
        .route(
            "/browser/screenshot/hash",
            get(routes::browser::screenshot_hash),
        )
        .route("/browser/click", post(routes::browser::click))
        .route("/browser/click_at", post(routes::browser::click_at))
        .route("/browser/click_node", post(routes::browser::click_node))
        .route("/browser/click_screen", post(routes::browser::click_screen))
        .route("/browser/double_click", post(routes::browser::double_click))
        .route("/browser/drag", post(routes::browser::drag))
        .route("/browser/move_to", post(routes::browser::move_to))
        .route(
            "/browser/move_to_viewport",
            post(routes::browser::move_to_viewport),
        )
        .route("/browser/scroll", post(routes::browser::scroll))
        .route("/browser/fill", post(routes::browser::fill))
        .route("/browser/type", post(routes::browser::type_text))
        .route("/browser/type_verified", post(routes::browser::type_verified))
        .route("/browser/press", post(routes::browser::press))
        .route("/browser/wait", post(routes::browser::wait))
        .route("/browser/eval", post(routes::browser::eval))
        .route("/browser/script", post(routes::browser::script))
        .route("/browser/order", post(routes::browser::order))
        .route("/browser/autocomplete", post(routes::browser::autocomplete))
        .route(
            "/browser/geometry/refresh",
            post(routes::browser::geometry_refresh),
        )
        .route("/browser/assert_visual", post(routes::browser::assert_visual))
        .route(
            "/browser/screenshot/compare",
            post(routes::browser::screenshot_compare),
        )
        .route(
            "/browser/screenshot/delta",
            post(routes::browser::screenshot_delta),
        )
        .route(
            "/browser/screenshot/region",
            post(routes::browser::screenshot_region),
        )
        .route("/browser/tab/new", post(routes::browser::tab_new))
        .route("/browser/tab/switch", post(routes::browser::tab_switch))
        // New deterministic endpoints (Phase 3)
        .route("/browser/upload", post(routes::browser::upload))
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
        // Layers: outermost first
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
            auth::auth_middleware,
        ))
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::rate_limit_middleware,
        ))
        .with_state(state.clone());

    // Graceful shutdown (audit BUG-4: no orphan Chrome on exit)
    let shutdown_state = state.clone();
    let graceful = async move {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl-c handler");
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
