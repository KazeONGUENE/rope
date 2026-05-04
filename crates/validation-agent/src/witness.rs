//! Testimony emission — the agent's job once a knot has verified.
//!
//! For each knot whose verification returned [`VerificationOutcome::Valid`]
//! the agent constructs a [`ValidationTestimony`], signs it with its
//! own hybrid key, and submits it via `rope_appendToLedger` against
//! its canonical wallet (`0x…C004`).
//!
//! The testimony content is canonical-JSON-serialized BEFORE signing,
//! and the resulting signature is encoded in the metadata as hex so
//! downstream consumers (DCScan testimony cache, audit tooling, etc.)
//! can re-verify it without ambiguity.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use rope_crypto::hybrid::{HybridPublicKey, HybridSigner};

use crate::knot::Knot;
use crate::rpc::{JsonRpcError, RopeRpcClient};
use crate::verify::VerificationResult;

/// Canonical testimony shape emitted by the ValidationAgent for every
/// successfully-validated knot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationTestimony {
    /// Identifier of the canonical agent (`"validation"`).
    pub agent_id: String,

    /// Human-readable agent name.
    pub agent_name: String,

    /// Crate semantic version that produced this testimony.
    pub agent_version: String,

    /// Knot id we validated.
    pub knot_id: String,

    /// Knot index at the time of observation.
    pub knot_index: u64,

    /// Source layer that produced the knot.
    pub knot_source: String,

    /// Algorithm that was used (`mldsa65+ed25519`, `ed25519`, …).
    pub sig_algo: String,

    /// Wall-clock cost of the cryptographic verification, in
    /// microseconds.
    pub validation_time_us: u128,

    /// Unix-second timestamp of the witness moment.
    pub witness_timestamp: i64,

    /// Optional opaque metadata bag — we use it to surface the
    /// original signing-message length and a brief note string. The
    /// shape is intentionally JSON so the testimony schema stays
    /// versionable.
    pub validation_metadata: Value,
}

impl ValidationTestimony {
    /// Build a testimony from a verified knot + its
    /// [`VerificationResult`]. Panics if the result is not `Valid`,
    /// because emitting a testimony for a non-valid knot would be a
    /// protocol violation. The agent control loop is responsible for
    /// gating this — see `agent.rs`.
    pub fn from_verified(knot: &Knot, result: &VerificationResult) -> Self {
        assert!(
            result.sig_valid,
            "ValidationTestimony::from_verified called with non-valid result"
        );
        Self {
            agent_id: crate::VALIDATION_AGENT_ID.to_string(),
            agent_name: crate::VALIDATION_AGENT_NAME.to_string(),
            agent_version: crate::VALIDATION_AGENT_VERSION.to_string(),
            knot_id: knot.knot_id.clone(),
            knot_index: knot.knot_index,
            knot_source: knot.source.as_str().to_string(),
            sig_algo: result.sig_algo.as_str().to_string(),
            validation_time_us: result.validation_time_us,
            witness_timestamp: chrono::Utc::now().timestamp(),
            validation_metadata: json!({
                "signing_message_len": knot.signing_message.len(),
                "validated_at": result.validated_at,
            }),
        }
    }

    /// Canonical JSON byte representation used as the signing message
    /// for the testimony itself. Stable field order via
    /// `serde_json::to_vec` on a typed value.
    pub fn canonical_signing_bytes(&self) -> Vec<u8> {
        // We hash through BLAKE3 to get a fixed-size deterministic
        // representation regardless of how the underlying serializer
        // orders keys at runtime.
        let raw = serde_json::to_vec(self).unwrap_or_default();
        blake3::hash(&raw).as_bytes().to_vec()
    }
}

/// Submitter that takes a verified knot, builds a testimony, signs
/// it with the agent's hybrid key, and forwards it to the local
/// rope-node via `rope_appendToLedger`.
///
/// Hand-rolled `Debug` because `rope_crypto::hybrid::HybridSigner`
/// holds raw secret-key material and intentionally does not derive
/// `Debug` to avoid accidental disclosure in logs.
pub struct WitnessSubmitter<C: RopeRpcClient + ?Sized> {
    client: Arc<C>,
    signer: Arc<HybridSigner>,
    pub_key: HybridPublicKey,
    wallet_address: String,
}

impl<C: RopeRpcClient + ?Sized> std::fmt::Debug for WitnessSubmitter<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WitnessSubmitter")
            .field("client", &self.client)
            .field("signer", &"<redacted HybridSigner>")
            .field("pubkey_ed25519_hex", &hex::encode(self.pub_key.ed25519))
            .field("wallet_address", &self.wallet_address)
            .finish()
    }
}

impl<C: RopeRpcClient + ?Sized> WitnessSubmitter<C> {
    /// Construct a submitter bound to the given hybrid signing key.
    pub fn new(client: Arc<C>, signer: Arc<HybridSigner>, wallet_address: String) -> Self {
        let pub_key = signer.public_key();
        Self {
            client,
            signer,
            pub_key,
            wallet_address,
        }
    }

    /// Get the canonical testimony submitter wallet address.
    pub fn wallet_address(&self) -> &str {
        &self.wallet_address
    }

    /// Get the agent's hybrid public key — exposed for tests so they
    /// can re-verify the testimony's signature out of band.
    pub fn public_key(&self) -> &HybridPublicKey {
        &self.pub_key
    }

    /// Sign and submit a testimony for `(knot, result)`. Returns the
    /// raw `rope_appendToLedger` response (typically `{ index, hash }`)
    /// so callers can include the testimony's anchor id in their
    /// metrics. Errors are returned unmodified.
    pub async fn submit(
        &self,
        knot: &Knot,
        result: &VerificationResult,
    ) -> Result<Value, JsonRpcError> {
        debug_assert!(result.sig_valid, "submit() called with non-valid result");
        if !result.sig_valid {
            return Err(JsonRpcError::Malformed {
                method: "rope_appendToLedger".to_string(),
                detail: "refusing to submit testimony for non-valid result".to_string(),
            });
        }
        let testimony = ValidationTestimony::from_verified(knot, result);
        let signing_bytes = testimony.canonical_signing_bytes();
        let signature = self.signer.sign(&signing_bytes);

        let interaction = json!({
            "interaction_type": "TestimonySubmission",
            "description": format!(
                "ValidationAgent witness for knot {} ({})",
                testimony.knot_id, testimony.sig_algo
            ),
            "metadata": {
                "agent_id": testimony.agent_id,
                "agent_version": testimony.agent_version,
                "knot_id": testimony.knot_id,
                "knot_index": testimony.knot_index,
                "knot_source": testimony.knot_source,
                "sig_algo": testimony.sig_algo,
                "validation_time_us": testimony.validation_time_us.to_string(),
                "witness_timestamp": testimony.witness_timestamp.to_string(),
                "validation_metadata": testimony.validation_metadata,
                "testimony_signature_ed25519": hex::encode(&signature.ed25519_sig),
                "testimony_signature_dilithium3": hex::encode(&signature.dilithium_sig),
                "testimony_signing_digest": hex::encode(&signing_bytes),
                "testimony_pubkey_ed25519": hex::encode(self.pub_key.ed25519),
            },
        });

        tracing::info!(
            target: "validation_agent::witness",
            knot_id = %testimony.knot_id,
            knot_index = testimony.knot_index,
            sig_algo = %testimony.sig_algo,
            "submitting ValidationTestimony to {}",
            self.wallet_address,
        );

        self.client
            .append_to_ledger(&self.wallet_address, interaction)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knot::{Knot, KnotSource};
    use crate::rpc::mock::MockRpcClient;
    use crate::verify::{KnotVerifier, VerificationOutcome};
    use rope_crypto::hybrid::{HybridSigner, HybridVerifier};

    fn signed_knot(message: &[u8]) -> (Knot, VerificationResult) {
        let (signer, pk) = HybridSigner::generate();
        let sig = signer.sign(message);
        let knot =
            Knot::new("0xkk", 7, KnotSource::CordAnchor, message.to_vec()).with_signature(pk, sig);
        let result = KnotVerifier::new().verify(&knot);
        assert_eq!(result.outcome, VerificationOutcome::Valid);
        (knot, result)
    }

    #[test]
    fn testimony_carries_expected_fields() {
        let (knot, result) = signed_knot(b"payload-1");
        let t = ValidationTestimony::from_verified(&knot, &result);
        assert_eq!(t.agent_id, crate::VALIDATION_AGENT_ID);
        assert_eq!(t.agent_name, crate::VALIDATION_AGENT_NAME);
        assert_eq!(t.knot_id, "0xkk");
        assert_eq!(t.knot_index, 7);
        assert_eq!(t.knot_source, "cord_anchor");
        assert_eq!(t.sig_algo, "mldsa65+ed25519");
        assert!(t.validation_time_us > 0);
    }

    #[test]
    fn canonical_signing_bytes_are_deterministic_for_same_input() {
        let (knot, result) = signed_knot(b"payload-2");
        let t1 = ValidationTestimony::from_verified(&knot, &result);
        let t2 = ValidationTestimony {
            // Force witness_timestamp + validation_time_us equal so we
            // measure structural determinism, not RNG.
            witness_timestamp: t1.witness_timestamp,
            validation_time_us: t1.validation_time_us,
            ..t1.clone()
        };
        assert_eq!(t1.canonical_signing_bytes(), t2.canonical_signing_bytes());
    }

    #[tokio::test]
    async fn submit_signs_and_calls_append_to_ledger() {
        let (knot, result) = signed_knot(b"payload-3");
        let mock = Arc::new(MockRpcClient::new());
        mock.enqueue_ok(
            "rope_appendToLedger",
            json!({"index": 1, "hash": "0xanchor"}),
        );

        let (signer, _) = HybridSigner::generate();
        let signer = Arc::new(signer);
        let submitter = WitnessSubmitter::new(
            mock.clone() as Arc<dyn RopeRpcClient>,
            signer.clone(),
            crate::VALIDATION_AGENT_WALLET.to_string(),
        );

        let resp = submitter.submit(&knot, &result).await.unwrap();
        assert_eq!(resp["hash"], "0xanchor");
        assert_eq!(mock.count("rope_appendToLedger"), 1);

        // Inspect the call: owner must be the canonical wallet, the
        // interaction must have type TestimonySubmission, and the
        // signature in metadata must verify against the agent's
        // pubkey when applied to the canonical signing digest.
        let calls = mock.calls();
        let call = calls
            .iter()
            .find(|c| c.method == "rope_appendToLedger")
            .unwrap();
        let owner = call.params.get(0).and_then(|v| v.as_str()).unwrap();
        assert_eq!(owner, crate::VALIDATION_AGENT_WALLET);
        let interaction = call.params.get(1).unwrap();
        assert_eq!(
            interaction.get("interaction_type").and_then(|v| v.as_str()),
            Some("TestimonySubmission")
        );

        let metadata = interaction.get("metadata").unwrap();
        let ed_hex = metadata
            .get("testimony_signature_ed25519")
            .and_then(|v| v.as_str())
            .unwrap();
        let dil_hex = metadata
            .get("testimony_signature_dilithium3")
            .and_then(|v| v.as_str())
            .unwrap();
        let digest_hex = metadata
            .get("testimony_signing_digest")
            .and_then(|v| v.as_str())
            .unwrap();
        let ed_bytes = hex::decode(ed_hex).unwrap();
        let dil_bytes = hex::decode(dil_hex).unwrap();
        let digest_bytes = hex::decode(digest_hex).unwrap();
        let testimony_sig = rope_crypto::hybrid::HybridSignature::new(
            ed_bytes.clone().try_into().unwrap(),
            dil_bytes.clone(),
        );
        let ok = HybridVerifier::verify(submitter.public_key(), &digest_bytes, &testimony_sig)
            .expect("verifier must not error on legitimate signed payload");
        assert!(
            ok,
            "testimony signature must verify against the agent's published pubkey"
        );
    }
}
