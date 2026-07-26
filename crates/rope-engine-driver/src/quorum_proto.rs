//! Wire types for the propose → attest → commit round that gates every
//! new EVM block on real, independent, multi-machine agreement instead
//! of one node's local timer.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestRequest {
    pub round: u64,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestResponse {
    pub pubkey_hex: String,
    pub status: String,
    pub block_number: u64,
    pub block_hash: String,
    /// Present only when `status == "VALID"`.
    pub signature_hex: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateEntry {
    pub pubkey_hex: String,
    pub signature_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRequest {
    pub round: u64,
    pub payload: Value,
    pub certificate: Vec<CertificateEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitResponse {
    pub ok: bool,
    pub block_number: u64,
    pub finalized_hash: Option<String>,
    pub reason: Option<String>,
}
