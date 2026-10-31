//! POST /browser/batch — run N browser actions in one HTTP round trip.
//!
//! Additive API surface: each step names an `action` plus its `params`;
//! the dispatcher delegates to the EXISTING route handlers, so behavior,
//! validation, and security checks are identical to calling the endpoints
//! individually. `abort_on_error` (default true) stops at the first step
//! whose response body has `ok: false`.
//!
//! SECURITY: admin-only actions (`eval`, `script`) are NOT batchable.
//! Letting an ACT-scoped key smuggle arbitrary JS through a batch would
//! be a privilege escalation (auth.rs: eval/script require Admin).

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::routes::{advanced, browser, input, read};
use crate::state::SharedState;

/// One batch step: an action name plus its params object.
#[derive(Deserialize)]
pub struct BatchStep {
    pub action: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Deserialize)]
pub struct BatchReq {
    pub steps: Vec<BatchStep>,
    /// Stop at the first step with `ok: false` (default true).
    #[serde(default = "default_true")]
    pub abort_on_error: bool,
}

fn default_true() -> bool {
    true
}

/// Actions the batch dispatcher accepts. Deliberately excludes
/// `eval`/`script` (Admin scope) — see module docs.
const BATCHABLE: &[&str] = &[
    "navigate",
    "click",
    "fill",
    "type",
    "press",
    "scroll",
    "wait",
    "drag",
    "double_click",
    "snapshot",
    "text",
    "html",
    "order",
];

/// Convert a handler response into its JSON body.
async fn resp_json(resp: axum::response::Response) -> Value {
    match axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024).await {
        Ok(b) => serde_json::from_slice(&b)
            .unwrap_or_else(|_| json!({"ok": false, "error": "non-JSON batch step response"})),
        Err(e) => json!({"ok": false, "error": format!("batch step body: {e}")}),
    }
}

/// Run one step by delegating to the existing route handler.
async fn run_step(state: &SharedState, step: &BatchStep) -> Value {
    let p = &step.params;
    let s = |k: &str| {
        p.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let b = |k: &str| p.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let resp = match step.action.as_str() {
        "navigate" => browser::navigate(
            State(state.clone()),
            Json(browser::NavigateRequest {
                url: s("url"),
                wait_until: s("wait_until"),
            }),
        )
        .await
        .into_response(),
        "click" => input::click(
            State(state.clone()),
            Json(input::ClickReq {
                selector: s("selector"),
                visible: b("visible"),
            }),
        )
        .await
        .into_response(),
        "fill" => input::fill(
            State(state.clone()),
            Json(input::FillReq {
                selector: s("selector"),
                text: s("text"),
                visible: b("visible"),
            }),
        )
        .await
        .into_response(),
        "type" => input::type_text(
            State(state.clone()),
            Json(input::FillReq {
                selector: s("selector"),
                text: s("text"),
                visible: b("visible"),
            }),
        )
        .await
        .into_response(),
        "press" => input::press(
            State(state.clone()),
            Json(input::PressReq {
                selector: s("selector"),
                key: {
                    let k = s("key");
                    if k.is_empty() {
                        "Enter".to_string()
                    } else {
                        k
                    }
                },
            }),
        )
        .await
        .into_response(),
        "scroll" => input::scroll(
            State(state.clone()),
            Json(input::ScrollReq {
                direction: {
                    let d = s("direction");
                    if d.is_empty() {
                        "down".to_string()
                    } else {
                        d
                    }
                },
                amount: p.get("amount").and_then(|v| v.as_i64()).unwrap_or(600),
            }),
        )
        .await
        .into_response(),
        "wait" => input::wait(
            State(state.clone()),
            Json(input::WaitReq {
                selector: s("selector"),
                timeout_ms: p
                    .get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10_000),
            }),
        )
        .await
        .into_response(),
        "drag" => {
            let points: Vec<Vec<f64>> = p
                .get("points")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            input::drag(
                State(state.clone()),
                Json(input::DragReq {
                    points,
                    draw_speed: p.get("draw_speed").and_then(|v| v.as_f64()).unwrap_or(1.0),
                    simplify: b("simplify"),
                }),
            )
            .await
            .into_response()
        }
        "double_click" => input::double_click(
            State(state.clone()),
            Json(input::ClickScreenReq {
                x: p.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
                y: p.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
            }),
        )
        .await
        .into_response(),
        "snapshot" => read::snapshot(
            State(state.clone()),
            Query(read::SnapshotQuery {
                source: p
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string()),
            }),
        )
        .await
        .into_response(),
        "text" => read::text(State(state.clone())).await.into_response(),
        "html" => read::html(State(state.clone())).await.into_response(),
        "order" => advanced::order(
            State(state.clone()),
            Json(advanced::OrderRequest {
                action: s("action"),
                text: s("text"),
                role: s("role"),
                value: s("value"),
                timeout_ms: p
                    .get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10_000),
                visible: b("visible"),
            }),
        )
        .await
        .into_response(),
        other => {
            return json!({"ok": false, "error": format!("unknown batch action: {other}"),
                "batchable": BATCHABLE});
        }
    };
    resp_json(resp).await
}

/// POST /browser/batch — {steps: [{action, params}], abort_on_error?}.
pub async fn batch(
    State(state): State<SharedState>,
    Json(req): Json<BatchReq>,
) -> impl IntoResponse {
    if req.steps.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "steps is required and must not be empty"})),
        )
            .into_response();
    }
    if req.steps.len() > 50 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "at most 50 steps per batch"})),
        )
            .into_response();
    }
    let mut results: Vec<Value> = Vec::with_capacity(req.steps.len());
    let mut aborted = false;
    for step in &req.steps {
        let r = run_step(&state, step).await;
        let failed = r.get("ok").and_then(|v| v.as_bool()).is_some_and(|ok| !ok);
        results.push(json!({"action": step.action, "result": r}));
        if failed && req.abort_on_error {
            aborted = true;
            break;
        }
    }
    (
        StatusCode::OK,
        Json(json!({"ok": true, "aborted": aborted, "results": results})),
    )
        .into_response()
}
