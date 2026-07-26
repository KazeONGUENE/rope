//! End-to-end test of the shadow chain in isolation from upstream RPC.
//!
//! Drives `ShadowChain::apply_observed` directly to simulate a canonical
//! chain producing a sequence of knots and one tombstone, then walks
//! the v2 chain via the store and verifies §6.1.1 properties:
//!
//!   1. The chain is contiguous over event_id.
//!   2. Each `previous_hash` matches the predecessor's `knot_hash`.
//!   3. After a tombstone is observed at event_id N, the chain head
//!      remains at the most recent active event_id (N+k), not at N.
//!      This is the chain-continuity-under-erasure property.
//!   4. Two independent shadow chains observing the same canonical
//!      sequence produce identical v2 chains.

use std::sync::Arc;

use rope_core::knot_hash::KnotHash;

use rope_shadow_witness::chain::{ObservedKnot, ShadowChain};
use rope_shadow_witness::store::{parse_string_id_hex, ShadowChainStore};

fn fresh_chain() -> (tempfile::TempDir, ShadowChain) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ShadowChainStore::open(dir.path()).unwrap());
    (dir, ShadowChain::new(store))
}

fn s_id() -> String {
    "0x".to_string() + &"77".repeat(32)
}

fn obs_active(i: u64) -> ObservedKnot {
    ObservedKnot {
        string_id: s_id(),
        knot_index: i,
        is_tombstone: false,
        tombstone_untied_at: None,
        tombstone_audit_hash_hex: None,
        tombstone_reason: None,
    }
}

fn obs_tomb(i: u64) -> ObservedKnot {
    ObservedKnot {
        string_id: s_id(),
        knot_index: i,
        is_tombstone: true,
        tombstone_untied_at: Some(1700000999),
        tombstone_audit_hash_hex: Some("0x".to_string() + &"de".repeat(32)),
        tombstone_reason: Some("OwnerRequest".to_string()),
    }
}

#[test]
fn end_to_end_chain_invariants() {
    let (_d, chain) = fresh_chain();

    for i in 0..7 {
        chain.apply_observed(&obs_active(i)).unwrap();
    }

    let id_b = parse_string_id_hex(&s_id()).unwrap();
    let walk = chain.store().walk_chain(&id_b, 0, 100).unwrap();
    assert_eq!(walk.len(), 7);

    assert_eq!(walk[0].previous_hash, KnotHash::GENESIS);
    for w in walk.windows(2) {
        let (prev, next) = (&w[0], &w[1]);
        assert_eq!(next.event_id, prev.event_id + 1);
        assert_eq!(next.previous_hash, prev.knot_hash);
    }

    let head_before = chain.store().get_head(&id_b).unwrap().unwrap();
    assert_eq!(head_before.latest_event_id, 6);

    chain.apply_observed(&obs_tomb(3)).unwrap();

    let head_after = chain.store().get_head(&id_b).unwrap().unwrap();
    assert_eq!(head_after.latest_event_id, 6);
    assert_eq!(head_after.latest_knot_hash, head_before.latest_knot_hash);

    let e3 = chain.store().get_entry(&id_b, 3).unwrap().unwrap();
    assert!(e3.is_tombstone);
    assert_eq!(e3.event_type, "erasure");

    let walk_after = chain.store().walk_chain(&id_b, 0, 100).unwrap();
    assert_eq!(walk_after.len(), 7);
    let active_count = walk_after.iter().filter(|e| !e.is_tombstone).count();
    let tombstone_count = walk_after.iter().filter(|e| e.is_tombstone).count();
    assert_eq!(active_count, 6);
    assert_eq!(tombstone_count, 1);
}

#[test]
fn two_independent_chains_agree() {
    let (_d1, c1) = fresh_chain();
    let (_d2, c2) = fresh_chain();

    let mut sequence: Vec<ObservedKnot> = (0..10).map(obs_active).collect();
    sequence.push(obs_tomb(2));
    sequence.push(obs_tomb(7));

    for k in &sequence {
        c1.apply_observed(k).unwrap();
        c2.apply_observed(k).unwrap();
    }

    let id_b = parse_string_id_hex(&s_id()).unwrap();
    let w1 = c1.store().walk_chain(&id_b, 0, 100).unwrap();
    let w2 = c2.store().walk_chain(&id_b, 0, 100).unwrap();

    assert_eq!(w1.len(), w2.len());
    for (e1, e2) in w1.iter().zip(w2.iter()) {
        assert_eq!(e1.knot_hash, e2.knot_hash);
        assert_eq!(e1.event_metadata_hash, e2.event_metadata_hash);
        assert_eq!(e1.previous_hash, e2.previous_hash);
        assert_eq!(e1.is_tombstone, e2.is_tombstone);
    }
}

#[test]
fn store_persistence_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    {
        let store = Arc::new(ShadowChainStore::open(path).unwrap());
        let chain = ShadowChain::new(store);
        for i in 0..5 {
            chain.apply_observed(&obs_active(i)).unwrap();
        }
    }

    let store2 = Arc::new(ShadowChainStore::open(path).unwrap());
    let chain2 = ShadowChain::new(store2);
    let id_b = parse_string_id_hex(&s_id()).unwrap();
    let walk = chain2.store().walk_chain(&id_b, 0, 100).unwrap();
    assert_eq!(walk.len(), 5);
    let head = chain2.store().get_head(&id_b).unwrap().unwrap();
    assert_eq!(head.latest_event_id, 4);
}
