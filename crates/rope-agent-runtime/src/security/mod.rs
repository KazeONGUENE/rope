//! Security modules for RopeAgent
//!
//! Provides comprehensive security features:
//! - **Cerber v3.0**: Enhanced security (ported from AlterOS)
//! - **Rate Limiting**: Token bucket and tiered rate limiters
//! - **Input Validation**: Address, email, URL, and data validation
//!
//! ## CERBER v3.0 Features
//!
//! - SQL injection detection
//! - XSS attack detection
//! - Path traversal prevention
//! - Request signature validation (HMAC-SHA256)
//! - LLM output sanitization
//! - API key management with tiers
//! - Blockchain transaction validation
//! - Threat detection and auto-blocking
//!
//! Organization: Braincities Lab / Datachain Foundation

mod cerber;
mod rate_limiter;
mod validation;

pub use cerber::*;
pub use rate_limiter::*;
pub use validation::*;
