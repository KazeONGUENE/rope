//! CERBER WATCH — outbound-URL SSRF guard.
//!
//! Several HTTP surfaces accept a free-text URL from an untrusted caller and
//! later have the server itself issue an outbound request to it (databox /
//! third-party service registration `health_url`, oracle feed URLs, webhook
//! callbacks, etc.). Without validation this is a classic Server-Side
//! Request Forgery primitive: an attacker can point the server at
//! `http://169.254.169.254/...` (cloud instance metadata), `127.0.0.1:<port>`
//! (internal-only services such as rope-idp on 9096 or Postgres on 5432), or
//! an arbitrary third party (turning the server into an anonymized scanner
//! or DoS amplifier).
//!
//! This module provides two layers, meant to be used together:
//!
//! 1. [`validate_url_syntax`] — synchronous, cheap, called at
//!    registration/submission time. Rejects disallowed schemes, credentials
//!    embedded in the URL, and hostnames that are themselves a blocked
//!    literal IP or a well-known internal name (`localhost`, `metadata`,
//!    `*.internal`).
//! 2. [`validate_resolved_target`] — asynchronous, resolves the hostname via
//!    DNS and rejects the URL if *any* resolved address falls in a blocked
//!    range. Called immediately before the server actually dials the URL
//!    (defense against a hostname that passes syntax but resolves to a
//!    private/loopback/link-local address, including simple DNS-rebinding).
//!
//! Residual risk (documented, not hidden): there is a small TOCTOU window
//! between step 2's resolution and the HTTP client's own resolution+connect.
//! A sophisticated DNS-rebinding attacker who controls authoritative DNS for
//! the target hostname and can answer two different IPs within that window
//! could still slip a private-IP connection through the underlying HTTP
//! client. Closing that gap fully requires a custom resolver/connector that
//! pins the exact IP validated here for the actual TCP connect; that is a
//! larger client-plumbing change tracked separately. The two layers below
//! already eliminate the overwhelming majority of real-world SSRF payloads
//! (literal internal/metadata IPs, `localhost`, non-http(s) schemes).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SsrfError {
    #[error("url is empty")]
    Empty,
    #[error("url failed to parse: {0}")]
    Unparseable(String),
    #[error("scheme '{0}' is not allowed (only http/https)")]
    DisallowedScheme(String),
    #[error("url must not embed userinfo credentials")]
    EmbeddedCredentials,
    #[error("url has no host")]
    NoHost,
    #[error("host resolves to a blocked address: {0}")]
    BlockedAddress(IpAddr),
    #[error("hostname '{0}' is a blocked internal alias")]
    BlockedHostname(String),
    #[error("DNS resolution failed: {0}")]
    ResolutionFailed(String),
    #[error("host resolved to zero addresses")]
    NoResolvedAddresses,
}

/// Well-known internal / metadata hostnames that must never be reachable
/// via a server-issued outbound fetch, regardless of what they resolve to.
const BLOCKED_HOSTNAME_SUBSTRINGS: &[&str] = &[
    "localhost",
    "metadata.google.internal",
    "metadata.azure.com",
    ".internal",
    ".local",
];

/// Layer 1 — synchronous syntax + literal-IP validation. Call this at the
/// moment a URL is accepted from a caller (e.g. service/databox
/// registration), before it is ever persisted or dialed.
pub fn validate_url_syntax(raw: &str) -> Result<url::Url, SsrfError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SsrfError::Empty);
    }

    let parsed = url::Url::parse(trimmed).map_err(|e| SsrfError::Unparseable(e.to_string()))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(SsrfError::DisallowedScheme(scheme.to_string()));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(SsrfError::EmbeddedCredentials);
    }

    let host_str = parsed.host_str().ok_or(SsrfError::NoHost)?;
    let host_lower = host_str.to_ascii_lowercase();
    for blocked in BLOCKED_HOSTNAME_SUBSTRINGS {
        if host_lower == *blocked || host_lower.ends_with(blocked) {
            return Err(SsrfError::BlockedHostname(host_str.to_string()));
        }
    }

    // If the host is itself a literal IP address, check it immediately —
    // no DNS resolution needed and none should be attempted. Use
    // `Url::host()` rather than re-parsing `host_str()`: for an IPv6
    // literal, `host_str()` returns the bracketed form (e.g. `"[::1]"`),
    // which is NOT valid input for `IpAddr::from_str` and would silently
    // fail to match, letting a bracketed-IPv6 loopback/private address
    // straight through.
    if let Some(host) = parsed.host() {
        let literal_ip = match host {
            url::Host::Ipv4(v4) => Some(IpAddr::V4(v4)),
            url::Host::Ipv6(v6) => Some(IpAddr::V6(v6)),
            url::Host::Domain(_) => None,
        };
        if let Some(ip) = literal_ip {
            if is_blocked_address(&ip) {
                return Err(SsrfError::BlockedAddress(ip));
            }
        }
    }

    Ok(parsed)
}

/// Layer 2 — asynchronous DNS resolution + address-range validation. Call
/// this immediately before the server actually issues the outbound request
/// (i.e. right before `http_client.get(url).send()`), not only at
/// registration time, so a hostname that later starts resolving to an
/// internal address is caught on every fetch, not just the first one.
pub async fn validate_resolved_target(url: &url::Url) -> Result<(), SsrfError> {
    let host_str = url.host_str().ok_or(SsrfError::NoHost)?;

    // Literal IP host — no DNS involved, already checked in layer 1, but
    // re-check here too since this function may be called standalone. Use
    // `Url::host()`, not `host_str()`, for the same bracketed-IPv6 reason
    // documented in `validate_url_syntax`.
    if let Some(host) = url.host() {
        let literal_ip = match host {
            url::Host::Ipv4(v4) => Some(IpAddr::V4(v4)),
            url::Host::Ipv6(v6) => Some(IpAddr::V6(v6)),
            url::Host::Domain(_) => None,
        };
        if let Some(ip) = literal_ip {
            return if is_blocked_address(&ip) {
                Err(SsrfError::BlockedAddress(ip))
            } else {
                Ok(())
            };
        }
    }

    let port = url.port_or_known_default().unwrap_or(if url.scheme() == "https" {
        443
    } else {
        80
    });
    let lookup_target = format!("{host_str}:{port}");

    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(&lookup_target)
        .await
        .map_err(|e| SsrfError::ResolutionFailed(e.to_string()))?
        .collect();

    if addrs.is_empty() {
        return Err(SsrfError::NoResolvedAddresses);
    }

    for addr in &addrs {
        if is_blocked_address(&addr.ip()) {
            return Err(SsrfError::BlockedAddress(addr.ip()));
        }
    }

    Ok(())
}

/// Convenience wrapper: run both layers back to back. Returns the parsed
/// `Url` on success so the caller can reuse it without re-parsing.
pub async fn validate_outbound_url(raw: &str) -> Result<url::Url, SsrfError> {
    let parsed = validate_url_syntax(raw)?;
    validate_resolved_target(&parsed).await?;
    Ok(parsed)
}

fn is_blocked_address(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: &Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // covers 169.254.0.0/16, i.e. cloud metadata
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        // Carrier-grade NAT (100.64.0.0/10) — commonly used for
        // internal cloud/VPC ranges, not globally routable.
        || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
        // 0.0.0.0/8 "this network" — already partially covered by
        // is_unspecified but that only matches the exact all-zero address.
        || ip.octets()[0] == 0
}

fn is_blocked_v6(ip: &Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    // Unique local addresses (fc00::/7) and link-local (fe80::/10).
    let seg0 = ip.segments()[0];
    if (0xfc00..=0xfdff).contains(&seg0) || (0xfe80..=0xfebf).contains(&seg0) {
        return true;
    }
    // IPv4-mapped / IPv4-compatible IPv6 addresses must be unwrapped and
    // checked against the v4 rules too, otherwise `::ffff:169.254.169.254`
    // sails straight through.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(&v4);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_empty_and_non_http_schemes() {
        assert_eq!(validate_url_syntax(""), Err(SsrfError::Empty));
        assert!(matches!(
            validate_url_syntax("ftp://example.com/x"),
            Err(SsrfError::DisallowedScheme(_))
        ));
        assert!(matches!(
            validate_url_syntax("file:///etc/passwd"),
            Err(SsrfError::DisallowedScheme(_))
        ));
        assert!(matches!(
            validate_url_syntax("gopher://internal:70/"),
            Err(SsrfError::DisallowedScheme(_))
        ));
    }

    #[tokio::test]
    async fn rejects_embedded_credentials() {
        assert_eq!(
            validate_url_syntax("http://user:pass@example.com/"),
            Err(SsrfError::EmbeddedCredentials)
        );
    }

    #[tokio::test]
    async fn rejects_localhost_and_internal_aliases() {
        assert!(matches!(
            validate_url_syntax("http://localhost:9096/v1/auth/login"),
            Err(SsrfError::BlockedHostname(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://metadata.google.internal/computeMetadata/v1/"),
            Err(SsrfError::BlockedHostname(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://foo.internal/health"),
            Err(SsrfError::BlockedHostname(_))
        ));
    }

    #[tokio::test]
    async fn rejects_literal_loopback_private_and_metadata_ips() {
        assert!(matches!(
            validate_url_syntax("http://127.0.0.1:5432/"),
            Err(SsrfError::BlockedAddress(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://10.0.0.5/health"),
            Err(SsrfError::BlockedAddress(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://192.168.1.1/"),
            Err(SsrfError::BlockedAddress(_))
        ));
        // AWS/GCP/Azure instance metadata endpoint — link-local.
        assert!(matches!(
            validate_url_syntax("http://169.254.169.254/latest/meta-data/"),
            Err(SsrfError::BlockedAddress(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://100.64.0.1/"),
            Err(SsrfError::BlockedAddress(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://0.0.0.0:8080/"),
            Err(SsrfError::BlockedAddress(_))
        ));
    }

    #[tokio::test]
    async fn rejects_ipv6_loopback_and_link_local_and_mapped_v4() {
        assert!(matches!(
            validate_url_syntax("http://[::1]:9096/"),
            Err(SsrfError::BlockedAddress(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://[fe80::1]/"),
            Err(SsrfError::BlockedAddress(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://[fc00::1]/"),
            Err(SsrfError::BlockedAddress(_))
        ));
        assert!(matches!(
            validate_url_syntax("http://[::ffff:169.254.169.254]/"),
            Err(SsrfError::BlockedAddress(_))
        ));
    }

    #[tokio::test]
    async fn accepts_well_formed_public_https_url_syntax() {
        let parsed = validate_url_syntax("https://api.example.com:8443/health?x=1")
            .expect("should parse and pass syntax layer");
        assert_eq!(parsed.scheme(), "https");
        assert_eq!(parsed.host_str(), Some("api.example.com"));
    }

    #[tokio::test]
    async fn resolved_target_rejects_literal_blocked_ip_without_dns() {
        let url = url::Url::parse("http://127.0.0.1:8545/").unwrap();
        let err = validate_resolved_target(&url).await.unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress(_)));
    }

    #[tokio::test]
    async fn full_pipeline_rejects_blocked_literal_ip_end_to_end() {
        let err = validate_outbound_url("http://169.254.169.254/latest/meta-data/")
            .await
            .unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress(_)));
    }

    #[test]
    fn is_blocked_v4_covers_all_documented_ranges() {
        assert!(is_blocked_v4(&Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_blocked_v4(&Ipv4Addr::new(10, 1, 2, 3)));
        assert!(is_blocked_v4(&Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_blocked_v4(&Ipv4Addr::new(192, 168, 0, 1)));
        assert!(is_blocked_v4(&Ipv4Addr::new(169, 254, 169, 254)));
        assert!(is_blocked_v4(&Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_blocked_v4(&Ipv4Addr::new(0, 0, 0, 0)));
        assert!(is_blocked_v4(&Ipv4Addr::new(224, 0, 0, 1)));
        // A normal public address must NOT be blocked.
        assert!(!is_blocked_v4(&Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_blocked_v4(&Ipv4Addr::new(1, 1, 1, 1)));
    }
}
