//! # CERBER v3.0 - Enhanced Security Module for Datachain Rope
//!
//! Ported from AlterOS enhanced security system.
//! Provides comprehensive security features:
//!
//! - **Input Validation**: SQL injection, XSS, path traversal detection
//! - **Request Signing**: HMAC-SHA256 signature validation
//! - **LLM Output Sanitization**: Secrets/API key redaction
//! - **API Key Management**: Tier-based rate limiting
//! - **Threat Detection**: Pattern-based threat identification
//!
//! ## Integration with AlterOS
//!
//! This module is designed to work seamlessly with AlterOS security APIs
//! at `https://alteros.io/api/*`
//!
//! Organization: Braincities Lab / Datachain Foundation
//! Lead Engineer: Kazé A. ONGUENE

use dashmap::DashMap;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// CERBER version
pub const CERBER_VERSION: &str = "3.0.0";

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Security level for different environments
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecurityLevel {
    Development,
    Staging,
    Production,
}

impl Default for SecurityLevel {
    fn default() -> Self {
        Self::Production
    }
}

/// Enhanced security configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CerberConfig {
    /// Security environment
    pub security_level: SecurityLevel,

    /// Allowed CORS origins
    pub allowed_origins: HashSet<String>,

    /// Endpoints requiring signature
    pub signed_endpoints: HashSet<String>,

    /// Signature validity duration
    pub signature_validity_seconds: u64,

    /// API key rotation period (days)
    pub api_key_rotation_days: u32,

    /// Maximum request body size (bytes)
    pub max_request_body_size: usize,

    /// Maximum JSON nesting depth
    pub max_json_depth: usize,

    /// Maximum prompt length for LLM
    pub max_prompt_length: usize,

    /// Maximum output tokens for LLM
    pub max_output_tokens: usize,

    /// Require wallet signature for blockchain ops
    pub require_wallet_signature: bool,

    /// Maximum transaction value (USD)
    pub max_transaction_value_usd: f64,

    /// Transaction cooldown (seconds)
    pub transaction_cooldown_seconds: u64,
}

impl Default for CerberConfig {
    fn default() -> Self {
        let mut allowed_origins = HashSet::new();
        allowed_origins.insert("https://alteros.io".to_string());
        allowed_origins.insert("https://app.alteros.io".to_string());
        allowed_origins.insert("https://datachain.foundation".to_string());
        allowed_origins.insert("https://app.datachain.foundation".to_string());
        allowed_origins.insert("https://braincities.io".to_string());

        let mut signed_endpoints = HashSet::new();
        signed_endpoints.insert("/api/dc/swap/build".to_string());
        signed_endpoints.insert("/api/dc/swap/execute".to_string());
        signed_endpoints.insert("/api/wallet/transfer".to_string());
        signed_endpoints.insert("/api/marketplace/purchase".to_string());
        signed_endpoints.insert("/admin/".to_string());

        Self {
            security_level: SecurityLevel::Production,
            allowed_origins,
            signed_endpoints,
            signature_validity_seconds: 300, // 5 minutes
            api_key_rotation_days: 90,
            max_request_body_size: 10 * 1024 * 1024, // 10MB
            max_json_depth: 10,
            max_prompt_length: 32_000,
            max_output_tokens: 8_192,
            require_wallet_signature: true,
            max_transaction_value_usd: 10_000.0,
            transaction_cooldown_seconds: 10,
        }
    }
}

// =============================================================================
// ERRORS
// =============================================================================

/// CERBER security errors
#[derive(Debug, Error)]
pub enum CerberError {
    #[error("SQL injection detected in field: {0}")]
    SqlInjection(String),

    #[error("XSS attack detected in field: {0}")]
    XssAttack(String),

    #[error("Path traversal attempt in field: {0}")]
    PathTraversal(String),

    #[error("Invalid request signature")]
    InvalidSignature,

    #[error("Signature expired")]
    SignatureExpired,

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Invalid API key")]
    InvalidApiKey,

    #[error("API key expired")]
    ApiKeyExpired,

    #[error("Insufficient API key scope")]
    InsufficientScope,

    #[error("CORS origin not allowed: {0}")]
    CorsNotAllowed(String),

    #[error("Request body too large: {0} > {1}")]
    BodyTooLarge(usize, usize),

    #[error("JSON depth exceeded: {0} > {1}")]
    JsonDepthExceeded(usize, usize),

    #[error("Prompt too long: {0} > {1}")]
    PromptTooLong(usize, usize),

    #[error("Transaction value too high: ${0} > ${1}")]
    TransactionTooLarge(f64, f64),

    #[error("Transaction cooldown active")]
    TransactionCooldown,

    #[error("Replay attack detected: nonce already used")]
    ReplayAttack,

    #[error("Threat detected: {0}")]
    ThreatDetected(String),
}

// =============================================================================
// INPUT VALIDATION
// =============================================================================

/// Fields that typically contain natural language (chat, prompts, etc.)
/// These fields should NOT be checked for SQL injection as they often contain
/// words like "alter", "select", "drop" in normal conversation.
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
];

/// Enhanced input validator with attack pattern detection
///
/// ## Chat-Aware Validation
///
/// CERBER v3.0 distinguishes between natural language (chat) and structured data:
/// - Chat fields (message, prompt, etc.) skip SQL keyword detection
/// - This prevents false positives like "hello **alter** what time is it?"
/// - XSS and path traversal checks still apply to all fields
pub struct EnhancedInputValidator {
    /// SQL patterns for structured data (forms, APIs)
    sql_patterns: Vec<Regex>,
    /// SQL patterns that indicate actual attacks (used for all contexts)
    sql_attack_patterns: Vec<Regex>,
    xss_patterns: Vec<Regex>,
    path_patterns: Vec<Regex>,
}

impl EnhancedInputValidator {
    /// Create new validator with compiled patterns
    pub fn new() -> Self {
        // SQL injection patterns - keywords that are common in attacks
        // NOTE: These can cause false positives in chat (e.g., "alter" is Alter's name!)
        let sql_patterns = vec![
            r"(?i)(\b(SELECT|INSERT|UPDATE|DELETE|DROP|UNION|ALTER|CREATE|TRUNCATE|EXEC|EXECUTE)\b)",
            "(?i)(\\bOR\\b\\s+\\d+\\s*=\\s*\\d+)",
            "(?i)(\\bAND\\b\\s+\\d+\\s*=\\s*\\d+)",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

        // SQL attack patterns - these are ALWAYS malicious, even in chat
        // Much more specific patterns that indicate actual SQL injection attempts
        let sql_attack_patterns = vec![
            // Comment-based injection
            r"(--|#|/\*|\*/)",
            // Null byte injection
            "\\x00",
            // Classic 1=1 injection
            "(?i)(\\bOR\\b\\s+1\\s*=\\s*1)",
            "(?i)(\\bAND\\b\\s+1\\s*=\\s*1)",
            // UNION SELECT (very specific attack pattern)
            "(?i)(\\bUNION\\s+SELECT\\b)",
            // DROP TABLE/DATABASE
            "(?i)(\\bDROP\\s+(TABLE|DATABASE)\\b)",
            // Semicolon followed by SQL keyword
            "(?i)(;\\s*(SELECT|INSERT|UPDATE|DELETE|DROP|UNION|ALTER|CREATE|TRUNCATE|EXEC))",
            // String termination followed by SQL
            "(?i)('\\s*(OR|AND|UNION|SELECT)\\b)",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

        // XSS patterns
        let xss_patterns = vec![
            "(?i)(<script[^>]*>.*?</script>)",
            "(?i)(javascript:)",
            "(?i)(on\\w+\\s*=)",
            "(?i)(<iframe[^>]*>)",
            "(?i)(<object[^>]*>)",
            "(?i)(<embed[^>]*>)",
            "(?i)(<link[^>]*>)",
            "(?i)(<meta[^>]*>)",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

        // Path traversal patterns
        let path_patterns = vec![
            r"\.\./",
            r"\.\.\\",
            "(?i)(%2e%2e%2f)",
            "(?i)(%252e%252e%252f)",
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

    /// Check for SQL injection (all patterns - may have false positives in chat)
    pub fn check_sql_injection(&self, value: &str) -> bool {
        self.sql_patterns.iter().any(|p| p.is_match(value))
    }

    /// Check for definite SQL attack patterns (safe for all contexts)
    pub fn check_sql_attack(&self, value: &str) -> bool {
        self.sql_attack_patterns.iter().any(|p| p.is_match(value))
    }

    /// Check for XSS
    pub fn check_xss(&self, value: &str) -> bool {
        self.xss_patterns.iter().any(|p| p.is_match(value))
    }

    /// Check for path traversal
    pub fn check_path_traversal(&self, value: &str) -> bool {
        self.path_patterns.iter().any(|p| p.is_match(value))
    }

    /// Check if a field name is a chat/natural language field
    pub fn is_chat_field(field_name: &str) -> bool {
        let lower = field_name.to_lowercase();
        CHAT_FIELDS.iter().any(|f| lower.contains(f))
    }

    /// Validate input for all attack patterns
    ///
    /// ## Arguments
    /// - `value`: The input value to validate
    /// - `field_name`: Name of the field (used to detect chat fields)
    ///
    /// ## Chat-Aware Logic
    /// - If `field_name` matches a chat field (message, prompt, etc.), only
    ///   definite attack patterns are checked (not SQL keywords)
    /// - This allows natural language like "hello alter" or "can you select"
    pub fn validate(&self, value: &str, field_name: &str) -> Result<(), CerberError> {
        let is_chat = Self::is_chat_field(field_name);
        self.validate_with_context(value, field_name, is_chat)
    }

    /// Validate input with explicit chat context
    ///
    /// ## Arguments
    /// - `value`: The input value to validate
    /// - `field_name`: Name of the field for error reporting
    /// - `is_chat`: If true, skip aggressive SQL keyword checks
    pub fn validate_with_context(
        &self,
        value: &str,
        field_name: &str,
        is_chat: bool,
    ) -> Result<(), CerberError> {
        // Always check for definite SQL attacks (even in chat)
        if self.check_sql_attack(value) {
            return Err(CerberError::SqlInjection(field_name.to_string()));
        }

        // Only check SQL keywords for non-chat fields
        if !is_chat && self.check_sql_injection(value) {
            return Err(CerberError::SqlInjection(field_name.to_string()));
        }

        // XSS check (applies to all fields)
        if self.check_xss(value) {
            return Err(CerberError::XssAttack(field_name.to_string()));
        }
        if self.check_path_traversal(value) {
            return Err(CerberError::PathTraversal(field_name.to_string()));
        }
        Ok(())
    }

    /// Sanitize string by removing dangerous characters
    pub fn sanitize(&self, value: &str) -> String {
        value
            .replace('\0', "")
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
            .replace('\'', "&#x27;")
    }
}

impl Default for EnhancedInputValidator {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// REQUEST SIGNATURE VALIDATION
// =============================================================================

/// Request signature validator using HMAC-SHA256
pub struct RequestSignatureValidator {
    secret_key: Vec<u8>,
    validity_seconds: u64,
}

impl RequestSignatureValidator {
    /// Create new validator with secret key
    pub fn new(secret_key: &[u8], validity_seconds: u64) -> Self {
        Self {
            secret_key: secret_key.to_vec(),
            validity_seconds,
        }
    }

    /// Validate request signature
    pub fn validate(
        &self,
        timestamp: u64,
        signature: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<(), CerberError> {
        // Check timestamp freshness
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if now.abs_diff(timestamp) > self.validity_seconds {
            return Err(CerberError::SignatureExpired);
        }

        // Compute expected signature
        let message = format!("{}{}{}{}", timestamp, method.to_uppercase(), path, body);

        type HmacSha256 = Hmac<Sha256>;
        let mut mac =
            HmacSha256::new_from_slice(&self.secret_key).expect("HMAC accepts any key length");
        mac.update(message.as_bytes());

        let expected = hex::encode(mac.finalize().into_bytes());

        // Constant-time comparison
        if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
            return Err(CerberError::InvalidSignature);
        }

        Ok(())
    }

    /// Generate signature for testing
    pub fn sign(&self, method: &str, path: &str, body: &str) -> (u64, String) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let message = format!("{}{}{}{}", timestamp, method.to_uppercase(), path, body);

        type HmacSha256 = Hmac<Sha256>;
        let mut mac =
            HmacSha256::new_from_slice(&self.secret_key).expect("HMAC accepts any key length");
        mac.update(message.as_bytes());

        (timestamp, hex::encode(mac.finalize().into_bytes()))
    }
}

/// Constant-time byte comparison
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

// =============================================================================
// LLM OUTPUT SANITIZATION
// =============================================================================

/// LLM output sanitizer to prevent data leakage
pub struct LLMOutputSanitizer {
    patterns: Vec<(Regex, &'static str)>,
}

impl LLMOutputSanitizer {
    /// Create new sanitizer with default patterns
    pub fn new() -> Self {
        let patterns = vec![
            // API keys / secrets
            (
                "(?i)(api[_-]?key|secret[_-]?key|password|token)\\s*[:=]\\s*['\"]?[\\w-]{20,}",
                "CREDENTIALS",
            ),
            // Database URLs
            (
                "(postgres://|mysql://|mongodb://|redis://)[^\\s]+",
                "DATABASE_URL",
            ),
            // Private keys (PEM)
            (
                "-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
                "PRIVATE_KEY",
            ),
            // Wallet private keys (hex)
            ("0x[a-fA-F0-9]{64}", "WALLET_KEY"),
            // JWT tokens
            (
                "eyJ[a-zA-Z0-9_-]*\\.eyJ[a-zA-Z0-9_-]*\\.[a-zA-Z0-9_-]*",
                "JWT_TOKEN",
            ),
            // AWS keys
            ("AKIA[A-Z0-9]{16}", "AWS_KEY"),
            // GitHub tokens
            ("ghp_[a-zA-Z0-9]{36}", "GITHUB_TOKEN"),
            // Anthropic API keys
            ("sk-ant-[a-zA-Z0-9-]+", "ANTHROPIC_KEY"),
            // OpenAI API keys
            ("sk-[a-zA-Z0-9]{48}", "OPENAI_KEY"),
        ]
        .into_iter()
        .filter_map(|(p, name)| Regex::new(p).ok().map(|r| (r, name)))
        .collect();

        Self { patterns }
    }

    /// Sanitize output by redacting sensitive patterns
    pub fn sanitize(&self, output: &str) -> (String, Vec<String>) {
        let mut result = output.to_string();
        let mut redacted = Vec::new();

        for (pattern, name) in &self.patterns {
            if pattern.is_match(&result) {
                redacted.push(name.to_string());
                result = pattern.replace_all(&result, "[REDACTED]").to_string();
            }
        }

        (result, redacted)
    }

    /// Check if output contains sensitive data
    pub fn contains_sensitive(&self, output: &str) -> bool {
        self.patterns.iter().any(|(p, _)| p.is_match(output))
    }
}

impl Default for LLMOutputSanitizer {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// API KEY MANAGEMENT
// =============================================================================

/// API key tier
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiKeyTier {
    Free,
    Pro,
    Enterprise,
    DatachainRope,
}

impl ApiKeyTier {
    /// Get rate limits for tier
    pub fn rate_limits(&self) -> TierRateLimits {
        match self {
            ApiKeyTier::Free => TierRateLimits {
                requests_per_minute: 30,
                requests_per_day: 1_000,
            },
            ApiKeyTier::Pro => TierRateLimits {
                requests_per_minute: 120,
                requests_per_day: 10_000,
            },
            ApiKeyTier::Enterprise => TierRateLimits {
                requests_per_minute: 600,
                requests_per_day: 100_000,
            },
            ApiKeyTier::DatachainRope => TierRateLimits {
                requests_per_minute: 1_000,
                requests_per_day: 500_000,
            },
        }
    }
}

/// Rate limits for a tier
#[derive(Clone, Debug)]
pub struct TierRateLimits {
    pub requests_per_minute: u32,
    pub requests_per_day: u32,
}

/// API key information
#[derive(Clone, Debug)]
pub struct ApiKeyInfo {
    /// Key hash (never store raw key)
    pub key_hash: String,
    /// Tier
    pub tier: ApiKeyTier,
    /// Owner identifier
    pub owner_id: String,
    /// Allowed scopes
    pub scopes: HashSet<String>,
    /// Creation timestamp
    pub created_at: u64,
    /// Expiration timestamp
    pub expires_at: Option<u64>,
}

/// API key manager
pub struct ApiKeyManager {
    /// Stored keys (hash -> info)
    keys: DashMap<String, ApiKeyInfo>,
    /// Key rotation period
    rotation_days: u32,
}

impl ApiKeyManager {
    /// Create new manager
    pub fn new(rotation_days: u32) -> Self {
        Self {
            keys: DashMap::new(),
            rotation_days,
        }
    }

    /// Hash an API key
    pub fn hash_key(key: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Generate new API key
    pub fn generate_key(&self, owner_id: &str, tier: ApiKeyTier) -> (String, String) {
        use rand::Rng;

        let random: [u8; 32] = rand::thread_rng().gen();
        let random_str = hex::encode(random);
        let tier_str = match tier {
            ApiKeyTier::Free => "free",
            ApiKeyTier::Pro => "pro",
            ApiKeyTier::Enterprise => "enterprise",
            ApiKeyTier::DatachainRope => "rope",
        };

        let full_key = format!("dcr_{}_{}", tier_str, random_str);
        let key_hash = Self::hash_key(&full_key);
        let prefix = format!("dcr_{}_{}...", tier_str, &random_str[..8]);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let expires_at = if self.rotation_days > 0 {
            Some(now + (self.rotation_days as u64 * 24 * 3600))
        } else {
            None
        };

        let mut scopes = HashSet::new();
        scopes.insert("api:read".to_string());
        scopes.insert("api:write".to_string());

        self.keys.insert(
            key_hash.clone(),
            ApiKeyInfo {
                key_hash,
                tier,
                owner_id: owner_id.to_string(),
                scopes,
                created_at: now,
                expires_at,
            },
        );

        (full_key, prefix)
    }

    /// Validate API key
    pub fn validate(&self, key: &str) -> Result<ApiKeyInfo, CerberError> {
        let hash = Self::hash_key(key);

        let info = self
            .keys
            .get(&hash)
            .map(|r| r.clone())
            .ok_or(CerberError::InvalidApiKey)?;

        // Check expiration
        if let Some(expires) = info.expires_at {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            if now > expires {
                return Err(CerberError::ApiKeyExpired);
            }
        }

        Ok(info)
    }

    /// Check if key has scope
    pub fn has_scope(&self, key_info: &ApiKeyInfo, scope: &str) -> bool {
        // Check exact match
        if key_info.scopes.contains(scope) {
            return true;
        }

        // Check wildcard
        if key_info.scopes.contains("*") {
            return true;
        }

        // Check prefix wildcard (e.g., "api:*" matches "api:read")
        for s in &key_info.scopes {
            if s.ends_with(":*") {
                let prefix = &s[..s.len() - 1];
                if scope.starts_with(prefix) {
                    return true;
                }
            }
        }

        false
    }
}

impl Default for ApiKeyManager {
    fn default() -> Self {
        Self::new(90) // 90 day rotation
    }
}

// =============================================================================
// BLOCKCHAIN TRANSACTION VALIDATOR
// =============================================================================

/// Blockchain transaction validator
pub struct BlockchainValidator {
    config: CerberConfig,
    /// Used nonces per wallet
    used_nonces: DashMap<String, HashSet<String>>,
    /// Last transaction time per wallet
    last_tx_time: DashMap<String, Instant>,
}

impl BlockchainValidator {
    /// Create new validator
    pub fn new(config: CerberConfig) -> Self {
        Self {
            config,
            used_nonces: DashMap::new(),
            last_tx_time: DashMap::new(),
        }
    }

    /// Validate transaction request
    pub fn validate_transaction(
        &self,
        wallet: &str,
        value_usd: f64,
        nonce: &str,
    ) -> Result<(), CerberError> {
        // Check value limit
        if value_usd > self.config.max_transaction_value_usd {
            return Err(CerberError::TransactionTooLarge(
                value_usd,
                self.config.max_transaction_value_usd,
            ));
        }

        // Check cooldown
        if let Some(last_time) = self.last_tx_time.get(wallet) {
            let elapsed = last_time.elapsed();
            if elapsed < Duration::from_secs(self.config.transaction_cooldown_seconds) {
                return Err(CerberError::TransactionCooldown);
            }
        }

        // Check nonce (replay prevention)
        let mut nonces = self.used_nonces.entry(wallet.to_string()).or_default();
        if nonces.contains(nonce) {
            return Err(CerberError::ReplayAttack);
        }

        // Record nonce and time
        nonces.insert(nonce.to_string());
        self.last_tx_time.insert(wallet.to_string(), Instant::now());

        // Cleanup old nonces (keep last 1000)
        if nonces.len() > 1000 {
            let to_remove: Vec<_> = nonces.iter().take(500).cloned().collect();
            for n in to_remove {
                nonces.remove(&n);
            }
        }

        Ok(())
    }
}

// =============================================================================
// THREAT DETECTOR
// =============================================================================

/// Threat detection levels
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThreatLevel {
    None = 0,
    Low = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
    ActiveAttack = 5,
}

/// Threat detection result
#[derive(Clone, Debug)]
pub struct ThreatResult {
    pub is_threat: bool,
    pub level: ThreatLevel,
    pub score: u32,
    pub patterns: Vec<String>,
    pub recommendations: Vec<String>,
}

/// Real-time threat detector
pub struct ThreatDetector {
    /// Known attacker IPs
    blocked_ips: DashMap<String, Instant>,
    /// Request counts for rate analysis
    request_counts: DashMap<String, Vec<Instant>>,
    /// Input validator
    input_validator: EnhancedInputValidator,
}

impl ThreatDetector {
    /// Create new detector
    pub fn new() -> Self {
        Self {
            blocked_ips: DashMap::new(),
            request_counts: DashMap::new(),
            input_validator: EnhancedInputValidator::new(),
        }
    }

    /// Analyze request for threats
    pub fn analyze(
        &self,
        source_ip: &str,
        endpoint: &str,
        method: &str,
        body: Option<&str>,
    ) -> ThreatResult {
        let mut score = 0u32;
        let mut patterns = Vec::new();
        let mut recommendations = Vec::new();

        // Check if IP is blocked
        if self.blocked_ips.contains_key(source_ip) {
            return ThreatResult {
                is_threat: true,
                level: ThreatLevel::Critical,
                score: 100,
                patterns: vec!["blocked_ip".to_string()],
                recommendations: vec!["IP has been blocked due to previous threats".to_string()],
            };
        }

        // Check body for attacks
        if let Some(body) = body {
            if self.input_validator.check_sql_injection(body) {
                score += 40;
                patterns.push("sql_injection".to_string());
                recommendations.push("SQL injection attempt detected".to_string());
            }

            if self.input_validator.check_xss(body) {
                score += 30;
                patterns.push("xss_attack".to_string());
                recommendations.push("XSS attack attempt detected".to_string());
            }

            if self.input_validator.check_path_traversal(body) {
                score += 35;
                patterns.push("path_traversal".to_string());
                recommendations.push("Path traversal attempt detected".to_string());
            }
        }

        // Check for suspicious request patterns
        self.record_request(source_ip);
        let rpm = self.requests_per_minute(source_ip);

        if rpm > 100 {
            score += 20;
            patterns.push("high_request_rate".to_string());
            recommendations.push(format!("High request rate: {} req/min", rpm));
        }

        // Determine threat level
        let level = match score {
            0 => ThreatLevel::None,
            1..=20 => ThreatLevel::Low,
            21..=40 => ThreatLevel::Medium,
            41..=70 => ThreatLevel::High,
            71..=90 => ThreatLevel::Critical,
            _ => ThreatLevel::ActiveAttack,
        };

        // Auto-block critical threats
        if level >= ThreatLevel::Critical {
            self.blocked_ips.insert(source_ip.to_string(), Instant::now());
        }

        ThreatResult {
            is_threat: score > 0,
            level,
            score,
            patterns,
            recommendations,
        }
    }

    /// Record request for rate analysis
    fn record_request(&self, source_ip: &str) {
        let mut requests = self.request_counts.entry(source_ip.to_string()).or_default();
        requests.push(Instant::now());

        // Keep only last 5 minutes
        let cutoff = Instant::now() - Duration::from_secs(300);
        requests.retain(|t| *t > cutoff);
    }

    /// Get requests per minute
    fn requests_per_minute(&self, source_ip: &str) -> usize {
        if let Some(requests) = self.request_counts.get(source_ip) {
            let cutoff = Instant::now() - Duration::from_secs(60);
            requests.iter().filter(|t| **t > cutoff).count()
        } else {
            0
        }
    }

    /// Manually block an IP
    pub fn block_ip(&self, ip: &str) {
        self.blocked_ips.insert(ip.to_string(), Instant::now());
    }

    /// Unblock an IP
    pub fn unblock_ip(&self, ip: &str) {
        self.blocked_ips.remove(ip);
    }

    /// Check if IP is blocked
    pub fn is_blocked(&self, ip: &str) -> bool {
        self.blocked_ips.contains_key(ip)
    }
}

impl Default for ThreatDetector {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// CERBER AGENT - MAIN ORCHESTRATOR
// =============================================================================

/// CERBER Security Agent for Datachain Rope
pub struct Cerber {
    /// Configuration
    pub config: CerberConfig,
    /// Input validator
    pub input_validator: EnhancedInputValidator,
    /// Request signature validator
    pub signature_validator: Option<RequestSignatureValidator>,
    /// LLM output sanitizer
    pub llm_sanitizer: LLMOutputSanitizer,
    /// API key manager
    pub api_key_manager: ApiKeyManager,
    /// Blockchain validator
    pub blockchain_validator: BlockchainValidator,
    /// Threat detector
    pub threat_detector: ThreatDetector,
}

impl Cerber {
    /// Create new CERBER agent
    pub fn new(config: CerberConfig) -> Self {
        let signature_validator = None; // Set via set_signing_secret

        Self {
            blockchain_validator: BlockchainValidator::new(config.clone()),
            config,
            input_validator: EnhancedInputValidator::new(),
            signature_validator,
            llm_sanitizer: LLMOutputSanitizer::new(),
            api_key_manager: ApiKeyManager::default(),
            threat_detector: ThreatDetector::new(),
        }
    }

    /// Set signing secret
    pub fn set_signing_secret(&mut self, secret: &[u8]) {
        self.signature_validator = Some(RequestSignatureValidator::new(
            secret,
            self.config.signature_validity_seconds,
        ));
    }

    /// Validate origin for CORS
    pub fn validate_origin(&self, origin: &str) -> Result<(), CerberError> {
        // Allow localhost in development
        if self.config.security_level == SecurityLevel::Development {
            if origin.starts_with("http://localhost") || origin.starts_with("http://127.0.0.1") {
                return Ok(());
            }
        }

        if !self.config.allowed_origins.contains(origin) {
            return Err(CerberError::CorsNotAllowed(origin.to_string()));
        }

        Ok(())
    }

    /// Full request validation
    pub fn validate_request(
        &self,
        source_ip: &str,
        origin: Option<&str>,
        endpoint: &str,
        method: &str,
        body: Option<&str>,
        api_key: Option<&str>,
        signature: Option<(&str, u64)>, // (signature, timestamp)
    ) -> Result<Option<ApiKeyInfo>, CerberError> {
        // 1. Threat detection
        let threat = self.threat_detector.analyze(source_ip, endpoint, method, body);
        if threat.level >= ThreatLevel::High {
            return Err(CerberError::ThreatDetected(threat.patterns.join(", ")));
        }

        // 2. CORS validation
        if let Some(origin) = origin {
            self.validate_origin(origin)?;
        }

        // 3. Body validation
        if let Some(body) = body {
            if body.len() > self.config.max_request_body_size {
                return Err(CerberError::BodyTooLarge(
                    body.len(),
                    self.config.max_request_body_size,
                ));
            }

            self.input_validator.validate(body, "body")?;
        }

        // 4. Signature validation (if required)
        let requires_signature = self
            .config
            .signed_endpoints
            .iter()
            .any(|e| endpoint.starts_with(e));

        if requires_signature {
            if let (Some(validator), Some((sig, ts))) = (&self.signature_validator, signature) {
                validator.validate(ts, sig, method, endpoint, body.unwrap_or(""))?;
            } else if self.signature_validator.is_some() {
                return Err(CerberError::InvalidSignature);
            }
        }

        // 5. API key validation
        if let Some(key) = api_key {
            let info = self.api_key_manager.validate(key)?;
            return Ok(Some(info));
        }

        Ok(None)
    }

    /// Sanitize LLM output
    pub fn sanitize_llm_output(&self, output: &str) -> (String, Vec<String>) {
        self.llm_sanitizer.sanitize(output)
    }

    /// Validate prompt length
    pub fn validate_prompt(&self, prompt: &str) -> Result<(), CerberError> {
        if prompt.len() > self.config.max_prompt_length {
            return Err(CerberError::PromptTooLong(
                prompt.len(),
                self.config.max_prompt_length,
            ));
        }
        Ok(())
    }

    /// Validate blockchain transaction
    pub fn validate_transaction(
        &self,
        wallet: &str,
        value_usd: f64,
        nonce: &str,
    ) -> Result<(), CerberError> {
        self.blockchain_validator
            .validate_transaction(wallet, value_usd, nonce)
    }

    /// Generate API key for owner
    pub fn generate_api_key(&self, owner_id: &str, tier: ApiKeyTier) -> (String, String) {
        self.api_key_manager.generate_key(owner_id, tier)
    }

    /// Get security headers
    pub fn security_headers() -> HashMap<String, String> {
        let mut headers = HashMap::new();

        // HSTS
        headers.insert(
            "Strict-Transport-Security".to_string(),
            "max-age=31536000; includeSubDomains; preload".to_string(),
        );

        // CSP
        headers.insert(
            "Content-Security-Policy".to_string(),
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; frame-ancestors 'none'".to_string(),
        );

        // Standard security headers
        headers.insert("X-Content-Type-Options".to_string(), "nosniff".to_string());
        headers.insert("X-Frame-Options".to_string(), "DENY".to_string());
        headers.insert("X-XSS-Protection".to_string(), "1; mode=block".to_string());
        headers.insert(
            "Referrer-Policy".to_string(),
            "strict-origin-when-cross-origin".to_string(),
        );
        headers.insert(
            "Permissions-Policy".to_string(),
            "camera=(), microphone=(), geolocation=(), payment=(self)".to_string(),
        );

        // Cross-origin policies
        headers.insert(
            "Cross-Origin-Embedder-Policy".to_string(),
            "require-corp".to_string(),
        );
        headers.insert(
            "Cross-Origin-Opener-Policy".to_string(),
            "same-origin".to_string(),
        );
        headers.insert(
            "Cross-Origin-Resource-Policy".to_string(),
            "same-origin".to_string(),
        );

        headers
    }
}

impl Default for Cerber {
    fn default() -> Self {
        Self::new(CerberConfig::default())
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_validation() {
        let validator = EnhancedInputValidator::new();

        // SQL injection - keyword patterns
        assert!(validator.check_sql_injection("SELECT * FROM users"));
        assert!(validator.check_sql_injection("1; DROP TABLE users;--"));
        assert!(!validator.check_sql_injection("Hello world"));

        // SQL attack patterns (always detected)
        assert!(validator.check_sql_attack("1 OR 1=1"));
        assert!(validator.check_sql_attack("UNION SELECT * FROM users"));
        assert!(validator.check_sql_attack("'; DROP TABLE users;--"));
        assert!(!validator.check_sql_attack("hello alter")); // Not an attack

        // XSS
        assert!(validator.check_xss("<script>alert('xss')</script>"));
        assert!(validator.check_xss("onclick=alert(1)"));
        assert!(!validator.check_xss("Normal text"));

        // Path traversal
        assert!(validator.check_path_traversal("../../../etc/passwd"));
        assert!(validator.check_path_traversal("..\\windows\\system32"));
        assert!(!validator.check_path_traversal("/valid/path"));
    }

    #[test]
    fn test_chat_aware_validation() {
        let validator = EnhancedInputValidator::new();

        // Chat fields should allow natural language with SQL keywords
        // These are common in conversation with "Alter" (the AI's name)
        assert!(validator.validate("hello alter what time is it?", "message").is_ok());
        assert!(validator.validate("Can you select a restaurant?", "prompt").is_ok());
        assert!(validator.validate("When do you drop new updates?", "query").is_ok());
        assert!(validator.validate("Who created you?", "content").is_ok());
        assert!(validator.validate("Can you delete old files?", "user_message").is_ok());

        // Non-chat fields should still block SQL keywords
        assert!(validator.validate("SELECT * FROM users", "username").is_err());
        assert!(validator.validate("DROP TABLE users", "email").is_err());

        // Actual attacks should be blocked in ALL contexts (including chat)
        assert!(validator.validate("1 OR 1=1", "message").is_err());
        assert!(validator.validate("'; DROP TABLE users;--", "prompt").is_err());
        assert!(validator.validate("UNION SELECT * FROM passwords", "query").is_err());

        // XSS should be blocked in all contexts
        assert!(validator.validate("<script>alert(1)</script>", "message").is_err());
    }

    #[test]
    fn test_is_chat_field() {
        assert!(EnhancedInputValidator::is_chat_field("message"));
        assert!(EnhancedInputValidator::is_chat_field("user_message"));
        assert!(EnhancedInputValidator::is_chat_field("prompt"));
        assert!(EnhancedInputValidator::is_chat_field("system_prompt"));
        assert!(EnhancedInputValidator::is_chat_field("query"));
        assert!(EnhancedInputValidator::is_chat_field("content"));
        
        assert!(!EnhancedInputValidator::is_chat_field("username"));
        assert!(!EnhancedInputValidator::is_chat_field("email"));
        assert!(!EnhancedInputValidator::is_chat_field("password"));
        assert!(!EnhancedInputValidator::is_chat_field("id"));
    }

    #[test]
    fn test_signature_validation() {
        let validator = RequestSignatureValidator::new(b"test_secret", 300);

        let (ts, sig) = validator.sign("POST", "/api/test", r#"{"data":1}"#);
        assert!(validator
            .validate(ts, &sig, "POST", "/api/test", r#"{"data":1}"#)
            .is_ok());

        // Wrong signature
        assert!(validator
            .validate(ts, "wrong_signature", "POST", "/api/test", r#"{"data":1}"#)
            .is_err());
    }

    #[test]
    fn test_llm_sanitizer() {
        let sanitizer = LLMOutputSanitizer::new();

        let fake_key = format!("sk_{}_{}", "live", "12345678901234567890123456");
        let test_input = format!("Here is my API key: api_key={}", fake_key);
        let (output, redacted) = sanitizer.sanitize(&test_input);
        assert!(output.contains("[REDACTED]"));
        assert!(!redacted.is_empty());

        // Clean output
        let (output, redacted) = sanitizer.sanitize("Hello, this is a normal response.");
        assert!(!output.contains("[REDACTED]"));
        assert!(redacted.is_empty());
    }

    #[test]
    fn test_api_key_management() {
        let manager = ApiKeyManager::new(90);

        let (full_key, _prefix) = manager.generate_key("user123", ApiKeyTier::DatachainRope);

        // Validate key
        let info = manager.validate(&full_key).unwrap();
        assert_eq!(info.tier, ApiKeyTier::DatachainRope);
        assert_eq!(info.owner_id, "user123");

        // Invalid key
        assert!(manager.validate("invalid_key").is_err());
    }

    #[test]
    fn test_threat_detector() {
        let detector = ThreatDetector::new();

        // Normal request
        let result = detector.analyze("192.168.1.1", "/api/test", "GET", None);
        assert!(!result.is_threat);

        // SQL injection
        let result = detector.analyze(
            "192.168.1.2",
            "/api/test",
            "POST",
            Some("SELECT * FROM users"),
        );
        assert!(result.is_threat);
        assert!(result.patterns.contains(&"sql_injection".to_string()));

        // Block IP
        detector.block_ip("192.168.1.100");
        let result = detector.analyze("192.168.1.100", "/api/test", "GET", None);
        assert!(result.is_threat);
        assert_eq!(result.level, ThreatLevel::Critical);
    }

    #[test]
    fn test_cerber_full() {
        let cerber = Cerber::default();

        // Valid request
        let result = cerber.validate_request(
            "192.168.1.1",
            Some("https://alteros.io"),
            "/api/test",
            "GET",
            None,
            None,
            None,
        );
        assert!(result.is_ok());

        // Invalid origin
        let result = cerber.validate_request(
            "192.168.1.1",
            Some("https://malicious.com"),
            "/api/test",
            "GET",
            None,
            None,
            None,
        );
        assert!(result.is_err());
    }
}
