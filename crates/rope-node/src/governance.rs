//! Master-node governance, founder authority, and signed-action ACL.
//!
//! Per `.cursor/rules/master-node-governance.mdc`:
//! - 4 RPC slots (BLUE/GREEN/rpc-1/rpc-2) are master nodes
//! - 2 witnesses (val-1/val-2) are recognized member nodes
//! - The Datachain founder identity holds L0 authority
//!
//! The on-disk source of truth is `master-nodes.toml`, normally
//! `/home/ubuntu/datachain-rope/deploy/config/master-nodes.toml`.
//!
//! Signed governance actions are gated on this module's
//! `verify_action_signature` returning `Authorized::*`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Top-level structure parsed from `master-nodes.toml`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MasterNodeRegistry {
    #[serde(default)]
    pub schema_version: String,
    #[serde(default)]
    pub chain_id: u64,
    #[serde(default)]
    pub last_updated: String,
    #[serde(default)]
    pub authority: String,
    #[serde(default)]
    pub master_nodes: Vec<NodeEntry>,
    #[serde(default)]
    pub member_nodes: Vec<NodeEntry>,
    #[serde(default)]
    pub founder: FounderAuthority,
    #[serde(default)]
    pub replay: ReplayWindow,
}

/// One row of the [[master_nodes]] / [[member_nodes]] table.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeEntry {
    pub slot: String,
    pub hostname: String,
    pub provider: String,
    pub region: String,
    pub ip: String,
    pub role: String,
    pub node_id: String,
    pub pubkey_ed25519: String,
}

/// `[founder]` block from the registry.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FounderAuthority {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub canonical_email: String,
    #[serde(default)]
    pub local_part_aliases: Vec<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub founder_keys: Vec<String>,
    #[serde(default)]
    pub founder_dids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayWindow {
    pub window_secs: i64,
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self { window_secs: 300 }
    }
}

/// Possible outcomes of a governance signature check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authorized {
    Founder,
    MasterNode { slot: String },
    Denied(String),
}

impl Authorized {
    pub fn is_founder(&self) -> bool {
        matches!(self, Self::Founder)
    }
    pub fn is_authorized(&self) -> bool {
        !matches!(self, Self::Denied(_))
    }
}

/// Action being requested. Used both for canonical-bytes generation (the
/// payload that must be signed) and for ACL routing (suspend can be done
/// by master, isolate/erase only by founder).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum GovernanceAction {
    #[serde(rename = "rope_suspendNode")]
    Suspend {
        node_id: String,
        reason: String,
        ttl_secs: u64,
        issued_at: String,
        nonce: String,
    },
    #[serde(rename = "rope_isolateNode")]
    Isolate {
        node_id: String,
        reason: String,
        issued_at: String,
        nonce: String,
    },
    #[serde(rename = "rope_eraseNode")]
    Erase {
        node_id: String,
        reason: String,
        issued_at: String,
        nonce: String,
    },
}

impl GovernanceAction {
    /// The bytes that must be signed (canonical JSON, sorted keys).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // serde_json::to_vec uses struct field order; we sort for determinism
        // by routing through serde_json::Value.
        let v = serde_json::to_value(self).unwrap_or_default();
        canonical_json_bytes(&v)
    }
    pub fn issued_at(&self) -> &str {
        match self {
            Self::Suspend { issued_at, .. }
            | Self::Isolate { issued_at, .. }
            | Self::Erase { issued_at, .. } => issued_at,
        }
    }
    pub fn nonce(&self) -> &str {
        match self {
            Self::Suspend { nonce, .. }
            | Self::Isolate { nonce, .. }
            | Self::Erase { nonce, .. } => nonce,
        }
    }
    pub fn target_node_id(&self) -> &str {
        match self {
            Self::Suspend { node_id, .. }
            | Self::Isolate { node_id, .. }
            | Self::Erase { node_id, .. } => node_id,
        }
    }
    pub fn requires_founder(&self) -> bool {
        matches!(self, Self::Isolate { .. } | Self::Erase { .. })
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::Suspend { .. } => "rope_suspendNode",
            Self::Isolate { .. } => "rope_isolateNode",
            Self::Erase { .. } => "rope_eraseNode",
        }
    }
}

/// One entry of the in-memory governance log (also append-only on disk).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceLogEntry {
    pub timestamp: String,
    pub action: GovernanceAction,
    pub authorized_as: String,
    pub signer_pubkey: String,
    pub signature: String,
}

/// Manages the loaded registry + replay-protection nonces + log.
pub struct GovernanceManager {
    registry: RwLock<MasterNodeRegistry>,
    seen_nonces: RwLock<Vec<String>>,
    log: RwLock<Vec<GovernanceLogEntry>>,
    log_path: String,
    enforce: bool,
}

impl GovernanceManager {
    pub fn from_file(path: &str, log_path: &str, enforce: bool) -> anyhow::Result<Arc<Self>> {
        let registry: MasterNodeRegistry = if Path::new(path).exists() {
            let body = fs::read_to_string(path)?;
            toml::from_str(&body)?
        } else {
            tracing::warn!(
                "governance: master-nodes.toml not found at {path}; loading empty registry \
                 (governance RPC methods will refuse all actions)"
            );
            MasterNodeRegistry::default()
        };
        Ok(Arc::new(Self {
            registry: RwLock::new(registry),
            seen_nonces: RwLock::new(Vec::with_capacity(256)),
            log: RwLock::new(Vec::new()),
            log_path: expand_tilde(log_path),
            enforce,
        }))
    }

    pub fn registry_snapshot(&self) -> MasterNodeRegistry {
        self.registry.read().clone()
    }

    pub fn enforce(&self) -> bool {
        self.enforce
    }

    pub fn recent_log(&self, limit: usize) -> Vec<GovernanceLogEntry> {
        let log = self.log.read();
        let start = log.len().saturating_sub(limit);
        log[start..].to_vec()
    }

    /// Verify an Ed25519 signature over the canonical bytes of the action,
    /// then check whether the signer pubkey is a founder key or a master
    /// node key. Also enforces the replay window and nonce uniqueness.
    pub fn verify_action_signature(
        &self,
        action: &GovernanceAction,
        signature_hex: &str,
        pubkey_hex: &str,
    ) -> Authorized {
        if !self.enforce {
            return Authorized::Founder; // dev-only escape hatch
        }

        let sig_bytes = match hex::decode(signature_hex.trim_start_matches("0x")) {
            Ok(b) if b.len() == 64 => b,
            _ => return Authorized::Denied("invalid signature length".into()),
        };
        let pk_bytes = match hex::decode(pubkey_hex.trim_start_matches("0x")) {
            Ok(b) if b.len() == 32 => b,
            _ => return Authorized::Denied("invalid pubkey length".into()),
        };

        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let signature = Signature::from_bytes(&sig_arr);

        let mut pk_arr = [0u8; 32];
        pk_arr.copy_from_slice(&pk_bytes);
        let verifying_key = match VerifyingKey::from_bytes(&pk_arr) {
            Ok(k) => k,
            Err(e) => return Authorized::Denied(format!("invalid Ed25519 pubkey: {e}")),
        };

        let canonical = action.canonical_bytes();
        if verifying_key.verify(&canonical, &signature).is_err() {
            return Authorized::Denied("signature does not verify".into());
        }

        // Replay protection
        let registry = self.registry.read();
        let issued_at = match DateTime::parse_from_rfc3339(action.issued_at()) {
            Ok(t) => t.with_timezone(&Utc),
            Err(_) => return Authorized::Denied("issued_at must be RFC3339 UTC".into()),
        };
        let now = Utc::now();
        let window = Duration::seconds(registry.replay.window_secs);
        if (now - issued_at).abs() > window {
            return Authorized::Denied(format!(
                "issued_at outside replay window (±{}s)",
                registry.replay.window_secs
            ));
        }

        {
            let mut nonces = self.seen_nonces.write();
            if nonces.iter().any(|n| n == action.nonce()) {
                return Authorized::Denied("nonce replay".into());
            }
            nonces.push(action.nonce().to_string());
            if nonces.len() > 1024 {
                let drop = nonces.len() - 1024;
                nonces.drain(..drop);
            }
        }

        // Authority check
        let pk_hex_lc = pubkey_hex.trim_start_matches("0x").to_lowercase();
        let is_founder = registry
            .founder
            .founder_keys
            .iter()
            .any(|k| k.trim_start_matches("0x").to_lowercase() == pk_hex_lc);
        if is_founder {
            return Authorized::Founder;
        }
        if action.requires_founder() {
            return Authorized::Denied(format!("{} requires a founder signature", action.name()));
        }
        if let Some(node) = registry
            .master_nodes
            .iter()
            .find(|n| n.pubkey_ed25519.trim_start_matches("0x").to_lowercase() == pk_hex_lc)
        {
            return Authorized::MasterNode {
                slot: node.slot.clone(),
            };
        }
        Authorized::Denied("signer is neither a founder key nor a master-node key".into())
    }

    /// Record a successfully-authorized action both in memory and to disk.
    pub fn record_action(
        &self,
        action: &GovernanceAction,
        authorized_as: &str,
        pubkey_hex: &str,
        signature_hex: &str,
    ) {
        let entry = GovernanceLogEntry {
            timestamp: Utc::now().to_rfc3339(),
            action: action.clone(),
            authorized_as: authorized_as.to_string(),
            signer_pubkey: pubkey_hex.to_string(),
            signature: signature_hex.to_string(),
        };
        self.log.write().push(entry.clone());
        if let Some(parent) = Path::new(&self.log_path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(line) = serde_json::to_string(&entry) {
            use std::io::Write;
            if let Ok(mut f) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log_path)
            {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{rest}", home.to_string_lossy());
        }
    }
    p.to_string()
}

/// Canonical JSON: sorted keys at every object level.
fn canonical_json_bytes(v: &serde_json::Value) -> Vec<u8> {
    use serde_json::Value;
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(&Value::String((*k).clone()), out);
                    out.push(':');
                    write(&o[*k], out);
                }
                out.push('}');
            }
        }
    }
    let mut s = String::new();
    write(v, &mut s);
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Deterministic test signing key (do NOT use any real seed here).
    fn test_signing_key(seed: u8) -> SigningKey {
        let bytes = [seed; 32];
        SigningKey::from_bytes(&bytes)
    }

    fn mk_registry_with_founder(founder_pk: [u8; 32]) -> MasterNodeRegistry {
        MasterNodeRegistry {
            schema_version: "1.0".into(),
            chain_id: 271828,
            authority: "Test".into(),
            founder: FounderAuthority {
                name: "Test Founder".into(),
                founder_keys: vec![hex::encode(founder_pk)],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn founder_can_erase() {
        let signing = test_signing_key(0xAB);
        let registry = mk_registry_with_founder(signing.verifying_key().to_bytes());
        let mgr = Arc::new(GovernanceManager {
            registry: RwLock::new(registry),
            seen_nonces: RwLock::new(Vec::new()),
            log: RwLock::new(Vec::new()),
            log_path: "/tmp/test-governance.log".into(),
            enforce: true,
        });
        let action = GovernanceAction::Erase {
            node_id: "abcd".into(),
            reason: "test".into(),
            issued_at: Utc::now().to_rfc3339(),
            nonce: "n1".into(),
        };
        let sig = signing.sign(&action.canonical_bytes());
        let result = mgr.verify_action_signature(
            &action,
            &hex::encode(sig.to_bytes()),
            &hex::encode(signing.verifying_key().to_bytes()),
        );
        assert!(result.is_founder(), "got {:?}", result);
    }

    #[test]
    fn unknown_key_denied() {
        let signing = test_signing_key(0xCD);
        let mgr = Arc::new(GovernanceManager {
            registry: RwLock::new(MasterNodeRegistry::default()),
            seen_nonces: RwLock::new(Vec::new()),
            log: RwLock::new(Vec::new()),
            log_path: "/tmp/test-governance.log".into(),
            enforce: true,
        });
        let action = GovernanceAction::Suspend {
            node_id: "abcd".into(),
            reason: "test".into(),
            ttl_secs: 60,
            issued_at: Utc::now().to_rfc3339(),
            nonce: "n2".into(),
        };
        let sig = signing.sign(&action.canonical_bytes());
        let result = mgr.verify_action_signature(
            &action,
            &hex::encode(sig.to_bytes()),
            &hex::encode(signing.verifying_key().to_bytes()),
        );
        assert!(matches!(result, Authorized::Denied(_)), "got {:?}", result);
    }

    #[test]
    fn master_can_suspend_but_not_erase() {
        let master = test_signing_key(0x11);
        let mut registry = MasterNodeRegistry::default();
        registry.master_nodes.push(NodeEntry {
            slot: "test-master".into(),
            pubkey_ed25519: hex::encode(master.verifying_key().to_bytes()),
            ..Default::default()
        });
        let mgr = Arc::new(GovernanceManager {
            registry: RwLock::new(registry),
            seen_nonces: RwLock::new(Vec::new()),
            log: RwLock::new(Vec::new()),
            log_path: "/tmp/test-governance.log".into(),
            enforce: true,
        });

        // suspend: should pass
        let suspend = GovernanceAction::Suspend {
            node_id: "abcd".into(),
            reason: "test".into(),
            ttl_secs: 60,
            issued_at: Utc::now().to_rfc3339(),
            nonce: "n_suspend".into(),
        };
        let sig = master.sign(&suspend.canonical_bytes());
        let r = mgr.verify_action_signature(
            &suspend,
            &hex::encode(sig.to_bytes()),
            &hex::encode(master.verifying_key().to_bytes()),
        );
        assert!(matches!(r, Authorized::MasterNode { .. }), "got {:?}", r);

        // erase: should be denied
        let erase = GovernanceAction::Erase {
            node_id: "abcd".into(),
            reason: "test".into(),
            issued_at: Utc::now().to_rfc3339(),
            nonce: "n_erase".into(),
        };
        let sig = master.sign(&erase.canonical_bytes());
        let r = mgr.verify_action_signature(
            &erase,
            &hex::encode(sig.to_bytes()),
            &hex::encode(master.verifying_key().to_bytes()),
        );
        assert!(matches!(r, Authorized::Denied(_)), "got {:?}", r);
    }
}
