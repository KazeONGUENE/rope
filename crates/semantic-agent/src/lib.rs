//! # Datachain Rope SemanticAgent
//!
//! One of the five canonical AI testimony agents on Datachain Rope (per the
//! `/api/v1/ai-agents` endpoint of `dc-explorer`). The SemanticAgent:
//!
//! 1. **Indexes** new knots (Quipu Canon v1.2 §1) discovered via the node's
//!    JSON-RPC surface — `rope_globalStats`, `rope_listStrings`,
//!    `rope_getStringWithKnots`.
//! 2. **Tags** each indexed knot with an `event_type` extracted, on a
//!    best-effort basis, from the knot payload (when available) and/or the
//!    owning string's `kind` (`wallet`, `contract`, `asset`, `did`, `cord`).
//!    Honest caveat: the production rope-node JSON-RPC surface does **not**
//!    expose decrypted payloads (by design — payloads are OES-encrypted and
//!    third-party agents do not hold the wallet key), so the agent
//!    indexes the metadata that IS observable: kind, knot_index, status
//!    (active/tombstone), timestamps, and any payload bytes/hints fed in
//!    out-of-band by enrichers running alongside the node.
//! 3. **Searches** the index over four axes: full-text query (`q`),
//!    `event_type` filter, `creator` (owning string) filter, and time
//!    range — exposed via `GET /v1/search`.
//! 4. **Checkpoints** the index state every `checkpoint_interval` (default
//!    600 s) by computing a deterministic BLAKE3 merkle root over the
//!    sorted set of indexed knot IDs and emitting a signed
//!    [`IndexCheckpointTestimony`] knot via `rope_appendToLedger`.
//!
//! The merkle root commits the agent to the exact set of knots it has
//! observed and indexed; any later disagreement between the agent and
//! reality (e.g. dropped knots, censorship, divergent shards) is provable
//! from the on-chain checkpoint trail. That is what makes the agent's
//! observations *auditable*, in the same sense that a notary's stamp
//! makes a paper testimony auditable.
//!
//! ## Module layout
//!
//! - [`config`] — runtime configuration and CLI parsing
//! - [`indexer`] — pulls knots from the node and writes to tantivy
//! - [`search`] — reads from tantivy
//! - [`checkpoint`] — deterministic merkle root over knot IDs
//! - [`anchor`] — submits signed checkpoints to `rope_appendToLedger`
//! - [`server`] — axum HTTP server (`GET /v1/search`, `GET /v1/health`,
//!   `GET /v1/metrics`, `GET /v1/checkpoint`)

pub mod anchor;
pub mod checkpoint;
pub mod config;
pub mod indexer;
pub mod rpc;
pub mod search;
pub mod server;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use anchor::{Anchor, AnchorOutcome, AnchorSubmitter};
pub use checkpoint::{CheckpointBuilder, IndexCheckpointTestimony, MerkleRoot};
pub use config::{AgentConfig, AgentIdentity};
pub use indexer::{Indexer, IndexerStats};
pub use rpc::{RpcClient, RpcError};
pub use search::{SearchHit, SearchQuery, SearchService};

/// Canonical wallet of the SemanticAgent on Datachain Rope.
///
/// Mirrors the constant exposed in
/// `crates/rope-explorer/src/main.rs::canonical_ai_agents()` so checkpoint
/// knots are anchored on the canonical agent's string. Operators may
/// override this via [`AgentIdentity::wallet`].
pub const CANONICAL_AGENT_WALLET: &str = "0x000000000000000000000000000000000000C001";

/// Canonical agent ID (matches the `/api/v1/ai-agents` `id` field).
pub const CANONICAL_AGENT_ID: &str = "semantic";

/// One indexed knot — the unit produced by [`Indexer`] and consumed by
/// [`SearchService`] / [`CheckpointBuilder`].
///
/// `KnotIndexEntry` is the projection of a Datachain Rope knot that the
/// SemanticAgent commits to. Two entries with the same `knot_id` are
/// the same knot (canonically); differences in any other field are a
/// re-index that supersedes the previous record (tantivy keeps the
/// latest by `(knot_id, indexed_at)`).
///
/// All fields use Quipu Canon v1.2 names — `knot_id`, `string_id`,
/// `string_kind` — never the deprecated v1.0/1.1 aliases.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KnotIndexEntry {
    /// 64-char hex knot ID (the `string_id` returned by
    /// `rope_appendToLedger` and the `string_id` of each entry in
    /// `rope_getStringWithKnots`).
    pub knot_id: String,
    /// Owning string ID — the (kind, id_bytes) tuple's hex id.
    pub string_id: String,
    /// One of `wallet | contract | asset | did | cord`.
    pub string_kind: String,
    /// Best-effort event-type tag (see [`event_type::extract`]).
    pub event_type: String,
    /// Position of this knot on its string (genesis = 0).
    pub knot_index: u64,
    /// `active` or `tombstone` (per Quipu Canon v1.1 §5).
    pub status: String,
    /// Unix-second timestamp of when the agent first indexed this knot.
    pub indexed_at: i64,
    /// Best-effort knot timestamp — falls back to `indexed_at` when the
    /// node response doesn't carry one.
    pub knot_timestamp: i64,
    /// Optional textual rendering of the payload (for full-text search).
    /// Empty for opaque/encrypted payloads.
    pub payload_text: String,
    /// Encrypted payload size in bytes (0 when unknown).
    pub payload_size: u64,
}

impl KnotIndexEntry {
    /// 32-byte digest committing to the canonical (knot_id, string_id,
    /// string_kind, knot_index, status) tuple. Used by the merkle root —
    /// stable across re-indexes that don't change identity fields.
    pub fn identity_digest(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"dcr-semantic-agent/knot-identity/v1\n");
        h.update(self.knot_id.as_bytes());
        h.update(b"\n");
        h.update(self.string_id.as_bytes());
        h.update(b"\n");
        h.update(self.string_kind.as_bytes());
        h.update(b"\n");
        h.update(&self.knot_index.to_be_bytes());
        h.update(b"\n");
        h.update(self.status.as_bytes());
        *h.finalize().as_bytes()
    }
}

/// Best-effort event_type extraction.
pub mod event_type {
    use super::KnotIndexEntry;

    /// Canonical fallback used when nothing better can be derived.
    pub const UNKNOWN: &str = "unknown";

    /// Derive an `event_type` tag from a knot's metadata + (optional)
    /// payload bytes.
    ///
    /// Honest caveat: the rope-node JSON-RPC surface does **not** today
    /// expose decrypted payload contents, so for the vast majority of
    /// production knots the payload-driven branches will not fire and
    /// the `event_type` will fall back to `<string_kind>:<status>`
    /// (e.g. `wallet:active`, `cord:active`, `asset:tombstone`). If a
    /// future rope-node release exposes plaintext payloads, the JSON
    /// branches below will pick up `event_type` / `interaction_type` /
    /// `attestation_kind` automatically — no code change required.
    ///
    /// Recognised input shapes:
    ///   1. Plain JSON object with `event_type` key (most explicit).
    ///   2. Plain JSON object with `interaction_type` key (matches the
    ///      `InteractionRecord` schema in `rope-core`).
    ///   3. Plain JSON object with `metadata.attestation_kind`
    ///      (matches the deployer-attestation pattern in `rope-node`).
    ///   4. Anything else: fall back to `<string_kind>:<status>`.
    pub fn extract(string_kind: &str, status: &str, payload: Option<&[u8]>) -> String {
        if let Some(bytes) = payload {
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) {
                if let Some(s) = json.get("event_type").and_then(|v| v.as_str()) {
                    return s.to_string();
                }
                if let Some(s) = json.get("interaction_type").and_then(|v| v.as_str()) {
                    return s.to_string();
                }
                if let Some(s) = json
                    .get("metadata")
                    .and_then(|m| m.get("attestation_kind"))
                    .and_then(|v| v.as_str())
                {
                    return s.to_string();
                }
            }
        }
        format!("{}:{}", string_kind, status)
    }

    /// Best-effort textual payload rendering for full-text indexing.
    /// Returns an empty string when `payload` is not human-readable.
    pub fn payload_text(payload: Option<&[u8]>) -> String {
        let Some(bytes) = payload else {
            return String::new();
        };
        if bytes.is_empty() {
            return String::new();
        }
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) {
            return json.to_string();
        }
        if let Ok(s) = std::str::from_utf8(bytes) {
            // Only return if reasonably printable — guard against binary
            // payloads that happen to be valid UTF-8.
            let printable = s
                .chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t' | '\r'))
                .count();
            if printable * 100 / s.chars().count().max(1) >= 90 {
                return s.to_string();
            }
        }
        String::new()
    }

    /// Convenience: produce both `event_type` and `payload_text` for a
    /// given knot, used by the indexer.
    pub fn enrich(entry: &mut KnotIndexEntry, payload: Option<&[u8]>) {
        entry.event_type = extract(&entry.string_kind, &entry.status, payload);
        entry.payload_text = payload_text(payload);
        if let Some(p) = payload {
            entry.payload_size = p.len() as u64;
        }
    }
}

/// Top-level handle binding the indexer, search service and anchor
/// submitter together. Cheap to clone — internally everything is
/// `Arc<...>`-shared.
#[derive(Clone)]
pub struct SemanticAgent {
    pub config: Arc<AgentConfig>,
    pub indexer: Arc<Indexer>,
    pub search: Arc<SearchService>,
    pub anchor: Arc<AnchorSubmitter>,
    pub metrics: Arc<RwLock<AgentMetrics>>,
}

impl SemanticAgent {
    /// Wire the four subsystems together. Panics never; returns errors
    /// via `anyhow::Result` so callers can decide whether to retry.
    pub fn new(config: AgentConfig) -> anyhow::Result<Self> {
        let config = Arc::new(config);
        let metrics = Arc::new(RwLock::new(AgentMetrics::default()));
        let rpc = Arc::new(RpcClient::new(config.rpc_url.clone(), config.rpc_timeout)?);
        let search = Arc::new(SearchService::open_or_create(&config.index_path)?);
        let indexer = Arc::new(Indexer::new(
            config.clone(),
            rpc.clone(),
            search.clone(),
            metrics.clone(),
        ));
        let anchor = Arc::new(AnchorSubmitter::new(
            config.clone(),
            rpc.clone(),
            search.clone(),
            metrics.clone(),
        ));
        Ok(Self {
            config,
            indexer,
            search,
            anchor,
            metrics,
        })
    }

    /// Snapshot of the metrics counters — cloned, lock released before
    /// returning.
    pub fn metrics(&self) -> AgentMetrics {
        self.metrics.read().clone()
    }
}

/// Minimal Prometheus-style counters surfaced by `GET /v1/metrics`.
///
/// `last_*` fields make state debuggable from the API alone, without
/// having to read the index off disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentMetrics {
    pub indexed_count: u64,
    pub search_count: u64,
    pub checkpoint_count: u64,
    pub last_indexed_knot_id: Option<String>,
    pub last_indexed_string_id: Option<String>,
    pub last_indexed_at: Option<i64>,
    pub last_checkpoint_at: Option<i64>,
    pub last_checkpoint_root: Option<String>,
    pub last_checkpoint_total_indexed: u64,
    pub last_anchor_knot_id: Option<String>,
    pub last_anchor_at: Option<i64>,
    pub indexer_errors: u64,
    pub anchor_errors: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(knot_id: &str, kind: &str, status: &str) -> KnotIndexEntry {
        KnotIndexEntry {
            knot_id: knot_id.to_string(),
            string_id: "0xabc".to_string(),
            string_kind: kind.to_string(),
            event_type: String::new(),
            knot_index: 1,
            status: status.to_string(),
            indexed_at: 0,
            knot_timestamp: 0,
            payload_text: String::new(),
            payload_size: 0,
        }
    }

    #[test]
    fn identity_digest_is_stable_and_unique() {
        let a = entry("0x01", "wallet", "active");
        let b = entry("0x01", "wallet", "active");
        let c = entry("0x02", "wallet", "active");
        assert_eq!(a.identity_digest(), b.identity_digest());
        assert_ne!(a.identity_digest(), c.identity_digest());
    }

    #[test]
    fn identity_digest_ignores_volatile_fields() {
        let mut a = entry("0x01", "wallet", "active");
        let mut b = entry("0x01", "wallet", "active");
        a.indexed_at = 1;
        b.indexed_at = 999;
        a.payload_text = "hello".into();
        b.payload_text = "world".into();
        assert_eq!(a.identity_digest(), b.identity_digest());
    }

    #[test]
    fn event_type_extracts_explicit_event_type() {
        let payload = br#"{"event_type":"DeployerAttestation"}"#;
        let s = event_type::extract("wallet", "active", Some(payload));
        assert_eq!(s, "DeployerAttestation");
    }

    #[test]
    fn event_type_extracts_interaction_type() {
        let payload = br#"{"interaction_type":"Transfer"}"#;
        let s = event_type::extract("wallet", "active", Some(payload));
        assert_eq!(s, "Transfer");
    }

    #[test]
    fn event_type_extracts_attestation_kind_from_metadata() {
        let payload = br#"{"metadata":{"attestation_kind":"deployer_v1"}}"#;
        let s = event_type::extract("wallet", "active", Some(payload));
        assert_eq!(s, "deployer_v1");
    }

    #[test]
    fn event_type_falls_back_to_kind_status() {
        assert_eq!(event_type::extract("cord", "active", None), "cord:active");
        assert_eq!(
            event_type::extract("wallet", "tombstone", Some(b"raw\x00\x01")),
            "wallet:tombstone"
        );
    }

    #[test]
    fn payload_text_passes_through_json_and_utf8() {
        assert!(!event_type::payload_text(Some(br#"{"a":1}"#)).is_empty());
        assert!(!event_type::payload_text(Some(b"hello world")).is_empty());
        assert_eq!(event_type::payload_text(Some(&[0u8, 1, 2, 3, 4])), "");
        assert_eq!(event_type::payload_text(None), "");
    }
}
