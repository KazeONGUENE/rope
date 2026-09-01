//! CERBER WATCH wiring for rope-explorer's HTTP write paths.
//!
//! `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md` §5.0 found that
//! `rope-agent-runtime::security::cerber` - the module carrying CERBER's
//! input-validation logic - was never imported by either production
//! binary (`rope-node` or `rope-explorer`), so it provided zero real
//! protection against SQL-injection / XSS / path-traversal payloads
//! submitted through public write endpoints (project submissions, contact
//! form, databox registration, source-verification uploads, etc.).
//!
//! `rope-node` closed this gap for the JSON-RPC surface by wiring
//! `rope_security::guard::RequestGuard` into its dispatcher (see
//! `rope-node/src/rpc_server.rs`). This module does the same for
//! `rope-explorer`'s HTTP handlers: a single process-wide `RequestGuard`
//! singleton, plus two small helpers (`validate_fields`, `check_signer`)
//! that every write handler can call with a couple of lines, returning an
//! already-shaped `(StatusCode, Json<Value>)` (the same tuple shape nearly
//! every handler in this crate returns) so call sites stay a one-line
//! early-return.

use axum::{http::StatusCode, response::Json};
use serde_json::{json, Value};
use std::sync::OnceLock;

/// The shared CERBER WATCH gate for this process. Seeded with the
/// known-compromised-signer default blocklist (finding H1/C4) and
/// extendable via `ROPE_ADDITIONAL_BLOCKED_SIGNERS` (comma-separated),
/// mirroring `rope-node`'s equivalent singleton so operators only need to
/// remember one env var across both binaries.
fn guard() -> &'static rope_security::guard::RequestGuard {
    static GUARD: OnceLock<rope_security::guard::RequestGuard> = OnceLock::new();
    GUARD.get_or_init(|| {
        let g = rope_security::guard::RequestGuard::with_default_blocklist();
        if let Ok(extra) = std::env::var("ROPE_ADDITIONAL_BLOCKED_SIGNERS") {
            for addr in extra.split(',') {
                let addr = addr.trim();
                if !addr.is_empty() {
                    g.block_signer(addr);
                    tracing::info!(
                        target: "rope_explorer::security",
                        signer = addr,
                        "CERBER WATCH: added operator-supplied signer to blocklist"
                    );
                }
            }
        }
        g
    })
}

/// Validate a batch of `(field_name, value)` pairs pulled straight out of
/// a deserialized request body. Empty values are skipped (handlers already
/// enforce their own required/optional rules; this is a security gate, not
/// a presence check). Returns the first violation as a ready-to-`return`
/// `(StatusCode, Json<Value>)` error tuple.
///
/// Do NOT pass free-form source-code / bytecode fields (e.g. a Solidity
/// `source_code` submission) through this helper - block comments (`/* */`)
/// are ubiquitous in real source and would trip the SQL-comment-injection
/// heuristic on every legitimate submission. Those fields should be left
/// unvalidated here (any dangerous content in them is inert text stored
/// for display, not executed as SQL or interpreted as HTML in this
/// explorer's own pages) or checked with a narrower, source-aware rule.
pub fn validate_fields(fields: &[(&str, &str)]) -> Result<(), (StatusCode, Json<Value>)> {
    for (name, value) in fields {
        if value.is_empty() {
            continue;
        }
        if let Err(denial) = guard().validate_input(value, name) {
            tracing::warn!(
                target: "rope_explorer::security",
                field = *name,
                category = %denial.category,
                "CERBER WATCH: rejected write request with malicious input"
            );
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "success": false,
                    "error": format!(
                        "request rejected by CERBER WATCH: field '{name}' matches a {} pattern",
                        denial.category
                    ),
                })),
            ));
        }
    }
    Ok(())
}

/// CERBER WATCH self-check: is the process-wide guard still seeded with
/// the known-compromised-signer blocklist? This is the cheapest possible
/// canary for "did a refactor accidentally construct a bare
/// `RequestGuard::new()` (empty blocklist) instead of
/// `with_default_blocklist()` somewhere", without requiring any network
/// call. Exposed (rather than kept private) so the config-drift probe
/// below - and any future health/status endpoint - can read it.
pub fn cerber_blocklist_active() -> bool {
    guard().is_signer_blocked(rope_security::guard::KNOWN_COMPROMISED_SIGNERS[0])
}

/// Two destructive `rope_*` JSON-RPC methods used as a live canary for the
/// Phase-1 V11 gate (`docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`
/// / `handover-security-audit-2026-06-11.mdc`). Both are on
/// `rope_auth::DESTRUCTIVE_METHODS` in `rope-node` and MUST be rejected
/// with JSON-RPC error `-32401` for any caller the node treats as
/// non-internal.
const DESTRUCTIVE_GATE_CANARY_METHODS: &[&str] = &["rope_untieKnot", "rope_appendToLedger"];

/// The Phase-1 destructive-method-gate denial code from
/// `rope-node/src/rpc_auth.rs`. Duplicated here (rather than a
/// cross-crate dependency on `rope-node`, which would be a real
/// dependency-graph inversion - `rope-node` already depends on
/// `rope-security`, not the other way around) because it is a small,
/// stable, publicly documented protocol constant.
const ROPE_NODE_DESTRUCTIVE_GATE_DENIAL_CODE: i64 = -32401;

/// Probe one destructive method against the currently active backend RPC
/// endpoint, **forging an `X-Forwarded-For` header** so the node's
/// loopback-without-XFF internal-caller bypass does not fire even though
/// this call is, physically, a co-located loopback connection. Per
/// `handover-security-audit-2026-06-11.mdc` §"Patched `rpc_server.rs`":
/// nginx always sets XFF on traffic it proxies, so a *missing* XFF on a
/// loopback connection is what marks a caller as internal/trusted. By
/// deliberately setting XFF ourselves for this one probe, we make
/// rope-node treat us exactly like an internet-side caller for the
/// duration of this single request - which is the only way to actually
/// observe the public-facing gate posture from a co-located process
/// without going out over the public internet.
///
/// The params sent are deliberately inert placeholders (an all-zero
/// string id / wallet address): the gate rejects by method name alone,
/// before any parameter is inspected, so this probe can never mutate
/// real state even if the gate were somehow bypassed.
async fn probe_destructive_gate(state: &crate::AppState, method: &str) -> String {
    let params = match method {
        "rope_untieKnot" => serde_json::json!([
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        ]),
        _ => serde_json::json!(["0x0000000000000000000000000000000000000000"]),
    };
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });
    let url = state.rpc_url_active();
    let resp = state
        .http_client
        .post(url)
        .header("X-Forwarded-For", "cerber-config-drift-probe")
        .json(&body)
        .send()
        .await;
    let json = match resp {
        Ok(r) => match r.json::<Value>().await {
            Ok(j) => j,
            Err(e) => return format!("unreachable (bad response body: {e})"),
        },
        Err(e) => return format!("unreachable ({e})"),
    };
    if json.get("result").is_some() {
        return "ALLOWED".to_string();
    }
    match json.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()) {
        Some(code) if code == ROPE_NODE_DESTRUCTIVE_GATE_DENIAL_CODE => "denied".to_string(),
        Some(code) => format!("unexpected_error_code:{code}"),
        None => "unexpected_response_shape".to_string(),
    }
}

/// CERBER's config-drift detector (the second new capability recommended
/// alongside the boot-time dispatcher-completeness check). Unlike
/// `validate_fields`/`check_signer`, which run on the request hot path,
/// this is meant to be driven by a periodic background task (see
/// `main.rs`'s startup task list) - it checks *ambient* security posture
/// that no single request would ever surface:
///
/// 1. Is this process's own `RequestGuard` still seeded with the
///    known-compromised-signer blocklist (a same-process canary)?
/// 2. Does the connected rope-node backend still reject destructive
///    `rope_*` methods for non-internal callers (a cross-process canary
///    for the Phase-1 V11 gate staying deployed and enabled)?
///
/// Findings are logged (WATCH), never auto-remediated (no STRIKE) -
/// reverting a live security posture from an autonomous background loop
/// would itself be a dangerous action; see `rope-security::config_drift`
/// module docs for the full rationale.
pub async fn run_config_drift_probe(state: &crate::AppState) -> rope_security::config_drift::DriftReport {
    let mut baseline = rope_security::config_drift::ConfigBaseline::new()
        .with("cerber.watch.blocklist_active", "true");
    for method in DESTRUCTIVE_GATE_CANARY_METHODS {
        baseline = baseline.with(format!("rope_node.destructive_gate.{method}"), "denied");
    }

    let mut observed = std::collections::BTreeMap::new();
    observed.insert(
        "cerber.watch.blocklist_active".to_string(),
        cerber_blocklist_active().to_string(),
    );
    for method in DESTRUCTIVE_GATE_CANARY_METHODS {
        let verdict = probe_destructive_gate(state, method).await;
        observed.insert(format!("rope_node.destructive_gate.{method}"), verdict);
    }

    let report = rope_security::config_drift::compare(&baseline, &observed);
    if report.is_clean() {
        tracing::debug!(
            target: "rope_explorer::security",
            "CERBER config-drift probe: security posture matches baseline (blocklist active, \
             destructive-method gate enforced)"
        );
    } else {
        tracing::error!(
            target: "rope_explorer::security",
            drift = %report.summary(),
            "CERBER config-drift probe: security posture DRIFTED from baseline - see \
             SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md and \
             handover-security-audit-2026-06-11.mdc"
        );
    }
    report
}

/// Check a wallet/signer address (e.g. `owner_address`, `voter_address`)
/// against the blocklist. A blank/empty address is treated as "not
/// applicable" (Ok) - callers that require a non-empty signer already
/// enforce that separately.
pub fn check_signer(addr: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if addr.trim().is_empty() {
        return Ok(());
    }
    if let Err(_denial) = guard().check_signer(addr) {
        tracing::warn!(
            target: "rope_explorer::security",
            signer = addr,
            "CERBER WATCH: rejected write request naming a blocklisted signer"
        );
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!(
                    "signer {addr} is denylisted by CERBER WATCH; see \
                     SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md finding H1/C4"
                ),
            })),
        ));
    }
    Ok(())
}

/// CERBER WATCH - SSRF guard for server-issued outbound URLs.
///
/// `docs/SECURITY_AUDIT_2026-07-26` counter-audit finding: the databox /
/// third-party-service registration endpoint (`extra.rs::services_registry_post`)
/// accepts an attacker-controlled `health_url`, which this process then
/// dials on a periodic health-check loop (`main.rs::agent_health_ok`) with
/// no validation at all - a classic Server-Side Request Forgery primitive
/// (cloud metadata endpoints, internal-only services on loopback, or
/// third-party DoS amplification). This helper wraps
/// `rope_security::ssrf_guard` in the same ready-to-`return`
/// `(StatusCode, Json<Value>)` shape as the other guards in this module, so
/// registration handlers reject a malicious `health_url` at submission time
/// (see also: call the async `validate_outbound_url_async` again
/// immediately before actually dialing the URL, for defense against a
/// hostname that later starts resolving to an internal address).
pub fn validate_outbound_url(field_name: &str, raw: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if raw.trim().is_empty() {
        return Ok(());
    }
    if let Err(e) = rope_security::ssrf_guard::validate_url_syntax(raw) {
        tracing::warn!(
            target: "rope_explorer::security",
            field = field_name,
            error = %e,
            "CERBER WATCH: rejected outbound URL - possible SSRF payload"
        );
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!(
                    "request rejected by CERBER WATCH: field '{field_name}' is not a permitted \
                     outbound URL ({e})"
                ),
            })),
        ));
    }
    Ok(())
}

/// Async companion to [`validate_outbound_url`] - resolves the hostname via
/// DNS and re-checks every resolved address, catching a hostname that
/// passed the syntax-only check at registration time but now (or always)
/// resolves to a private/loopback/link-local/metadata address. Callers
/// should invoke this immediately before actually dialing the URL, not
/// only at registration time.
///
/// Currently unused by any call site: `services_registry_post`'s
/// `health_url` (the only attacker-controlled outbound-URL field in this
/// crate today) is stored but never dialed by any existing health-check
/// loop - `agent_health_ok`'s two callers both use trusted,
/// operator-controlled URLs (see the doc comment on `agent_health_ok` in
/// `main.rs`). Kept (fully implemented and unit-tested, not a stub) so
/// that the moment a health-check loop for `services_registry` entries is
/// added, this is a one-line call away instead of a forgotten SSRF gap.
#[allow(dead_code)]
pub async fn validate_outbound_url_before_fetch(raw: &str) -> bool {
    if raw.trim().is_empty() {
        return false;
    }
    match rope_security::ssrf_guard::validate_outbound_url(raw).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(
                target: "rope_explorer::security",
                url = raw,
                error = %e,
                "CERBER WATCH: refused to dial outbound URL at fetch time - possible SSRF \
                 (DNS rebinding or drifted resolution since registration)"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_fields_allows_benign_batch() {
        assert!(validate_fields(&[
            ("name", "Kibali Gold Mine"),
            ("description", "A real-world gold mine"),
            ("empty_field", ""),
        ])
        .is_ok());
    }

    #[test]
    fn validate_fields_rejects_sql_attack_in_structured_field() {
        let err = validate_fields(&[("category", "'; DROP TABLE projects; --")]);
        assert!(err.is_err());
        let (status, _) = err.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_fields_rejects_xss_even_in_chat_field() {
        let err = validate_fields(&[("description", "<script>alert(1)</script>")]);
        assert!(err.is_err());
    }

    #[test]
    fn validate_fields_allows_sql_keywords_in_free_text_description() {
        // "select" / "alter" are ordinary English words that legitimately
        // appear in project descriptions - must not false-positive.
        assert!(validate_fields(&[(
            "description",
            "Users can select a plan and alter their subscription anytime."
        )])
        .is_ok());
    }

    #[test]
    fn cerber_blocklist_active_is_true_by_default() {
        // Guards against a future refactor accidentally constructing a
        // bare `RequestGuard::new()` (empty blocklist) instead of
        // `with_default_blocklist()` for the process-wide singleton.
        assert!(cerber_blocklist_active());
    }

    #[test]
    fn check_signer_allows_empty_address() {
        assert!(check_signer("").is_ok());
        assert!(check_signer("   ").is_ok());
    }

    #[test]
    fn check_signer_rejects_known_compromised_key() {
        let err = check_signer("0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195");
        assert!(err.is_err());
        let (status, _) = err.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn check_signer_allows_clean_address() {
        assert!(check_signer("0x000000000000000000000000000000000000dEaD").is_ok());
    }

    #[test]
    fn validate_outbound_url_allows_empty_and_public_https() {
        assert!(validate_outbound_url("health_url", "").is_ok());
        assert!(validate_outbound_url("health_url", "https://tanastok.io/api/v1/health").is_ok());
    }

    #[test]
    fn validate_outbound_url_rejects_metadata_and_loopback() {
        assert!(validate_outbound_url("health_url", "http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_outbound_url("health_url", "http://127.0.0.1:5432/").is_err());
        assert!(validate_outbound_url("health_url", "http://localhost:9096/").is_err());
        assert!(validate_outbound_url("health_url", "ftp://internal/x").is_err());
    }

    #[tokio::test]
    async fn validate_outbound_url_before_fetch_rejects_blocked_literal_ip() {
        assert!(!validate_outbound_url_before_fetch("http://127.0.0.1:8545/").await);
    }

    #[tokio::test]
    async fn validate_outbound_url_before_fetch_rejects_empty() {
        assert!(!validate_outbound_url_before_fetch("").await);
    }
}
