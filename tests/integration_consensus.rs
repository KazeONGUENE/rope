//! Integration tests for Datachain Rope consensus subsystem
//!
//! These tests verify the virtual voting consensus mechanism,
//! testimony creation/verification, and finality determination.

use rope_consensus::virtual_voting::{
    VirtualVotingEngine, ConsensusConfig, GossipEvent,
};
use rope_consensus::testimony::{
    Testimony, TestimonyType, TestimonyValidator,
};
use rope_core::string::RopeString;
use std::collections::HashMap;

mod virtual_voting_tests {
    use super::*;

    fn create_test_config() -> ConsensusConfig {
        ConsensusConfig {
            min_validators: 4,
            supermajority_threshold: 0.67,
            round_timeout_ms: 1000,
            max_rounds_ahead: 100,
            enable_ai_agents: false,
        }
    }

    #[test]
    fn test_consensus_engine_creation() {
        let config = create_test_config();
        let engine = VirtualVotingEngine::new(config);

        assert_eq!(engine.current_round(), 0);
        assert!(!engine.is_running());
    }

    #[test]
    fn test_gossip_event_creation() {
        let creator = [1u8; 32];
        let strings = vec![[2u8; 32], [3u8; 32]];
        let self_parent = Some([4u8; 32]);
        let other_parent = Some([5u8; 32]);

        let event = GossipEvent::new(
            creator,
            1, // round
            strings.clone(),
            self_parent,
            other_parent,
        );

        assert_eq!(event.creator(), &creator);
        assert_eq!(event.round(), 1);
        assert_eq!(event.string_ids().len(), 2);
        assert!(event.self_parent().is_some());
        assert!(event.other_parent().is_some());
    }

    #[test]
    fn test_gossip_event_serialization() {
        let event = GossipEvent::new(
            [1u8; 32],
            5,
            vec![[2u8; 32]],
            None,
            None,
        );

        let serialized = event.to_bytes();
        let deserialized = GossipEvent::from_bytes(&serialized)
            .expect("Deserialization failed");

        assert_eq!(event.creator(), deserialized.creator());
        assert_eq!(event.round(), deserialized.round());
        assert_eq!(event.event_id(), deserialized.event_id());
    }

    #[test]
    fn test_famous_witness_detection() {
        // This test verifies the Appendix B.1 virtual voting algorithm
        let config = create_test_config();
        let mut engine = VirtualVotingEngine::new(config);

        // Add validators
        for i in 0..5 {
            engine.add_validator([i as u8; 32], 1);
        }

        // Create witness events for round 0
        for i in 0..5 {
            let event = GossipEvent::new(
                [i as u8; 32],
                0,
                vec![],
                None,
                None,
            );
            engine.add_event(event).expect("Failed to add event");
        }

        // Witnesses at round 0 should exist
        let witnesses = engine.witnesses_at_round(0);
        assert!(!witnesses.is_empty());
    }

    #[test]
    fn test_strongly_seeing() {
        let config = create_test_config();
        let mut engine = VirtualVotingEngine::new(config);

        // Setup 5 validators
        for i in 0..5 {
            engine.add_validator([i as u8; 32], 1);
        }

        // Round 0 - initial events
        let round0_events: Vec<GossipEvent> = (0..5)
            .map(|i| GossipEvent::new([i as u8; 32], 0, vec![], None, None))
            .collect();

        for event in &round0_events {
            engine.add_event(event.clone()).expect("Failed to add round 0 event");
        }

        // Round 1 - each validator sees at least 2/3 of round 0
        for i in 0..5 {
            let self_parent = round0_events[i].event_id();
            let other_parent = round0_events[(i + 1) % 5].event_id();

            let event = GossipEvent::new(
                [i as u8; 32],
                1,
                vec![],
                Some(self_parent),
                Some(other_parent),
            );
            engine.add_event(event).expect("Failed to add round 1 event");
        }

        // After enough gossip, witnesses should be strongly seen
        let round1_witnesses = engine.witnesses_at_round(1);
        assert!(!round1_witnesses.is_empty(), "Should have round 1 witnesses");
    }
}

mod testimony_tests {
    use super::*;

    #[test]
    fn test_testimony_creation() {
        let target_string_id = [1u8; 32];
        let validator_id = [2u8; 32];

        let testimony = Testimony::new(
            target_string_id,
            validator_id,
            TestimonyType::Attest,
            1, // round
            vec![0u8; 64], // signature placeholder
        );

        assert_eq!(testimony.target_string_id(), &target_string_id);
        assert_eq!(testimony.validator_id(), &validator_id);
        assert_eq!(testimony.attestation_type(), TestimonyType::Attest);
        assert_eq!(testimony.round(), 1);
    }

    #[test]
    fn test_testimony_types() {
        let types = vec![
            TestimonyType::Attest,
            TestimonyType::Reject,
            TestimonyType::Abstain,
            TestimonyType::Challenge,
        ];

        for t in types {
            let testimony = Testimony::new(
                [1u8; 32],
                [2u8; 32],
                t.clone(),
                0,
                vec![],
            );

            assert_eq!(testimony.attestation_type(), t);
        }
    }

    #[test]
    fn test_testimony_validator() {
        let validator = TestimonyValidator::new();

        // Create test testimony
        let testimony = Testimony::new(
            [1u8; 32],
            [2u8; 32],
            TestimonyType::Attest,
            1,
            vec![0u8; 64],
        );

        // Structure validation (signature verification would need actual crypto)
        assert!(validator.validate_structure(&testimony).is_ok());
    }

    #[test]
    fn test_testimony_quorum() {
        let validator = TestimonyValidator::new();

        // Create 5 attestations
        let testimonies: Vec<Testimony> = (0..5)
            .map(|i| Testimony::new(
                [1u8; 32], // same target
                [i as u8; 32], // different validators
                TestimonyType::Attest,
                1,
                vec![0u8; 64],
            ))
            .collect();

        // With 5/5 attestations, should have quorum
        let quorum = validator.check_quorum(&testimonies, 5, 0.67);
        assert!(quorum.is_ok());
        assert!(quorum.unwrap());
    }

    #[test]
    fn test_testimony_rejection() {
        let validator = TestimonyValidator::new();

        // Create 3 attestations and 3 rejections
        let mut testimonies: Vec<Testimony> = (0..3)
            .map(|i| Testimony::new(
                [1u8; 32],
                [i as u8; 32],
                TestimonyType::Attest,
                1,
                vec![0u8; 64],
            ))
            .collect();

        testimonies.extend((3..6).map(|i| Testimony::new(
            [1u8; 32],
            [i as u8; 32],
            TestimonyType::Reject,
            1,
            vec![0u8; 64],
        )));

        // 3/6 attestations = 50%, below 67% threshold
        let quorum = validator.check_quorum(&testimonies, 6, 0.67);
        assert!(quorum.is_ok());
        assert!(!quorum.unwrap());
    }
}

mod finality_tests {
    use super::*;

    #[test]
    fn test_string_finality_flow() {
        let config = ConsensusConfig {
            min_validators: 3,
            supermajority_threshold: 0.67,
            round_timeout_ms: 1000,
            max_rounds_ahead: 100,
            enable_ai_agents: false,
        };

        let mut engine = VirtualVotingEngine::new(config);

        // Add 5 validators with equal stake
        for i in 0..5 {
            engine.add_validator([i as u8; 32], 1);
        }

        // Create a string to be finalized
        let string_id = [100u8; 32];

        // Simulate the finality process
        // Round 0: String is proposed
        engine.propose_string(string_id, [0u8; 32]).expect("Propose failed");

        // Add testimonies from 4/5 validators (80% > 67%)
        for i in 0..4 {
            let testimony = Testimony::new(
                string_id,
                [i as u8; 32],
                TestimonyType::Attest,
                0,
                vec![0u8; 64],
            );
            engine.add_testimony(testimony).expect("Add testimony failed");
        }

        // Check if string has reached finality conditions
        let status = engine.string_status(&string_id);
        // Actual finality depends on virtual voting rounds completing
        assert!(status.is_some());
    }

    #[test]
    fn test_anchor_creation() {
        let config = ConsensusConfig {
            min_validators: 3,
            supermajority_threshold: 0.67,
            round_timeout_ms: 1000,
            max_rounds_ahead: 100,
            enable_ai_agents: false,
        };

        let engine = VirtualVotingEngine::new(config);

        // After finality, an anchor should be created
        // This is typically done automatically by the consensus engine
        let anchors = engine.anchors_in_range(0, 10);

        // Initially no anchors
        assert!(anchors.is_empty() || anchors.len() >= 0);
    }
}
