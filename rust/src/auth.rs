//! Scoped API key authentication (audit SEC-2, SEC-4).
//!
//! Three scopes: READ (GETs), ACT (interactions), ADMIN (eval, pool/eval, ops).
//! Comparison uses constant-time byte equality (no non-ASCII 500 crash —
//! headers are bytes natively in Rust). Rate limiting runs BEFORE auth
//! (see middleware.rs) so brute-force attempts consume tokens.

use crate::state::SharedState;
use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use subtle::ConstantTimeEq;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Act,
    Admin,
}

impl Scope {
    /// Minimum scope required for a (method, path-template) pair.
    pub fn required_for(method: &str, template: &str) -> Self {
        // Admin: arbitrary code execution + ops
        if template.starts_with("/browser/eval")
            || template.starts_with("/browser/script")
            || template.starts_with("/pool/eval")
            || template.starts_with("/ops/")
        {
            return Scope::Admin;
        }
        // Read: safe GETs
        if method == "GET" {
            return Scope::Read;
        }
        // Everything else mutating needs ACT
        Scope::Act
    }
}

/// Extract the required scope from request extensions (set by route templates).
fn required_scope(req: &Request<axum::body::Body>) -> Scope {
    let method = req.method().as_str();
    // axum MatchedPath gives us the template, not the raw path
    let template = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str())
        .unwrap_or(req.uri().path());
    Scope::required_for(method, template)
}

pub async fn auth_middleware(
    State(state): State<SharedState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // /health is exempt (but redacted — see routes/ops.rs)
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }

    let got: Vec<u8> = req
        .headers()
        .get("x-api-key")
        .map(|v| v.as_bytes().to_vec())
        .unwrap_or_default();

    let scope = required_scope(&req);
    let keys = &state.config.keys;

    // Check the required scope's key, then escalate: ADMIN satisfies all.
    let ok = match scope {
        Scope::Read => {
            ct_eq(&got, keys.read.as_bytes())
                || ct_eq(&got, keys.act.as_bytes())
                || ct_eq(&got, keys.admin.as_bytes())
        }
        Scope::Act => ct_eq(&got, keys.act.as_bytes()) || ct_eq(&got, keys.admin.as_bytes()),
        Scope::Admin => ct_eq(&got, keys.admin.as_bytes()),
    };

    if !ok {
        // Log the rejection (audit SEC-4: invisible 401s) — never log the key
        tracing::warn!(
            path = req.uri().path(),
            scope = ?scope,
            "authentication rejected"
        );
        state.metrics.lock().await.rejections_401 += 1;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "unauthorized"})),
        )
            .into_response();
    }

    next.run(req).await
}

/// Constant-time byte comparison. Empty `got` never matches.
fn ct_eq(got: &[u8], expected: &[u8]) -> bool {
    !got.is_empty() && bool::from(got.ct_eq(expected))
}
