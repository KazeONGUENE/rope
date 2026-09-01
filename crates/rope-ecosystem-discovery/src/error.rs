//! Top-level error type for the discovery crate.
//!
//! Rules:
//!
//! - Every failable operation returns `DiscoveryResult<T>`.
//! - `DiscoveryError` is `Display + std::error::Error`. Formatting is
//!   stable enough to grep for in logs (e.g. `WriterIo(…)` prefix).
//! - Errors DO NOT panic the daemon by default. The binary catches at
//!   the top level and exits with a non-zero code, but scanners that
//!   fail are logged and skipped so a single broken feed doesn't take
//!   down the whole discovery run.

use std::fmt;

pub type DiscoveryResult<T> = Result<T, DiscoveryError>;

#[derive(Debug)]
pub enum DiscoveryError {
    /// Config file could not be read / parsed.
    Config(String),
    /// Handover scanner could not walk the rules directory.
    HandoverScan(String),
    /// On-chain scanner failed a network / parse call.
    OnchainScan(String),
    /// Partner API scanner failed a network / parse call.
    PartnerApiScan(String),
    /// Entry rejected by client-side validation before write.
    Validation(String),
    /// Atomic writer I/O error.
    WriterIo(String),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoveryError::Config(msg) => write!(f, "Config({})", msg),
            DiscoveryError::HandoverScan(msg) => write!(f, "HandoverScan({})", msg),
            DiscoveryError::OnchainScan(msg) => write!(f, "OnchainScan({})", msg),
            DiscoveryError::PartnerApiScan(msg) => write!(f, "PartnerApiScan({})", msg),
            DiscoveryError::Validation(msg) => write!(f, "Validation({})", msg),
            DiscoveryError::WriterIo(msg) => write!(f, "WriterIo({})", msg),
        }
    }
}

impl std::error::Error for DiscoveryError {}

impl From<serde_json::Error> for DiscoveryError {
    fn from(e: serde_json::Error) -> Self {
        DiscoveryError::WriterIo(format!("serde_json: {}", e))
    }
}

impl From<std::io::Error> for DiscoveryError {
    fn from(e: std::io::Error) -> Self {
        DiscoveryError::WriterIo(format!("io: {}", e))
    }
}
