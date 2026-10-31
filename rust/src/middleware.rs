//! Middleware stack (audit SEC-4, SEC-5).
//!
//! Layer order (outermost first): timeout → rate_limit → auth → lease →
//! body_cap → log → handler. (Registered innermost-first: axum applies the
//! LAST .layer() call as the outermost wrapper.)
//! Rate limiting BEFORE auth so brute-force attempts consume tokens.
//! The timeout layer bounds total request time (audit HIGH-5).
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

/// Pre-auth rate limiting, keyed by (client_identity, raw_path).
///
/// NOTE: this middleware runs as a `Router::layer`, i.e. BEFORE route
/// matching, so `MatchedPath` is never populated here — the key always
/// uses the raw request path (audit HIGH-4). Unbounded growth from
/// 404-scan path variants is contained by the idle-bucket eviction
/// below (cap 10k entries, 120s idle TTL).
///
/// Client identity is the socket IP (audit 40 HIGH-3: the presented key is
/// unverified pre-auth, so it must not feed the identity). Runs before auth,
/// so brute-force attempts consume tokens too. Liveness probes (/health) are
/// exempt; expensive paths get the documented stricter quota (audit 40 MED-8).
pub async fn rate_limit_middleware(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();

    // Liveness probes are never throttled: a monitoring poller must not
    // 429 itself out of visibility.
    if path == "/health" {
        return next.run(req).await;
    }

    let identity = client_identity(&req);

    // Per-(identity, path) bucket. Expensive paths match the documented
    // contract (docs/chromebook-vm-setup.md): 1 token per 3s sustained,
    // burst 5. Everything else: 30 req/min sustained, burst 10
    // (governor's default burst equals the rate; allow_burst pins it
    // to the documented contract).
    // `replenish_s` is the worst-case wait for the next token, reported
    // as Retry-After on rejection (audit 40 LOW-9).
    let (quota, replenish_s) = if matches!(
        path.as_str(),
        "/browser/drag" | "/browser/eval" | "/browser/script"
    ) {
        (
            governor::Quota::per_minute(std::num::NonZeroU32::new(20).unwrap())
                .allow_burst(std::num::NonZeroU32::new(5).unwrap()),
            3u64,
        )
    } else {
        (
            governor::Quota::per_minute(std::num::NonZeroU32::new(30).unwrap())
                .allow_burst(std::num::NonZeroU32::new(10).unwrap()),
            2u64,
        )
    };
    let key = (identity.clone(), path.clone());
    // On rejection, report the bucket's actual backoff instead of a
    // hardcoded guess, and set the standard header (audit 40 LOW-9).
    let retry_after_s: Option<u64> = {
        let mut buckets = state.rate_buckets.lock().await;
        let now = std::time::Instant::now();
        // Bound memory: evict buckets idle longer than 120s once the
        // map grows past 10k entries (audit HIGH-4).
        if buckets.len() > 10_000 {
            buckets.retain(|_, (_, last_seen)| {
                now.duration_since(*last_seen) < Duration::from_secs(120)
            });
        }
        let entry = buckets
            .entry(key)
            .or_insert_with(|| (governor::RateLimiter::direct(quota), now));
        entry.1 = now;
        match entry.0.check() {
            Ok(_) => None,
            // Worst case the client waits one replenish interval for the
            // next token; never a hardcoded guess.
            Err(_) => Some(replenish_s),
        }
    };

    if let Some(wait_s) = retry_after_s {
        tracing::warn!(identity = %identity, path = %path, wait_s, "rate limit exceeded");
        state.metrics.lock().await.rejections_429 += 1;
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", wait_s.to_string())],
            Json(json!({"ok": false, "error": "rate_limited", "retry_after_s": wait_s})),
        )
            .into_response();
    }

    next.run(req).await
}

/// Stable per-client identity for rate limiting (audit 40 HIGH-3).
///
/// Uses the socket IP only. The presented `x-api-key` is UNVERIFIED at this
/// layer (auth runs after the rate limiter by design), so keying on it lets
/// a brute-forcer rotate keys for a fresh bucket on every request and the
/// pre-auth throttle never fires.
fn client_identity(req: &Request<Body>) -> String {
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| format!("ip:{}", c.0.ip()))
        .unwrap_or_else(|| "ip:unknown".to_string())
}

/// Body cap: counts ACTUAL bytes streamed through the body, defeating the
/// chunked-Transfer-Encoding Content-Length bypass (audit SEC-5).
pub async fn body_cap_middleware(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let max = state.config.max_body_bytes;
    let (parts, body) = req.into_parts();

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

/// Lease enforcement: when a lease is held, mutating requests to browser #1
/// need the matching X-Lease-Id header or the bridge answers 423.
/// Reads never need a lease. Pool paths are exempt (separate browser).
pub async fn lease_middleware(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let is_mutating = matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE");
    let exempt = path == "/lease/acquire"
        || path == "/lease/release"
        || path == "/lease/renew"
        || path.starts_with("/pool/");

    if is_mutating && !exempt {
        let allowed = {
            let mut leases = state.leases.lock().await;
            if !leases.locked() {
                true
            } else {
                let presented = req
                    .headers()
                    .get("x-lease-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                leases.check(presented)
            }
        };
        if !allowed {
            state.metrics.lock().await.rejections_423 += 1;
            return (
                StatusCode::LOCKED,
                Json(json!({"ok": false, "error": "browser leased by another client"})),
            )
                .into_response();
        }
    }

    next.run(req).await
}

/// Request logging with timing. Rejections logged by inner layers.
/// Also the metrics hook (audit MED-5): counts every request and every
/// 4xx/5xx response here, the one place the final status is known.
pub async fn log_middleware(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let start = std::time::Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let resp = next.run(req).await;
    let elapsed = start.elapsed();
    {
        let mut m = state.metrics.lock().await;
        m.requests_total += 1;
        match resp.status().as_u16() {
            400..=499 => m.requests_4xx += 1,
            500..=599 => m.requests_5xx += 1,
            _ => {}
        }
    }
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
