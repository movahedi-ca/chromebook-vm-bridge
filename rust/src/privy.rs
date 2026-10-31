//! PrivyGate: outbound perception firewall (playbook §11.4).
//!
//! Two passes before any payload leaves the bridge:
//! 1. DOM sanitization: strip invisible/deceptive nodes used for prompt
//!    injection (opacity:0, font-size:0, display:none, off-screen, zero-contrast).
//! 2. PII redaction: regex pass replacing secrets with typed tokens.
//!
//! Also provides the URL firewall for navigation (audit SEC-6).

use regex::Regex;
use std::sync::OnceLock;

/// JS: remove deceptive nodes before serialization. Returns count removed.
pub const SANITIZE_JS: &str = r#"
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
"#;

static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();

fn patterns() -> &'static Vec<(Regex, &'static str)> {
    PATTERNS.get_or_init(|| {
        vec![
            // API keys / tokens (generic high-entropy)
            (
                Regex::new(r#"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*['"]?([A-Za-z0-9_\-]{16,})['"]?"#).unwrap(),
                "<REDACTED_SECRET>",
            ),
            // Payment cards (13-19 digits, Luhn-ish)
            (
                Regex::new(r"\b(?:\d[ -]?){13,19}\b").unwrap(),
                "<REDACTED_CARD>",
            ),
            // SSN / SIN patterns
            (
                Regex::new(r"\b\d{3}[- ]\d{3}[- ]\d{3}\b").unwrap(),
                "<REDACTED_ID>",
            ),
            // Bearer tokens
            (
                Regex::new(r"Bearer\s+[A-Za-z0-9_\-\.]{16,}").unwrap(),
                "Bearer <REDACTED_TOKEN>",
            ),
            // AWS-style keys
            (
                Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
                "<REDACTED_AWS_KEY>",
            ),
        ]
    })
}

/// Redact PII/secrets from text. Returns (redacted_text, redaction_count).
pub fn redact(text: &str) -> (String, usize) {
    let mut out = text.to_string();
    let mut count = 0;
    for (re, token) in patterns() {
        let new = re.replace_all(&out, *token).into_owned();
        if new.len() != out.len() {
            count += 1;
        }
        out = new;
    }
    (out, count)
}

/// URL firewall for navigation (audit SEC-6).
/// Allows only http/https; blocks loopback, link-local, private ranges,
/// and cloud metadata IPs after DNS resolution unless allowlisted.
pub fn validate_navigate_url(raw: &str, allowlist: &[String]) -> Result<url::Url, String> {
    let parsed = url::Url::parse(raw).map_err(|e| format!("invalid URL: {}", e))?;
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("scheme '{}' not allowed (http/https only)", s)),
    }
    let host = parsed.host_str().unwrap_or("");
    // Explicit allowlist bypass
    if allowlist.iter().any(|a| a == host) {
        return Ok(parsed);
    }
    // Block by literal IP
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_blocked_ip(&ip) {
            return Err(format!("IP {} is blocked (loopback/link-local/private)", ip));
        }
        return Ok(parsed);
    }
    // DNS-resolve and check all returned IPs
    match dns_resolve(host) {
        Ok(ips) => {
            for ip in ips {
                if is_blocked_ip(&ip) {
                    return Err(format!(
                        "host {} resolves to blocked IP {}",
                        host, ip
                    ));
                }
            }
            Ok(parsed)
        }
        Err(e) => Err(format!("DNS resolution failed for {}: {}", host, e)),
    }
}

fn is_blocked_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_link_local() || v4.is_private()
            // 169.254.169.254 (cloud metadata) is link-local → blocked
            // 100.64.0.0/10 (CGNAT) — check manually
            || (v4.octets()[0] == 100 && (v4.octets()[1] & 0b11000000) == 0b01000000)
        }
        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unicast_link_local(),
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

    #[test]
    fn blocks_file_scheme() {
        assert!(validate_navigate_url("file:///etc/passwd", &[]).is_err());
    }

    #[test]
    fn blocks_loopback() {
        assert!(validate_navigate_url("http://127.0.0.1:8899/", &[]).is_err());
    }

    #[test]
    fn blocks_metadata() {
        assert!(validate_navigate_url("http://169.254.169.254/", &[]).is_err());
    }

    #[test]
    fn allows_https() {
        assert!(validate_navigate_url("https://example.com/", &[]).is_ok());
    }

    #[test]
    fn redact_finds_secrets() {
        let (out, n) = redact("my api_key = \"sk-1234567890abcdef\" end");
        assert!(n > 0);
        assert!(!out.contains("sk-1234567890abcdef"));
    }
}
