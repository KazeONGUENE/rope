//! Request Guard — the CERBER **WATCH** capability wired into the live
//! request path.
//!
//! ## Why this module exists
//!
//! `CerberAgent` (see `lib.rs`) is an offline/periodic scanner: you hand it
//! a `ScanTarget` (contract bytecode, Solidity/Rust source, a batch of
//! `NetworkEvent`s) and it runs a battery of async scanners against it.
//! That is the right shape for auditing code or reviewing traffic samples,
//! but it is the *wrong* shape for the thing `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`
//! §5 actually asked for: a synchronous, allocation-light check that
//! `rope-node`'s JSON-RPC dispatcher and `rope-explorer`'s HTTP handlers
//! can call **on every single inbound request** without adding an async
//! scan pipeline to the hot path.
//!
//! `RequestGuard` is that gate. It ports the input-validation logic that
//! already existed (and was already correct) in
//! `rope-agent-runtime::security::cerber::EnhancedInputValidator`, which
//! was never imported by either production binary and therefore provided
//! zero real protection — see the audit's §5.0: "cerber.rs ... is only
//! exercised in its own unit tests ... it is inert code." Porting the logic
//! into `rope-security` (a crate `rope-node` and `rope-explorer` can safely
//! depend on — it only pulls in `rope-core`/`rope-crypto`, so there is no
//! circular-dependency risk) and wiring `RequestGuard` into the actual
//! dispatch path closes that gap.
//!
//! It also adds the **`blocked_signers`** capability the audit specifically
//! recommended for finding H1: the compromised deployer key
//! (`0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195`, see C4/H1) still held
//! `PROPOSER_ROLE`/`CANCELLER_ROLE` on `DCSwapTimelock` at audit time. Any
//! Rope-native RPC call whose wallet/signer parameter matches an entry in
//! this list is rejected at the RPC layer regardless of what on-chain role
//! that address still legitimately holds — a second line of defense that
//! does not depend on every on-chain role revocation having landed yet.

use parking_lot::RwLock;
use regex::Regex;
use std::collections::HashSet;

/// The compromised deployer key identified in
/// `SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md` findings C4/H1. Seeded into
/// every [`RequestGuard::with_default_blocklist`] instance so a fresh node
/// boots with this protection in place without any operator configuration.
///
/// Do not remove this entry without confirming (a) the key has been fully
/// rotated everywhere per `handover-from-dcswap-minter-rotation-2026-07-03.mdc`
/// and (b) every remaining on-chain role has been revoked. Until both are
/// true, this in-process gate is a real, independent control — not
/// redundant with the on-chain rotation.
pub const KNOWN_COMPROMISED_SIGNERS: &[&str] = &["0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195"];

/// Fields that typically contain natural language (chat, prompts, free-text
/// descriptions). These fields should NOT be checked against the generic
/// SQL-keyword heuristics, which flag ordinary words like "select" or
/// "alter" in prose. XSS and path-traversal checks still apply to every
/// field regardless of this classification.
pub const CHAT_FIELDS: &[&str] = &[
    "message",
    "prompt",
    "query",
    "content",
    "text",
    "input",
    "user_message",
    "assistant_message",
    "system_prompt",
    "context",
    "conversation",
    "description",
    "notes",
];

/// Why a [`RequestGuard`] check rejected a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DenyCategory {
    SqlInjection,
    XssAttack,
    PathTraversal,
    BlockedSigner,
    BlockedIp,
}

impl std::fmt::Display for DenyCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DenyCategory::SqlInjection => write!(f, "sql_injection"),
            DenyCategory::XssAttack => write!(f, "xss_attack"),
            DenyCategory::PathTraversal => write!(f, "path_traversal"),
            DenyCategory::BlockedSigner => write!(f, "blocked_signer"),
            DenyCategory::BlockedIp => write!(f, "blocked_ip"),
        }
    }
}

/// A rejected verdict from [`RequestGuard`], carrying enough context to log
/// a useful WATCH alert and to build a JSON-RPC / HTTP error response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardDenial {
    pub category: DenyCategory,
    /// The field name (input validation) or the offending signer/IP
    /// (blocklist checks) that triggered the denial.
    pub subject: String,
}

impl std::fmt::Display for GuardDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CERBER WATCH: {} ({})", self.category, self.subject)
    }
}

/// Enhanced input validator with SQL-injection / XSS / path-traversal
/// pattern detection. Faithful port of
/// `rope-agent-runtime::security::cerber::EnhancedInputValidator` (v3.0),
/// with the same chat-aware behaviour (natural-language fields skip the
/// generic SQL-keyword heuristic but still get the "definite attack" and
/// XSS/path-traversal checks).
pub struct InputValidator {
    /// SQL keyword patterns — useful signal on structured fields, but noisy
    /// (false-positive-prone) on free text, so gated by `is_chat_field`.
    sql_patterns: Vec<Regex>,
    /// SQL patterns that indicate an actual attack regardless of context
    /// (comment injection, `OR 1=1`, `UNION SELECT`, etc.).
    sql_attack_patterns: Vec<Regex>,
    xss_patterns: Vec<Regex>,
    path_patterns: Vec<Regex>,
}

impl InputValidator {
    pub fn new() -> Self {
        let sql_patterns = [
            r"(?i)(\b(SELECT|INSERT|UPDATE|DELETE|DROP|UNION|ALTER|CREATE|TRUNCATE|EXEC|EXECUTE)\b)",
            r"(?i)(\bOR\b\s+\d+\s*=\s*\d+)",
            r"(?i)(\bAND\b\s+\d+\s*=\s*\d+)",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

        let sql_attack_patterns = [
            r"(--|#|/\*|\*/)",
            r"\x00",
            r"(?i)(\bOR\b\s+1\s*=\s*1)",
            r"(?i)(\bAND\b\s+1\s*=\s*1)",
            r"(?i)(\bUNION\s+SELECT\b)",
            r"(?i)(\bDROP\s+(TABLE|DATABASE)\b)",
            r"(?i)(;\s*(SELECT|INSERT|UPDATE|DELETE|DROP|UNION|ALTER|CREATE|TRUNCATE|EXEC))",
            r"(?i)('\s*(OR|AND|UNION|SELECT)\b)",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

        let xss_patterns = [
            r"(?i)(<script[^>]*>.*?</script>)",
            r"(?i)(javascript:)",
            r"(?i)(on\w+\s*=)",
            r"(?i)(<iframe[^>]*>)",
            r"(?i)(<object[^>]*>)",
            r"(?i)(<embed[^>]*>)",
            r"(?i)(<link[^>]*>)",
            r"(?i)(<meta[^>]*>)",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

        let path_patterns = [
            r"\.\./",
            r"\.\.\\",
            r"(?i)(%2e%2e%2f)",
            r"(?i)(%252e%252e%252f)",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

        Self {
            sql_patterns,
            sql_attack_patterns,
            xss_patterns,
            path_patterns,
        }
    }

    pub fn check_sql_injection(&self, value: &str) -> bool {
        self.sql_patterns.iter().any(|p| p.is_match(value))
    }

    pub fn check_sql_attack(&self, value: &str) -> bool {
        self.sql_attack_patterns.iter().any(|p| p.is_match(value))
    }

    pub fn check_xss(&self, value: &str) -> bool {
        self.xss_patterns.iter().any(|p| p.is_match(value))
    }

    pub fn check_path_traversal(&self, value: &str) -> bool {
        self.path_patterns.iter().any(|p| p.is_match(value))
    }

    pub fn is_chat_field(field_name: &str) -> bool {
        let lower = field_name.to_lowercase();
        CHAT_FIELDS.iter().any(|f| lower.contains(f))
    }

    /// Validate `value` (from a field named `field_name`) against every
    /// attack class this validator knows about, auto-detecting chat-field
    /// context from the field name.
    pub fn validate(&self, value: &str, field_name: &str) -> Result<(), GuardDenial> {
        let is_chat = Self::is_chat_field(field_name);
        self.validate_with_context(value, field_name, is_chat)
    }

    pub fn validate_with_context(
        &self,
        value: &str,
        field_name: &str,
        is_chat: bool,
    ) -> Result<(), GuardDenial> {
        if self.check_sql_attack(value) {
            return Err(GuardDenial {
                category: DenyCategory::SqlInjection,
                subject: field_name.to_string(),
            });
        }
        if !is_chat && self.check_sql_injection(value) {
            return Err(GuardDenial {
                category: DenyCategory::SqlInjection,
                subject: field_name.to_string(),
            });
        }
        if self.check_xss(value) {
            return Err(GuardDenial {
                category: DenyCategory::XssAttack,
                subject: field_name.to_string(),
            });
        }
        if self.check_path_traversal(value) {
            return Err(GuardDenial {
                category: DenyCategory::PathTraversal,
                subject: field_name.to_string(),
            });
        }
        Ok(())
    }
}

impl Default for InputValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize an address/signer string for set membership: lowercase, and
/// tolerant of a missing `0x` prefix (some callers pass bare hex).
fn normalize_signer(addr: &str) -> String {
    let trimmed = addr.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("0x") {
        lower
    } else {
        format!("0x{lower}")
    }
}

/// The synchronous, per-request CERBER WATCH gate. Safe to share behind an
/// `Arc` across every connection handler; all interior state is protected
/// by `parking_lot::RwLock` (no async lock, so it is trivially callable
/// from both `rope-node`'s Tokio tasks and `rope-explorer`'s Axum handlers).
pub struct RequestGuard {
    input_validator: InputValidator,
    blocked_ips: RwLock<HashSet<String>>,
    blocked_signers: RwLock<HashSet<String>>,
}

impl RequestGuard {
    /// Empty guard — no IPs or signers blocked. Prefer
    /// [`RequestGuard::with_default_blocklist`] in production so the
    /// H1/C4 compromised-key protection is present by default.
    pub fn new() -> Self {
        Self {
            input_validator: InputValidator::new(),
            blocked_ips: RwLock::new(HashSet::new()),
            blocked_signers: RwLock::new(HashSet::new()),
        }
    }

    /// Production default: seeds [`KNOWN_COMPROMISED_SIGNERS`] into the
    /// signer blocklist. Callers may extend the list further at boot from
    /// an operator-supplied env var (see `rope-node`'s wiring).
    pub fn with_default_blocklist() -> Self {
        let guard = Self::new();
        for signer in KNOWN_COMPROMISED_SIGNERS {
            guard.block_signer(signer);
        }
        guard
    }

    pub fn input_validator(&self) -> &InputValidator {
        &self.input_validator
    }

    pub fn block_signer(&self, addr: &str) {
        self.blocked_signers.write().insert(normalize_signer(addr));
    }

    pub fn unblock_signer(&self, addr: &str) {
        self.blocked_signers.write().remove(&normalize_signer(addr));
    }

    pub fn is_signer_blocked(&self, addr: &str) -> bool {
        self.blocked_signers.read().contains(&normalize_signer(addr))
    }

    /// Snapshot of the current signer blocklist (lowercased, `0x`-prefixed).
    pub fn blocked_signers(&self) -> Vec<String> {
        self.blocked_signers.read().iter().cloned().collect()
    }

    pub fn block_ip(&self, ip: &str) {
        self.blocked_ips.write().insert(ip.trim().to_string());
    }

    pub fn unblock_ip(&self, ip: &str) {
        self.blocked_ips.write().remove(ip.trim());
    }

    pub fn is_ip_blocked(&self, ip: &str) -> bool {
        self.blocked_ips.read().contains(ip.trim())
    }

    /// Check a wallet/signer parameter pulled straight out of an RPC
    /// request's `params` (e.g. `params[0]` for every Quipu Canon
    /// wallet-keyed method). Returns [`GuardDenial`] iff the address is on
    /// the blocklist.
    pub fn check_signer(&self, wallet: &str) -> Result<(), GuardDenial> {
        if self.is_signer_blocked(wallet) {
            return Err(GuardDenial {
                category: DenyCategory::BlockedSigner,
                subject: normalize_signer(wallet),
            });
        }
        Ok(())
    }

    /// Check a caller's source IP against the block list. Independent of
    /// (and does not replace) rate limiting — this is for IPs that have
    /// been explicitly banned (manually, or by a future STRIKE
    /// auto-escalation), not for the general request-rate budget.
    pub fn check_ip(&self, ip: &str) -> Result<(), GuardDenial> {
        if self.is_ip_blocked(ip) {
            return Err(GuardDenial {
                category: DenyCategory::BlockedIp,
                subject: ip.to_string(),
            });
        }
        Ok(())
    }

    /// Run the input validator against a single field.
    pub fn validate_input(&self, value: &str, field_name: &str) -> Result<(), GuardDenial> {
        self.input_validator.validate(value, field_name)
    }
}

impl Default for RequestGuard {
    fn default() -> Self {
        Self::with_default_blocklist()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_blocklist_contains_known_compromised_key() {
        let guard = RequestGuard::with_default_blocklist();
        assert!(guard.is_signer_blocked("0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195"));
        // Case-insensitive.
        assert!(guard.is_signer_blocked("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"));
        assert!(guard.check_signer("0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195").is_err());
    }

    #[test]
    fn fresh_guard_has_no_blocklist() {
        let guard = RequestGuard::new();
        assert!(!guard.is_signer_blocked("0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195"));
    }

    #[test]
    fn block_and_unblock_signer_roundtrip() {
        let guard = RequestGuard::new();
        let addr = "0x000000000000000000000000000000000000dEaD";
        assert!(!guard.is_signer_blocked(addr));
        guard.block_signer(addr);
        assert!(guard.is_signer_blocked(addr));
        assert!(guard.check_signer(addr).is_err());
        guard.unblock_signer(addr);
        assert!(!guard.is_signer_blocked(addr));
    }

    #[test]
    fn block_and_unblock_ip_roundtrip() {
        let guard = RequestGuard::new();
        assert!(guard.check_ip("203.0.113.7").is_ok());
        guard.block_ip("203.0.113.7");
        assert!(guard.check_ip("203.0.113.7").is_err());
        guard.unblock_ip("203.0.113.7");
        assert!(guard.check_ip("203.0.113.7").is_ok());
    }

    #[test]
    fn input_validator_flags_definite_sql_attack_even_in_chat_field() {
        let v = InputValidator::new();
        assert!(v.validate("hello'; DROP TABLE users;", "message").is_err());
        assert!(v.validate("1' OR 1=1 --", "prompt").is_err());
    }

    #[test]
    fn input_validator_allows_sql_keywords_in_chat_field() {
        let v = InputValidator::new();
        // "alter" and "select" are ordinary English words — must not trip
        // the generic keyword heuristic when the field is chat-classified.
        assert!(v.validate("hello alter, can you select a movie for me?", "message").is_ok());
    }

    #[test]
    fn input_validator_flags_sql_keywords_in_structured_field() {
        let v = InputValidator::new();
        // "sort_column" does not match any CHAT_FIELDS substring, so this
        // goes through the generic SQL-keyword heuristic path.
        assert!(v.validate("SELECT * FROM users", "sort_column").is_err());
    }

    #[test]
    fn is_chat_field_matches_by_substring() {
        // Regression guard for the classification rule itself: any field
        // name containing a CHAT_FIELDS entry as a substring is treated as
        // free text (e.g. "contract_notes" contains "notes").
        assert!(InputValidator::is_chat_field("contract_notes"));
        assert!(InputValidator::is_chat_field("user_message"));
        assert!(!InputValidator::is_chat_field("sort_column"));
    }

    #[test]
    fn input_validator_flags_xss() {
        let v = InputValidator::new();
        assert!(v.validate("<script>alert(1)</script>", "notes").is_err());
        assert!(v.validate("<img src=x onerror=alert(1)>", "notes").is_err());
    }

    #[test]
    fn input_validator_flags_path_traversal() {
        let v = InputValidator::new();
        assert!(v.validate("../../etc/passwd", "path").is_err());
        assert!(v.validate("%2e%2e%2fetc%2fpasswd", "path").is_err());
    }

    #[test]
    fn input_validator_allows_benign_input() {
        let v = InputValidator::new();
        assert!(v.validate("Kibali Gold Mine, Congo DRC", "asset_name").is_ok());
        assert!(v.validate("0x91f884D436858ad221436573BC2cB5117E27e564", "contract").is_ok());
    }

    #[test]
    fn normalize_signer_adds_missing_prefix_and_lowercases() {
        assert_eq!(normalize_signer("60FB32ef3A2381c2Ed71613F34fd56D56fCF4195"), "0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195");
        assert_eq!(normalize_signer(" 0xABCDEF "), "0xabcdef");
    }
}
