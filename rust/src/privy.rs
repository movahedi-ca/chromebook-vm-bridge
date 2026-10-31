//! `PrivyGate`: outbound perception firewall (playbook §11.4).
//!
//! Two passes before any payload leaves the bridge:
//! 1. DOM sanitization: strip invisible/deceptive nodes used for prompt
//!    injection (opacity:0, font-size:0, display:none, off-screen, zero-contrast).
//! 2. PII redaction: regex pass replacing secrets with typed tokens.
//!
//! Also provides the URL firewall for navigation (audit SEC-6).

use regex::{Captures, Regex};
use std::borrow::Cow;
use std::sync::OnceLock;

/// JS: remove deceptive nodes before serialization. Returns count removed.
#[allow(dead_code)] // parked: sanitizer for a future snapshot pipeline (audit LOW-1)
pub const SANITIZE_JS: &str = r"
(function() {
  let removed = 0;
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT);
  const doomed = [];
  while (walker.nextNode()) {
    const el = walker.currentNode;
    const cs = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    const deceptive =
      cs.opacity === '0' || cs.display === 'none' ||
      cs.fontSize === '0px' || cs.visibility === 'hidden' ||
      (r.width === 0 && r.height === 0 && el.textContent.trim().length > 0) ||
      r.left < -10000 || r.top < -10000;
    // Zero-contrast text (e.g. white on white)
    const fg = cs.color, bg = cs.backgroundColor;
    const zeroContrast = fg === bg && fg !== 'rgba(0, 0, 0, 0)';
    if ((deceptive || zeroContrast) && el.textContent.trim().length > 0) {
      doomed.push(el);
    }
  }
  for (const el of doomed) { el.remove(); removed++; }
  return removed;
})();
";

static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();

fn patterns() -> &'static Vec<(Regex, &'static str)> {
    // Static regexes: expect, not unwrap — a broken pattern is a
    // programming bug and must fail loudly at first use, with a
    // message (pre-emptive hardening: no bare unwrap in src).
    PATTERNS.get_or_init(|| {
        vec![
            // API keys / tokens (generic high-entropy)
            (
                Regex::new(r#"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*['"]?([A-Za-z0-9_\-]{16,})['"]?"#).expect("PII pattern: static regex"),
                "<REDACTED_SECRET>",
            ),
            // Payment cards (13-19 digits, Luhn-ish)
            (
                Regex::new(r"\b(?:\d[ -]?){13,19}\b").expect("PII pattern: static regex"),
                "<REDACTED_CARD>",
            ),
            // SSN / SIN patterns
            (
                Regex::new(r"\b\d{3}[- ]\d{3}[- ]\d{3}\b").expect("PII pattern: static regex"),
                "<REDACTED_ID>",
            ),
            // Bearer tokens
            (
                Regex::new(r"Bearer\s+[A-Za-z0-9_\-\.]{16,}").expect("PII pattern: static regex"),
                "Bearer <REDACTED_TOKEN>",
            ),
            // AWS-style keys
            (
                Regex::new(r"AKIA[0-9A-Z]{16}").expect("PII pattern: static regex"),
                "<REDACTED_AWS_KEY>",
            ),
        ]
    })
}

/// Redact PII/secrets from text. Returns (`redacted_text`, `redaction_count`).
/// Wired into /browser/text, /browser/html, and eval string results
/// (audit 42 High-5).
pub fn redact(text: &str) -> (String, usize) {
    let mut count = 0usize;
    // Borrowed until the first pattern actually matches: clean text costs
    // no allocation.
    let mut out: Cow<'_, str> = Cow::Borrowed(text);
    for (re, token) in patterns() {
        // Single pass per pattern: count inside the replacer instead of a
        // separate `find_iter` scan (the old code walked the string twice
        // per pattern). `replace_all` returns `Borrowed` when nothing
        // matched, in which case the Cow is left untouched.
        let next = re.replace_all(&out, |_: &Captures| {
            count += 1;
            (*token).to_string()
        });
        if let Cow::Owned(s) = next {
            out = Cow::Owned(s);
        }
    }
    (out.into_owned(), count)
}

/// URL firewall for navigation (audit SEC-6).
/// Validate a client-supplied navigation URL and map rejection to the
/// standard 400 response. Shared by `/browser/navigate`, `/browser/tab/new`
/// and `/pool/navigate` (pre-emptive dedup: three identical match blocks).
#[allow(clippy::result_large_err)] // axum Response is large by design; not worth boxing
pub async fn validated_url_or_400(raw: &str) -> Result<url::Url, axum::response::Response> {
    use axum::{
        http::StatusCode,
        response::{IntoResponse, Json},
    };
    match validate_navigate_url(raw, &[]).await {
        Ok(u) => Ok(u),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": e})),
        )
            .into_response()),
    }
}

/// Allows only http/https; blocks loopback, link-local, private ranges,
/// and cloud metadata IPs after DNS resolution unless allowlisted.
///
/// KNOWN GAP (audit MED-2): DNS-rebinding TOCTOU. We resolve the host in
/// Rust, then Chrome resolves it again at navigate time — a rebinding
/// attacker can serve a benign IP to us and a private IP to Chrome.
/// Fully closing this requires navigating by literal resolved IP with a
/// Host-header rewrite, which breaks TLS SNI/virtual hosting; accepted
/// as a documented residual risk for a single-operator bridge.
pub async fn validate_navigate_url(raw: &str, allowlist: &[String]) -> Result<url::Url, String> {
    let parsed = url::Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("scheme '{s}' not allowed (http/https only)")),
    }
    let host = parsed.host_str().unwrap_or("");
    // Explicit allowlist bypass
    if allowlist.iter().any(|a| a == host) {
        return Ok(parsed);
    }
    // Block by literal IP
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_blocked_ip(&ip) {
            return Err(format!("IP {ip} is blocked (loopback/link-local/private)"));
        }
        return Ok(parsed);
    }
    // DNS-resolve and check all returned IPs. Resolution is blocking
    // std I/O — run it off the tokio worker (audit MED-2).
    let host_owned = host.to_string();
    let resolved = tokio::task::spawn_blocking(move || dns_resolve(&host_owned))
        .await
        .map_err(|e| format!("DNS task failed: {e}"))?;
    match resolved {
        Ok(ips) => {
            for ip in ips {
                if is_blocked_ip(&ip) {
                    return Err(format!("host {host} resolves to blocked IP {ip}"));
                }
            }
            Ok(parsed)
        }
        Err(e) => Err(format!("DNS resolution failed for {host}: {e}")),
    }
}

/// Normalize IPv4-mapped (`::ffff:10.0.0.1`) and IPv4-compatible
/// (`::10.0.0.1`) IPv6 addresses to plain IPv4 so the blocklist below
/// can't be bypassed with a mapped/compatible address (audit MED-2,
/// audit 42 High-1).
fn normalize(ip: std::net::IpAddr) -> std::net::IpAddr {
    if let std::net::IpAddr::V6(v6) = ip {
        if let Some(v4) = v6.to_ipv4() {
            return std::net::IpAddr::V4(v4);
        }
    }
    ip
}

fn is_blocked_ip(ip: &std::net::IpAddr) -> bool {
    let ip = normalize(*ip);
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback() || v4.is_link_local() || v4.is_private()
            // 0.0.0.0/8 ("this network"): is_unspecified() covers only
            // 0.0.0.0 itself — the whole /8 must be blocked (audit 42
            // High-1). Chrome treats 0.x as "unspecified-ish" and it can
            // reach local listeners on some stacks.
            || o[0] == 0
            || v4.is_broadcast()
            || v4.is_multicast()
            // 169.254.169.254 (cloud metadata) is link-local → blocked
            // 100.64.0.0/10 (CGNAT) — check manually
            || (o[0] == 100 && (o[1] & 0b11_000000) == 0b01_000000)
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unicast_link_local()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 unique-local — the IPv6 analog of RFC1918
                // (audit 40 MED-1)
                || (s[0] & 0xfe00) == 0xfc00
                // 2002::/16 6to4: the first 16 bits are 0x2002 and the
                // next 32 embed an IPv4 address that can itself be
                // private/loopback (audit 42 High-1)
                || s[0] == 0x2002
        }
    }
}

fn dns_resolve(host: &str) -> Result<Vec<std::net::IpAddr>, String> {
    use std::net::ToSocketAddrs;
    (host, 0)
        .to_socket_addrs()
        .map(|addrs| addrs.map(|a| a.ip()).collect())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocks_file_scheme() {
        assert!(validate_navigate_url("file:///etc/passwd", &[])
            .await
            .is_err());
    }

    #[tokio::test]
    async fn blocks_loopback() {
        assert!(validate_navigate_url("http://127.0.0.1:8899/", &[])
            .await
            .is_err());
    }

    #[tokio::test]
    async fn blocks_metadata() {
        assert!(validate_navigate_url("http://169.254.169.254/", &[])
            .await
            .is_err());
    }

    #[tokio::test]
    async fn allows_https() {
        assert!(validate_navigate_url("https://example.com/", &[])
            .await
            .is_ok());
    }

    #[test]
    fn blocks_ipv6_ula() {
        // fc00::/7 unique-local — the IPv6 analog of RFC1918 (audit 40 MED-1)
        assert!(is_blocked_ip(&"fd00::1".parse().unwrap()));
        assert!(is_blocked_ip(&"fc12:3456::1".parse().unwrap()));
        assert!(!is_blocked_ip(&"2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn blocks_zero_net_and_broadcast() {
        // 0.0.0.0/8: is_unspecified() alone covers only 0.0.0.0
        // (audit 42 High-1)
        assert!(is_blocked_ip(&"0.0.0.0".parse().unwrap()));
        assert!(is_blocked_ip(&"0.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip(&"0.1.2.3".parse().unwrap()));
        assert!(is_blocked_ip(&"255.255.255.255".parse().unwrap()));
        assert!(!is_blocked_ip(&"1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn blocks_6to4_and_compat() {
        // 2002::/16 6to4 embeds an IPv4 address (audit 42 High-1)
        assert!(is_blocked_ip(&"2002:7f00:1::1".parse().unwrap()));
        // ::ffff:127.0.0.1 (mapped) and ::127.0.0.1 (compatible) both
        // normalize to IPv4 loopback
        assert!(is_blocked_ip(&"::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip(&"::127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip(&"::ffff:10.0.0.1".parse().unwrap()));
        assert!(!is_blocked_ip(&"::ffff:1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn redact_finds_secrets() {
        let (out, n) = redact("my api_key = \"sk-1234567890abcdef\" end");
        assert!(n > 0);
        assert!(!out.contains("sk-1234567890abcdef"));
    }

    #[test]
    fn redact_single_pass_counts_every_match() {
        // Two secrets of the same class: the count must be exact (the
        // single-pass replacer counts inside the closure, no find_iter).
        let (out, n) = redact("a api_key=aaaaaaaaaaaaaaaa b api_key=bbbbbbbbbbbbbbbb c");
        assert_eq!(n, 2);
        assert!(!out.contains("aaaaaaaaaaaaaaaa"));
        assert!(!out.contains("bbbbbbbbbbbbbbbb"));
    }

    #[test]
    fn redact_clean_text_is_untouched() {
        let (out, n) = redact("hello world, nothing secret here");
        assert_eq!(n, 0);
        assert_eq!(out, "hello world, nothing secret here");
    }
}
