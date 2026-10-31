//! Lease route handlers (audit SEC-7).

use crate::leases::LeaseError;
use crate::state::SharedState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
pub struct AcquireRequest {
    #[serde(default = "default_client")]
    pub client: String,
    #[serde(default = "default_ttl")]
    pub ttl_s: i64,
}

fn default_client() -> String {
    "unknown".to_string()
}

fn default_ttl() -> i64 {
    300
}

#[derive(Debug, Deserialize)]
pub struct LeaseIdRequest {
    pub lease_id: String,
    #[serde(default = "default_ttl")]
    pub ttl_s: i64,
}

/// POST /lease/acquire — returns (lease_id, clamped_ttl_s).
/// 423 if another client holds the lock.
pub async fn acquire(
    State(state): State<SharedState>,
    Json(req): Json<AcquireRequest>,
) -> impl IntoResponse {
    let mut leases = state.leases.lock().await;
    match leases.acquire(req.client, req.ttl_s) {
        Ok((id, ttl)) => (
            StatusCode::OK,
            Json(json!({"ok": true, "lease_id": id, "ttl_s": ttl})),
        )
            .into_response(),
        Err(LeaseError::Locked { holder, ttl_left_s }) => {
            state.metrics.lock().await.rejections_423 += 1;
            (
                StatusCode::LOCKED,
                Json(json!({
                    "ok": false,
                    "error": "lease_locked",
                    "holder": holder,
                    "ttl_left_s": ttl_left_s
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /lease/renew — extend an existing lease.
pub async fn renew(
    State(state): State<SharedState>,
    Json(req): Json<LeaseIdRequest>,
) -> impl IntoResponse {
    let mut leases = state.leases.lock().await;
    match leases.renew(&req.lease_id, req.ttl_s) {
        Ok(ttl) => (
            StatusCode::OK,
            Json(json!({"ok": true, "ttl_s": ttl})),
        )
            .into_response(),
        Err(LeaseError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "lease_not_found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /lease/release — release a lease.
pub async fn release(
    State(state): State<SharedState>,
    Json(req): Json<LeaseIdRequest>,
) -> impl IntoResponse {
    let mut leases = state.leases.lock().await;
    if leases.release(&req.lease_id) {
        (StatusCode::OK, Json(json!({"ok": true}))).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "lease_not_found"})),
        )
            .into_response()
    }
}

/// GET /lease/status — lease_id REDACTED (audit SEC-7).
pub async fn status(State(state): State<SharedState>) -> impl IntoResponse {
    let mut leases = state.leases.lock().await;
    let status = leases.status();
    Json(json!({"ok": true, "lease": status})).into_response()
}
