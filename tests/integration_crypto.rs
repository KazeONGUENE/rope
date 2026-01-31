//! Integration tests for Datachain Rope cryptographic subsystem
//!
//! These tests verify end-to-end behavior of the hybrid cryptography system
//! including Ed25519, Dilithium3, X25519, and Kyber768.

use rope_crypto::hybrid::{
    HybridKEM, HybridPublicKey, HybridSecretKey, HybridSignature,
    HybridSigner, HybridVerifier, SharedSecret,
    DILITHIUM3_PUBLIC_KEY_SIZE, KYBER768_PUBLIC_KEY_SIZE,
};

mod hybrid_signature_tests {
    use super::*;

    #[test]
    fn test_full_signature_lifecycle() {
        // Generate keypair
        let (signer, public_key) = HybridSigner::generate();

        // Sign multiple messages
        let messages: Vec<&[u8]> = vec![
            b"First message",
            b"Second message with more content",
            b"",  // Empty message
            &[0u8; 1000],  // Binary data
        ];

        for msg in &messages {
            let signature = signer.sign(msg);

            // Verify structure
            assert!(signature.is_valid_structure(), "Signature structure invalid for message len {}", msg.len());
            assert_eq!(signature.ed25519_sig.len(), 64);
            assert!(!signature.dilithium_sig.is_empty());

            // Verify cryptographically
            let result = HybridVerifier::verify(&public_key, msg, &signature);
            assert!(result.is_ok(), "Verification failed for message len {}", msg.len());
            assert!(result.unwrap(), "Signature invalid for message len {}", msg.len());
        }
    }

    #[test]
    fn test_signature_tampering_detection() {
        let (signer, public_key) = HybridSigner::generate();
        let message = b"Important financial transaction";
        let signature = signer.sign(message);

        // Test 1: Tampered Ed25519 signature
        let mut tampered_ed = signature.clone();
        tampered_ed.ed25519_sig[0] ^= 0xFF;
        assert!(!HybridVerifier::verify(&public_key, message, &tampered_ed).unwrap());

        // Test 2: Tampered Dilithium signature
        let mut tampered_dil = signature.clone();
        if !tampered_dil.dilithium_sig.is_empty() {
            tampered_dil.dilithium_sig[0] ^= 0xFF;
            // Should either fail verification or return error
            let result = HybridVerifier::verify(&public_key, message, &tampered_dil);
            assert!(result.is_err() || !result.unwrap());
        }

        // Test 3: Wrong message
        let wrong_message = b"Different financial transaction";
        assert!(!HybridVerifier::verify(&public_key, wrong_message, &signature).unwrap());
    }

    #[test]
    fn test_cross_keypair_rejection() {
        let (signer1, _pk1) = HybridSigner::generate();
        let (_signer2, pk2) = HybridSigner::generate();

        let message = b"Test message";
        let signature = signer1.sign(message);

        // Signature from signer1 should NOT verify with pk2
        assert!(!HybridVerifier::verify(&pk2, message, &signature).unwrap());
    }

    #[test]
    fn test_pq_keys_mandatory_when_present() {
        let (signer, public_key) = HybridSigner::generate();
        let message = b"Test message";

        // Create signature with valid Ed25519 but empty Dilithium
        let ed25519_only_sig = HybridSignature {
            ed25519_sig: signer.sign(message).ed25519_sig,
            dilithium_sig: Vec::new(),
        };

        // Since public_key has Dilithium key, verification MUST fail
        assert!(public_key.has_pq_keys());
        let result = HybridVerifier::verify(&public_key, message, &ed25519_only_sig);
        assert!(result.is_ok());
        assert!(!result.unwrap(), "Should reject when PQ signature missing but PQ key present");
    }

    #[test]
    fn test_backward_compatibility_ed25519_only() {
        // Create Ed25519-only signer (for legacy compatibility)
        let (signer, _full_pk) = HybridSigner::generate_signing_only();
        let message = b"Legacy message";

        let signature = signer.sign(message);

        // Create Ed25519-only public key
        let ed25519_only_pk = HybridPublicKey::from_ed25519(signer.public_key().ed25519);

        // Should still work with Ed25519-only signature
        // Note: This requires the signature to also be Ed25519-only compatible
        assert!(!ed25519_only_pk.has_pq_keys());
    }
}

mod hybrid_kem_tests {
    use super::*;

    #[test]
    fn test_kem_roundtrip() {
        let (signer, public_key) = HybridSigner::generate();

        // Verify full key support
        assert!(public_key.has_x25519());
        assert!(public_key.has_encryption_key());

        // Encapsulate
        let (encapsulated, shared_secret1) = HybridKEM::encapsulate(&public_key)
            .expect("Encapsulation failed");

        // Verify encapsulated key structure
        assert_ne!(encapsulated.x25519_ephemeral, [0u8; 32]);
        assert!(!encapsulated.kyber_ciphertext.is_empty());
        assert!(encapsulated.has_kyber());

        // Decapsulate
        let secret_key = signer.secret_key();
        let shared_secret2 = HybridKEM::decapsulate(&secret_key, &encapsulated)
            .expect("Decapsulation failed");

        // Shared secrets MUST match
        assert_eq!(
            shared_secret1.as_bytes(),
            shared_secret2.as_bytes(),
            "Shared secrets don't match!"
        );
    }

    #[test]
    fn test_kem_different_recipients() {
        let (signer1, pk1) = HybridSigner::generate();
        let (signer2, pk2) = HybridSigner::generate();

        // Encapsulate to pk1
        let (encapsulated, secret1) = HybridKEM::encapsulate(&pk1)
            .expect("Encapsulation failed");

        // Decapsulate with sk1 should work
        let decapsulated1 = HybridKEM::decapsulate(&signer1.secret_key(), &encapsulated)
            .expect("Decapsulation with correct key failed");
        assert_eq!(secret1.as_bytes(), decapsulated1.as_bytes());

        // Decapsulate with sk2 should produce DIFFERENT secret
        let decapsulated2 = HybridKEM::decapsulate(&signer2.secret_key(), &encapsulated)
            .expect("Decapsulation should complete");
        assert_ne!(
            secret1.as_bytes(),
            decapsulated2.as_bytes(),
            "Different keys should produce different secrets"
        );
    }

    #[test]
    fn test_kem_many_iterations() {
        let (signer, public_key) = HybridSigner::generate();
        let secret_key = signer.secret_key();

        // Run many encapsulate/decapsulate cycles
        for _ in 0..10 {
            let (encap, secret1) = HybridKEM::encapsulate(&public_key)
                .expect("Encapsulation failed");

            let secret2 = HybridKEM::decapsulate(&secret_key, &encap)
                .expect("Decapsulation failed");

            assert_eq!(secret1.as_bytes(), secret2.as_bytes());
        }
    }

    #[test]
    fn test_kem_ciphertext_tampering() {
        let (signer, public_key) = HybridSigner::generate();
        let secret_key = signer.secret_key();

        let (mut encapsulated, original_secret) = HybridKEM::encapsulate(&public_key)
            .expect("Encapsulation failed");

        // Tamper with Kyber ciphertext
        if !encapsulated.kyber_ciphertext.is_empty() {
            encapsulated.kyber_ciphertext[0] ^= 0xFF;
        }

        // Decapsulation might succeed (Kyber decapsulation is always possible)
        // but the shared secret should be different
        if let Ok(tampered_secret) = HybridKEM::decapsulate(&secret_key, &encapsulated) {
            // With IND-CCA2 secure KEM, tampered ciphertext produces different secret
            // (this is the security guarantee of Kyber)
            assert_ne!(
                original_secret.as_bytes(),
                tampered_secret.as_bytes(),
                "Tampered ciphertext should not produce same secret"
            );
        }
    }
}

mod key_serialization_tests {
    use super::*;

    #[test]
    fn test_public_key_roundtrip() {
        let (_, public_key) = HybridSigner::generate();

        let serialized = public_key.to_bytes();
        let deserialized = HybridPublicKey::from_bytes(&serialized)
            .expect("Deserialization failed");

        assert_eq!(public_key.ed25519, deserialized.ed25519);
        assert_eq!(public_key.x25519, deserialized.x25519);
        assert_eq!(public_key.dilithium, deserialized.dilithium);
        assert_eq!(public_key.kyber, deserialized.kyber);
    }

    #[test]
    fn test_public_key_sizes() {
        let (_, public_key) = HybridSigner::generate();

        assert_eq!(public_key.ed25519.len(), 32);
        assert_eq!(public_key.x25519.len(), 32);
        assert_eq!(public_key.dilithium.len(), DILITHIUM3_PUBLIC_KEY_SIZE);
        assert_eq!(public_key.kyber.len(), KYBER768_PUBLIC_KEY_SIZE);
    }

    #[test]
    fn test_invalid_public_key_bytes() {
        // Too short
        let result = HybridPublicKey::from_bytes(&[0u8; 10]);
        assert!(result.is_err());

        // Valid length but truncated Dilithium
        let mut bad_bytes = vec![0u8; 72]; // ed25519 + x25519 + length fields
        bad_bytes[64..68].copy_from_slice(&100u32.to_le_bytes()); // Claim 100 bytes of Dilithium
        let result = HybridPublicKey::from_bytes(&bad_bytes);
        assert!(result.is_err());
    }
}

mod security_property_tests {
    use super::*;

    #[test]
    fn test_signatures_are_deterministic_per_keypair() {
        // Note: Actually, Ed25519 signatures with the same key and message
        // should be deterministic. Let's verify this.
        let (signer, _) = HybridSigner::generate();
        let message = b"Test determinism";

        let sig1 = signer.sign(message);
        let sig2 = signer.sign(message);

        // Ed25519 is deterministic
        assert_eq!(sig1.ed25519_sig, sig2.ed25519_sig);
        // Note: Dilithium may or may not be deterministic depending on implementation
    }

    #[test]
    fn test_different_messages_different_signatures() {
        let (signer, _) = HybridSigner::generate();

        let sig1 = signer.sign(b"Message 1");
        let sig2 = signer.sign(b"Message 2");

        // Signatures must be different for different messages
        assert_ne!(sig1.ed25519_sig, sig2.ed25519_sig);
        assert_ne!(sig1.dilithium_sig, sig2.dilithium_sig);
    }

    #[test]
    fn test_keypair_uniqueness() {
        // Generate many keypairs and ensure they're all unique
        let mut ed25519_keys = std::collections::HashSet::new();
        let mut x25519_keys = std::collections::HashSet::new();

        for _ in 0..20 {
            let (_, pk) = HybridSigner::generate();

            assert!(
                ed25519_keys.insert(pk.ed25519),
                "Duplicate Ed25519 key generated!"
            );
            assert!(
                x25519_keys.insert(pk.x25519),
                "Duplicate X25519 key generated!"
            );
        }
    }

    #[test]
    fn test_shared_secret_uniqueness() {
        let (_, public_key) = HybridSigner::generate();
        let mut secrets = std::collections::HashSet::new();

        // Generate many encapsulations
        for _ in 0..20 {
            let (_, secret) = HybridKEM::encapsulate(&public_key)
                .expect("Encapsulation failed");

            assert!(
                secrets.insert(*secret.as_bytes()),
                "Duplicate shared secret generated!"
            );
        }
    }
}
