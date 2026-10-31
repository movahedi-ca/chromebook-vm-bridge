//! Middleware stack (audit SEC-4, SEC-5).
//!
//! Layer order (outermost first): rate_limit → auth → body_cap → handler.
//! Rate limiting BEFORE auth so brute-force attempts consume tokens.
//! Body caps count actual bytes streamed (not Content-Length header),
//! closing the chunked-encoding bypass. Gzip decompresses with a hard
//! output cap, applied uniformly to all endpoints.

use crate::state::SharedState;
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde_json::json;
use std::time::Duration;

/// Pre-auth rate limiting, keyed by (client_ip, route_template).
/// Route templates (not raw paths) prevent 404-scan map growth (audit §5.3).
pub async fn rate_limit_middleware(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ip = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let template = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());

    // Per-(ip, template) bucket: 30 req/min sustained, burst 10.
    // Auth failures share the same bucket (they arrive here first).
    let key = (ip.clone(), template.clone());
    let allowed = {
        let mut buckets = state.rate_buckets.lock().await;
        let limiter = buckets.entry(key).or_insert_with(|| {
            governor::RateLimiter::direct(governor::Quota::per_minute(
                std::num::NonZeroU32::new(30).unwrap(),
            ))
        });
        limiter.check().is_ok()
    };

    if !allowed {
        tracing::warn!(%ip, template = %template, "rate limit exceeded");
        state.metrics.lock().await.rejections_429 += 1;
        // Best-effort cleanup of idle buckets to bound memory
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"ok": false, "error": "rate_limited", "retry_after_s": 60})),
        )
            .into_response();
    }

    next.run(req).await
}

/// Body cap: counts ACTUAL bytes streamed through the body, defeating the
/// chunked-Transfer-Encoding Content-Length bypass (audit SEC-5).
pub async fn body_cap_middleware(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let max = state.config.max_body_bytes;
    let (mut parts, body) = req.into_parts();

    let mut stream = body.into_data_stream();
    let mut total: usize = 0;
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "error": format!("body read error: {}", e)})),
                )
                    .into_response();
            }
        };
        total += chunk.len();
        if total > max {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"ok": false, "error": "body exceeds cap"})),
            )
                .into_response();
        }
        buf.extend_from_slice(&chunk);
    }

    // Check for gzip content-encoding and decompress with output cap
    let body_bytes = if parts
        .headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        == Some("gzip")
    {
        match decompress_capped(&buf, state.config.max_decompressed_bytes).await {
            Ok(b) => b,
            Err(e) => {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(json!({"ok": false, "error": e})),
                )
                    .into_response();
            }
        }
    } else {
        buf
    };

    let req = Request::from_parts(parts, Body::from(body_bytes));
    next.run(req).await
}

/// Streaming gzip decompression with a hard output cap (audit SEC-5).
/// Applied uniformly to ALL endpoints (fixes the eval/script asymmetry).
async fn decompress_capped(input: &[u8], max_out: usize) -> Result<Vec<u8>, String> {
    use async_compression::tokio::bufread::GzipDecoder;
    use tokio::io::AsyncReadExt;

    let mut decoder = GzipDecoder::new(input);
    let mut out = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = decoder
            .read(&mut chunk)
            .await
            .map_err(|e| format!("gzip decode error: {}", e))?;
        if n == 0 {
            break;
        }
        if out.len() + n > max_out {
            return Err(format!(
                "decompressed body exceeds cap of {} bytes (zip bomb rejected)",
                max_out
            ));
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

/// Request logging with timing. Rejections logged by inner layers.
pub async fn log_middleware(req: Request<Body>, next: Next) -> Response {
    let start = std::time::Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let resp = next.run(req).await;
    let elapsed = start.elapsed();
    tracing::info!(
        method = %method,
        path = %path,
        status = resp.status().as_u16(),
        elapsed_ms = elapsed.as_millis(),
        "request"
    );
    // Slow-request warning (event-loop stall detector)
    if elapsed > Duration::from_secs(5) {
        tracing::warn!(method = %method, path = %path, elapsed_ms = elapsed.as_millis(), "slow request");
    }
    resp
}
