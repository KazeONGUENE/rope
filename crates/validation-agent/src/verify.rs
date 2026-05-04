//! Post-quantum knot signature verifier.
//!
//! The verifier is intentionally a thin, stateless adapter over
//! [`rope_crypto::hybrid::HybridVerifier`]. It exists so the agent's
//! control loop, metrics, and tests can reason about a single typed
//! [`VerificationResult`] regardless of which signature path actually
//! ran:
//!
//! * **Hybrid (primary)** — the creator public key carries a
//!   Dilithium3 component and the knot's signature carries a
//!   Dilithium3 component. Both Ed25519 AND Dilithium3 must verify.
//!   This is the canonical post-quantum path — identifier
//!   `mldsa65+ed25519`.
//!
//! * **Ed25519 fallback** — the creator public key has no Dilithium3
//!   component (and the signature also carries no Dilithium bytes).
//!   We accept the classical-only signature with a downgrade warning,
//!   matching the policy in `HybridVerifier::verify`.
//!
//! * **Skipped** — the knot did not present any signature material at
//!   all. This is the current state of EVM-shape cord anchors; the
//!   agent does NOT count these as either validated or rejected, and
//!   does NOT emit a testimony for them.

use std::time::Instant;

use rope_crypto::hybrid::HybridVerifier;
use serde::{Deserialize, Serialize};

use crate::knot::Knot;

/// Algorithm tag stamped onto a verification result. Plain strings are
/// used in testimony metadata so external auditors don't need to share
/// our enum definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigAlgo {
    /// Hybrid Ed25519 + ML-DSA-65 (CRYSTALS-Dilithium3, NIST PQ-3).
    /// Both signatures verified.
    Mldsa65Hybrid,

    /// Classical Ed25519 only — accepted via the fallback path because
    /// the creator did not advertise a Dilithium3 public key.
    Ed25519Only,

    /// No signature material was present on the knot. Verification was
    /// skipped — see module docs.
    None,
}

impl SigAlgo {
    /// Stable string used in testimony JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mldsa65Hybrid => "mldsa65+ed25519",
            Self::Ed25519Only => "ed25519",
            Self::None => "none",
        }
    }
}

/// Tri-state verification outcome (intentionally distinct from a plain
/// `bool` so the agent metrics can separate `skipped` from `rejected`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    /// Signature material was present and cryptographically verified.
    Valid,
    /// Signature material was present but did NOT verify (corrupted
    /// signature, wrong public key, or message tampering).
    Invalid,
    /// No signature material was present on the knot — nothing to do.
    Skipped,
}

impl VerificationOutcome {
    /// `true` only for [`Self::Valid`].
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid)
    }

    /// `true` only for [`Self::Invalid`].
    pub fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid)
    }
}

/// Result of one verification attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Identifier of the knot we attempted to validate.
    pub knot_id: String,

    /// `true` iff the cryptographic check passed.
    /// Convenience boolean — same information as `outcome.is_valid()`.
    pub sig_valid: bool,

    /// Tri-state outcome — see [`VerificationOutcome`].
    pub outcome: VerificationOutcome,

    /// Algorithm that ran (or `None` when skipped).
    pub sig_algo: SigAlgo,

    /// Wall-clock duration of the cryptographic operation, in
    /// microseconds. Useful for capacity planning — Dilithium3
    /// `open` is ~2-4ms on modern x86 single-core, ed25519 ~50µs.
    pub validation_time_us: u128,

    /// When we performed the verification (Unix seconds).
    pub validated_at: i64,

    /// On failure: a human-readable note (NEVER includes secret
    /// material). On success: `None`.
    pub note: Option<String>,
}

/// Stateless verifier. Holding a verifier is purely organizational —
/// the cryptographic primitives themselves are global.
#[derive(Debug, Default, Clone)]
pub struct KnotVerifier;

impl KnotVerifier {
    /// Construct a fresh verifier. Equivalent to `Self::default()`.
    pub fn new() -> Self {
        Self
    }

    /// Verify the signature on a knot. Never panics — all error paths
    /// produce a `VerificationResult` with `outcome = Invalid` (or
    /// `Skipped` for missing signature material). The unsigned-knot
    /// path returns `Skipped` and counts toward neither validated nor
    /// rejected metrics; this is deliberate and documented in the
    /// crate-level scope note.
    pub fn verify(&self, knot: &Knot) -> VerificationResult {
        let start = Instant::now();
        let validated_at = chrono::Utc::now().timestamp();

        // Decide which path to take based on what the knot presents.
        let (creator, signature) = match (&knot.creator, &knot.signature) {
            (Some(creator), Some(sig)) if !sig.is_empty() => (creator, sig),
            _ => {
                tracing::trace!(
                    target: "validation_agent::verify",
                    knot_id = %knot.knot_id,
                    source = knot.source.as_str(),
                    "knot has no signature material — skipping (not counted as rejected)",
                );
                return VerificationResult {
                    knot_id: knot.knot_id.clone(),
                    sig_valid: false,
                    outcome: VerificationOutcome::Skipped,
                    sig_algo: SigAlgo::None,
                    validation_time_us: start.elapsed().as_micros(),
                    validated_at,
                    note: Some("no signature material on knot".to_string()),
                };
            }
        };

        // Classify the algorithm BEFORE verification — failure mode
        // depends on which path we tried, and the agent metrics count
        // the attempted algo, not the abstract outcome.
        let sig_algo = if creator.has_pq_keys() {
            SigAlgo::Mldsa65Hybrid
        } else {
            SigAlgo::Ed25519Only
        };

        // Run the actual verification through `rope-crypto`. This call
        // enforces the strict semantics defined in `hybrid.rs`:
        //   - Ed25519 must always verify
        //   - When PQ keys are present, Dilithium3 MUST also verify
        //   - There is no fallback path that bypasses verification
        let crypto_outcome = HybridVerifier::verify(creator, &knot.signing_message, signature);

        match crypto_outcome {
            Ok(true) => {
                let elapsed_us = start.elapsed().as_micros();
                tracing::debug!(
                    target: "validation_agent::verify",
                    knot_id = %knot.knot_id,
                    source = knot.source.as_str(),
                    algo = sig_algo.as_str(),
                    elapsed_us = elapsed_us,
                    "knot signature verified",
                );
                VerificationResult {
                    knot_id: knot.knot_id.clone(),
                    sig_valid: true,
                    outcome: VerificationOutcome::Valid,
                    sig_algo,
                    validation_time_us: elapsed_us,
                    validated_at,
                    note: None,
                }
            }
            Ok(false) => {
                let elapsed_us = start.elapsed().as_micros();
                tracing::warn!(
                    target: "validation_agent::verify",
                    knot_id = %knot.knot_id,
                    source = knot.source.as_str(),
                    algo = sig_algo.as_str(),
                    "knot signature did NOT verify (Invalid)",
                );
                VerificationResult {
                    knot_id: knot.knot_id.clone(),
                    sig_valid: false,
                    outcome: VerificationOutcome::Invalid,
                    sig_algo,
                    validation_time_us: elapsed_us,
                    validated_at,
                    note: Some("cryptographic verification failed".to_string()),
                }
            }
            Err(e) => {
                let elapsed_us = start.elapsed().as_micros();
                let note = format!("rope_crypto verification error: {e}");
                tracing::warn!(
                    target: "validation_agent::verify",
                    knot_id = %knot.knot_id,
                    source = knot.source.as_str(),
                    algo = sig_algo.as_str(),
                    error = %e,
                    "rope_crypto rejected the signature payload structure",
                );
                VerificationResult {
                    knot_id: knot.knot_id.clone(),
                    sig_valid: false,
                    outcome: VerificationOutcome::Invalid,
                    sig_algo,
                    validation_time_us: elapsed_us,
                    validated_at,
                    note: Some(note),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knot::{Knot, KnotSource};
    use rope_crypto::hybrid::{HybridPublicKey, HybridSignature, HybridSigner};

    fn make_signed_knot(message: &[u8], hybrid: bool) -> (Knot, HybridSigner, HybridPublicKey) {
        let (signer, pk) = if hybrid {
            HybridSigner::generate()
        } else {
            // Build an Ed25519-only signer/verifier pair manually:
            // we still go through `HybridSigner::generate_signing_only`
            // and then strip the Dilithium component from the
            // *public* key so the verifier takes the ed25519-only
            // fallback. The signature object will still carry the
            // Dilithium signature bytes, which is fine because
            // `HybridVerifier::verify` only consults Dilithium when
            // the *public key* advertises it.
            let (s, full_pk) = HybridSigner::generate_signing_only();
            let ed_only_pk = HybridPublicKey::from_ed25519(full_pk.ed25519);
            (s, ed_only_pk)
        };
        let sig = signer.sign(message);
        let knot = Knot::new("0xtest", 0, KnotSource::Test, message.to_vec())
            .with_signature(pk.clone(), sig);
        (knot, signer, pk)
    }

    #[test]
    fn verifies_hybrid_dilithium3_signature() {
        let v = KnotVerifier::new();
        let (knot, _signer, _pk) = make_signed_knot(b"valid hybrid payload", true);
        let r = v.verify(&knot);
        assert!(r.sig_valid, "hybrid signature must verify");
        assert_eq!(r.outcome, VerificationOutcome::Valid);
        assert_eq!(r.sig_algo, SigAlgo::Mldsa65Hybrid);
        assert_eq!(r.knot_id, "0xtest");
        assert!(r.note.is_none());
    }

    #[test]
    fn verifies_ed25519_only_signature_via_fallback() {
        let v = KnotVerifier::new();
        let (knot, _signer, _pk) = make_signed_knot(b"valid ed25519 payload", false);
        let r = v.verify(&knot);
        assert!(
            r.sig_valid,
            "ed25519-only signature must verify in fallback"
        );
        assert_eq!(r.outcome, VerificationOutcome::Valid);
        assert_eq!(r.sig_algo, SigAlgo::Ed25519Only);
    }

    #[test]
    fn rejects_corrupted_dilithium_signature() {
        let v = KnotVerifier::new();
        let (mut knot, _signer, _pk) = make_signed_knot(b"will be tampered", true);
        // Flip a chunk in the middle of the Dilithium signature.
        if let Some(sig) = knot.signature.as_mut() {
            assert!(!sig.dilithium_sig.is_empty());
            let mid = sig.dilithium_sig.len() / 2;
            sig.dilithium_sig[mid] ^= 0xff;
            sig.dilithium_sig[mid + 1] ^= 0xa5;
        }
        let r = v.verify(&knot);
        assert!(!r.sig_valid, "tampered Dilithium signature must NOT verify");
        assert_eq!(r.outcome, VerificationOutcome::Invalid);
        assert_eq!(r.sig_algo, SigAlgo::Mldsa65Hybrid);
        assert!(r.note.is_some(), "Invalid result must carry a note");
    }

    #[test]
    fn rejects_wrong_public_key() {
        let v = KnotVerifier::new();
        let message = b"signed by signer A".to_vec();
        let (signer_a, _pk_a) = HybridSigner::generate();
        let (_signer_b, pk_b) = HybridSigner::generate();
        let sig = signer_a.sign(&message);
        let knot = Knot::new("0xwrongkey", 1, KnotSource::Test, message).with_signature(pk_b, sig);
        let r = v.verify(&knot);
        assert!(
            !r.sig_valid,
            "verifying signer A's signature against signer B's key must fail"
        );
        assert_eq!(r.outcome, VerificationOutcome::Invalid);
    }

    #[test]
    fn rejects_message_tampering() {
        let v = KnotVerifier::new();
        let (mut knot, _signer, _pk) = make_signed_knot(b"original message", true);
        knot.signing_message = b"tampered message".to_vec();
        let r = v.verify(&knot);
        assert!(!r.sig_valid);
        assert_eq!(r.outcome, VerificationOutcome::Invalid);
    }

    #[test]
    fn skips_unsigned_knot() {
        let v = KnotVerifier::new();
        let knot = Knot::new(
            "0xunsigned",
            42,
            KnotSource::CordAnchor,
            b"no sig here".to_vec(),
        );
        let r = v.verify(&knot);
        assert!(!r.sig_valid);
        assert_eq!(r.outcome, VerificationOutcome::Skipped);
        assert_eq!(r.sig_algo, SigAlgo::None);
        assert!(
            !r.outcome.is_invalid(),
            "skipped is NOT counted as rejected"
        );
    }

    #[test]
    fn empty_signature_treated_as_skipped() {
        // Knot carries a creator key but the signature is the
        // canonical "empty" placeholder — this is what
        // `personal_ledger.rs` writes today. Must be `Skipped`, NOT
        // `Invalid`, otherwise we'd log spurious WARN every cycle.
        let (_signer, pk) = HybridSigner::generate();
        let knot = Knot::new(
            "0xempty",
            7,
            KnotSource::CordAnchor,
            b"placeholder signing input".to_vec(),
        )
        .with_signature(pk, HybridSignature::empty());
        let r = KnotVerifier::new().verify(&knot);
        assert_eq!(r.outcome, VerificationOutcome::Skipped);
        assert_eq!(r.sig_algo, SigAlgo::None);
    }

    #[test]
    fn validation_time_is_recorded() {
        let v = KnotVerifier::new();
        let (knot, _, _) = make_signed_knot(b"timing test", true);
        let r = v.verify(&knot);
        assert!(r.sig_valid);
        // Hybrid Dilithium verification is real work — we expect at
        // least a few microseconds of wall clock to have elapsed.
        // We tolerate fast machines by only asserting non-zero.
        assert!(r.validation_time_us > 0, "validation_time_us must be > 0");
    }

    #[test]
    fn sig_algo_strings_are_canonical() {
        assert_eq!(SigAlgo::Mldsa65Hybrid.as_str(), "mldsa65+ed25519");
        assert_eq!(SigAlgo::Ed25519Only.as_str(), "ed25519");
        assert_eq!(SigAlgo::None.as_str(), "none");
    }
}
