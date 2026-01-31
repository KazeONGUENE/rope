//! RopeAgent Identity Management
//!
//! Provides secure identity binding via Datawallet+ integration,
//! authorization token management, and reputation tracking.

use crate::error::AuthError;
use crate::intent::ActionType;
use rope_crypto::hybrid::HybridSignature;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Datawallet+ Identity
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatawalletIdentity {
    /// Node ID (32 bytes)
    pub node_id: [u8; 32],

    /// Public key (hybrid: Ed25519 + Dilithium)
    pub public_key: Vec<u8>,

    /// Display name
    pub display_name: String,

    /// DID (Decentralized Identifier)
    pub did: String,

    /// Verified status
    pub verified: bool,

    /// KYC level (0-3)
    pub kyc_level: u8,

    /// Creation timestamp
    pub created_at: i64,
}

impl DatawalletIdentity {
    /// Create new identity
    pub fn new(node_id: [u8; 32], public_key: Vec<u8>, display_name: String) -> Self {
        let did = format!("did:datachain:{}", hex::encode(&node_id[..16]));

        Self {
            node_id,
            public_key,
            display_name,
            did,
            verified: false,
            kyc_level: 0,
            created_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Get seed for OES derivation
    pub fn seed(&self) -> &[u8] {
        &self.node_id
    }
}

/// RopeAgent identity with authorization management
#[derive(Clone)]
pub struct RopeAgentIdentity {
    /// Datawallet+ identity
    pub datawallet: DatawalletIdentity,

    /// Agent-specific keypair (derived from datawallet)
    pub agent_keys: AgentKeyPair,

    /// Reputation score (0-100)
    pub reputation: u8,

    /// Verified capabilities
    pub verified_capabilities: Vec<VerifiedCapability>,

    /// Active authorization tokens
    active_tokens: HashMap<[u8; 32], AuthorizationToken>,

    /// Current OES epoch
    current_oes_epoch: u64,
}

impl RopeAgentIdentity {
    /// Create new agent identity from Datawallet+
    pub fn new(datawallet: DatawalletIdentity) -> Self {
        let agent_keys = AgentKeyPair::derive_from_datawallet(&datawallet);

        Self {
            datawallet,
            agent_keys,
            reputation: 50, // Start with neutral reputation
            verified_capabilities: Vec::new(),
            active_tokens: HashMap::new(),
            current_oes_epoch: 0,
        }
    }

    /// Update OES epoch
    pub fn set_oes_epoch(&mut self, epoch: u64) {
        // Invalidate tokens from previous epochs
        self.active_tokens
            .retain(|_, token| token.oes_epoch == epoch);
        self.current_oes_epoch = epoch;
    }

    /// Get current OES epoch
    pub fn current_oes_epoch(&self) -> u64 {
        self.current_oes_epoch
    }

    /// Create authorization token for specific action
    pub fn create_authorization_token(
        &mut self,
        action_type: ActionType,
        value_limit: Option<u64>,
        expires_in: Duration,
    ) -> AuthorizationToken {
        let token_id = Self::generate_token_id();
        let expires_at = chrono::Utc::now().timestamp() + expires_in.as_secs() as i64;

        let token = AuthorizationToken {
            id: token_id,
            owner: self.datawallet.node_id,
            action_type,
            value_limit,
            created_at: chrono::Utc::now().timestamp(),
            expires_at,
            oes_epoch: self.current_oes_epoch,
            used: false,
            signature: self.sign_token(&token_id, expires_at),
        };

        self.active_tokens.insert(token_id, token.clone());
        token
    }

    /// Verify and consume authorization token
    pub fn verify_and_consume_token(
        &mut self,
        token_id: &[u8; 32],
        action: &ActionRequest,
    ) -> Result<(), AuthError> {
        let token = self
            .active_tokens
            .get_mut(token_id)
            .ok_or(AuthError::TokenNotFound)?;

        // Check not expired
        if chrono::Utc::now().timestamp() > token.expires_at {
            return Err(AuthError::TokenExpired);
        }

        // Check not already used
        if token.used {
            return Err(AuthError::TokenAlreadyUsed);
        }

        // Check action type matches
        if !token.action_type.matches(&action.action_type) {
            return Err(AuthError::ActionTypeMismatch);
        }

        // Check value limit
        if let (Some(limit), Some(value)) = (token.value_limit, action.estimated_value_usd) {
            if value > limit {
                return Err(AuthError::ValueLimitExceeded);
            }
        }

        // Check OES epoch (prevent replay across epochs)
        if token.oes_epoch != self.current_oes_epoch {
            return Err(AuthError::EpochMismatch);
        }

        // Mark as used (single-use)
        token.used = true;

        Ok(())
    }

    /// Get active (unused, not expired) token count
    pub fn active_token_count(&self) -> usize {
        let now = chrono::Utc::now().timestamp();
        self.active_tokens
            .values()
            .filter(|t| !t.used && t.expires_at > now)
            .count()
    }

    /// Clean up expired tokens
    pub fn cleanup_expired_tokens(&mut self) {
        let now = chrono::Utc::now().timestamp();
        self.active_tokens.retain(|_, token| token.expires_at > now);
    }

    /// Generate unique token ID
    fn generate_token_id() -> [u8; 32] {
        let uuid = uuid::Uuid::new_v4();
        let timestamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);

        let mut input = Vec::new();
        input.extend_from_slice(uuid.as_bytes());
        input.extend_from_slice(&timestamp.to_le_bytes());

        *blake3::hash(&input).as_bytes()
    }

    /// Sign token data
    fn sign_token(&self, token_id: &[u8; 32], expires_at: i64) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(token_id);
        data.extend_from_slice(&expires_at.to_le_bytes());
        data.extend_from_slice(&self.current_oes_epoch.to_le_bytes());

        // In production, this would use actual hybrid signing
        blake3::hash(&data).as_bytes().to_vec()
    }
}

/// Agent-specific keypair
#[derive(Clone, Debug)]
pub struct AgentKeyPair {
    /// Ed25519 public key
    pub ed25519_public: [u8; 32],
    /// Dilithium public key
    pub dilithium_public: Vec<u8>,
    // Private keys stored securely (not serialized)
}

impl AgentKeyPair {
    /// Derive agent keypair from Datawallet identity
    pub fn derive_from_datawallet(datawallet: &DatawalletIdentity) -> Self {
        // Derive deterministic keys from datawallet
        let ed25519_seed = blake3::hash(&[&datawallet.node_id[..], b"ed25519"].concat());
        let dilithium_seed = blake3::hash(&[&datawallet.node_id[..], b"dilithium"].concat());

        Self {
            ed25519_public: *ed25519_seed.as_bytes(),
            dilithium_public: dilithium_seed.as_bytes().to_vec(),
        }
    }
}

/// Single-use authorization token
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorizationToken {
    /// Token ID
    pub id: [u8; 32],

    /// Owner's node ID
    pub owner: [u8; 32],

    /// Authorized action type
    pub action_type: ActionType,

    /// Maximum value (USD) for this action
    pub value_limit: Option<u64>,

    /// Creation timestamp
    pub created_at: i64,

    /// Expiration timestamp
    pub expires_at: i64,

    /// OES epoch when created
    pub oes_epoch: u64,

    /// Has token been used?
    pub used: bool,

    /// Signature proving authenticity
    pub signature: Vec<u8>,
}

/// Verified capability
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedCapability {
    /// Capability type
    pub capability: String,

    /// Verification timestamp
    pub verified_at: i64,

    /// Verifier identity
    pub verifier: [u8; 32],

    /// Verification proof
    pub proof: Vec<u8>,
}

/// Action request for token verification
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionRequest {
    /// Action ID
    pub id: [u8; 32],

    /// Action type
    pub action_type: ActionType,

    /// Estimated USD value
    pub estimated_value_usd: Option<u64>,

    /// Timestamp
    pub timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity() -> DatawalletIdentity {
        DatawalletIdentity::new([1u8; 32], vec![0u8; 64], "Test User".to_string())
    }

    #[test]
    fn test_create_token() {
        let mut identity = RopeAgentIdentity::new(test_identity());

        let token = identity.create_authorization_token(
            ActionType::Transfer {
                asset: "FAT".to_string(),
            },
            Some(1000),
            Duration::from_secs(3600),
        );

        assert!(!token.used);
        assert_eq!(token.value_limit, Some(1000));
        assert_eq!(identity.active_token_count(), 1);
    }

    #[test]
    fn test_verify_and_consume_token() {
        let mut identity = RopeAgentIdentity::new(test_identity());

        let token = identity.create_authorization_token(
            ActionType::Transfer {
                asset: "FAT".to_string(),
            },
            Some(1000),
            Duration::from_secs(3600),
        );

        let action = ActionRequest {
            id: [0u8; 32],
            action_type: ActionType::Transfer {
                asset: "FAT".to_string(),
            },
            estimated_value_usd: Some(500),
            timestamp: chrono::Utc::now().timestamp(),
        };

        // First use should succeed
        assert!(identity.verify_and_consume_token(&token.id, &action).is_ok());

        // Second use should fail
        assert!(matches!(
            identity.verify_and_consume_token(&token.id, &action),
            Err(AuthError::TokenAlreadyUsed)
        ));
    }

    #[test]
    fn test_value_limit_exceeded() {
        let mut identity = RopeAgentIdentity::new(test_identity());

        let token = identity.create_authorization_token(
            ActionType::Transfer {
                asset: "FAT".to_string(),
            },
            Some(100),
            Duration::from_secs(3600),
        );

        let action = ActionRequest {
            id: [0u8; 32],
            action_type: ActionType::Transfer {
                asset: "FAT".to_string(),
            },
            estimated_value_usd: Some(500), // Exceeds limit
            timestamp: chrono::Utc::now().timestamp(),
        };

        assert!(matches!(
            identity.verify_and_consume_token(&token.id, &action),
            Err(AuthError::ValueLimitExceeded)
        ));
    }
}
