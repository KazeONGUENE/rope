//! Lamport Clock implementation for causal ordering
//!
//! τ (Tau) - Temporal Marker using Lamport clock extended with causal ordering.
//!
//! Unlike synchronized wall clocks, Lamport clocks provide a logical ordering
//! that respects causality: if event A caused event B, then clock(A) < clock(B).
//!
//! ## Quipu Canon v2.0 Phase 1.3 — Per-Shard Hybrid Logical Clock
//!
//! In v1.x the [`ClockManager`] held a single `parking_lot::Mutex<LamportClock>`
//! that every knot append funnelled through, capping write throughput at the
//! contention rate of one mutex (~1.5K–4K knot/sec sustained — see
//! `docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §3.3).
//!
//! Phase 1.3 replaces that with **256 per-shard Hybrid Logical Clocks**
//! (Kulkarni et al. 2014). Each shard has its own mutex-guarded HLC state.
//! `tick_for_wallet` selects the shard deterministically from a hash of the
//! wallet address, so two writes to two different wallets touch two different
//! mutexes and proceed in parallel.
//!
//! ### Wire-format compatibility
//!
//! [`LamportClock`] is the persisted/serialised type and is **unchanged**.
//! The HLC produces a [`LamportClock`] whose `logical_time: u64` is a packed
//! `(physical_ms_high48, logical_low16)` value. Lexicographic ordering on
//! the packed `u64` matches the natural HLC ordering, so:
//!
//! - Existing `LamportClock` callers (`personal_ledger`, `lattice`,
//!   `complement`, `string`, `testimony`, `agent_runner`, `string_producer`,
//!   `consensus_orchestrator`) keep working without changes.
//! - Stored RopeStrings remain comparable across the v1.x → v2.0 boundary:
//!   v1.x sequential values start near 0, v2.0 packed values start near
//!   `1.7×10¹² << 16` ≈ `10¹⁷`, so a v2.0 timestamp is always strictly
//!   greater than any v1.x timestamp — i.e. v2.0 events are always causally
//!   after v1.x events, which is exactly what we want.
//!
//! ### What is NOT in Phase 1.3
//!
//! - Sharded `StringLattice` — Phase 1.1
//! - Per-wallet head-string lock — Phase 1.2
//! - The other local clocks in `agent_runner.rs`, `string_producer.rs`, and
//!   `consensus_orchestrator.rs` are subsystem-local, not on the per-knot
//!   hot path; they remain `Mutex<LamportClock>` for now.

use crate::types::NodeId;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

/// Number of per-shard HLC instances. Power of two so wallet → shard maps to
/// `hash(wallet)[0] & (NUM_SHARDS - 1)`. Sized for ~10K active wallets per
/// node (~40 wallets per shard average) and matches the 256-way sharding
/// target used elsewhere in Phase 1 (`StringLattice`, `LedgerStore`).
pub const NUM_SHARDS: usize = 256;

/// Width of the logical-counter portion of the packed HLC timestamp (low
/// bits of [`LamportClock::time`]). 16 bits gives 65,536 ticks per millisecond
/// per shard before wrap; current Phase 1 ceiling is ~400 ticks/sec/shard
/// (~0.4/ms), Phase 4's 5M aggregate is ~80 ticks/ms/shard — orders of
/// magnitude under the ceiling.
const LOGICAL_BITS: u32 = 16;
const LOGICAL_MASK: u64 = (1u64 << LOGICAL_BITS) - 1;
const PHYSICAL_MAX: u64 = (1u64 << (64 - LOGICAL_BITS)) - 1;

/// Pack `(physical_ms, logical)` into the `u64` carried by
/// [`LamportClock::logical_time`]. High 48 bits = physical ms, low 16 = logical.
#[inline]
fn pack_hlc(physical_ms: u64, logical: u64) -> u64 {
    (physical_ms.min(PHYSICAL_MAX) << LOGICAL_BITS) | (logical & LOGICAL_MASK)
}

/// Inverse of [`pack_hlc`].
#[inline]
fn unpack_hlc(packed: u64) -> (u64, u64) {
    (packed >> LOGICAL_BITS, packed & LOGICAL_MASK)
}

/// Wall-clock now in ms since UNIX epoch. Saturates at `PHYSICAL_MAX` to
/// avoid panics; under any sane clock that never trips before year 10889 AD.
#[inline]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        .min(PHYSICAL_MAX)
}

/// Map a wallet address (or any byte string) to one of [`NUM_SHARDS`].
///
/// Uses the first byte of `blake3(wallet)` so that distributions stay
/// uniform even if the input is structured (e.g. a contract address with
/// a non-random prefix). BLAKE3 of a 20-byte wallet is ~150 ns — negligible
/// vs the ~10 µs hot-path cost of an append.
#[inline]
pub fn shard_for_wallet(wallet: &[u8]) -> u8 {
    blake3::hash(wallet).as_bytes()[0]
}

/// Extended Lamport Clock with causal parent tracking
///
/// This implementation extends the basic Lamport clock with:
/// - Node identification for tie-breaking
/// - Causal parent references for DAG construction
///
/// **Wire format is stable across v1.x and v2.0.** In v2.0 the
/// `logical_time` field is filled with a packed HLC timestamp (see
/// module-level docs), but the type, layout, ordering, and serialisation
/// are unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LamportClock {
    /// Logical time counter
    logical_time: u64,

    /// Node that created this timestamp
    node_id: NodeId,

    /// Causal parents: (NodeId, logical_time) pairs
    causal_parents: Vec<(NodeId, u64)>,
}

impl LamportClock {
    /// Create a new clock for a node, starting at 0
    pub fn new(node_id: NodeId) -> Self {
        Self {
            logical_time: 0,
            node_id,
            causal_parents: Vec::new(),
        }
    }

    /// Create clock with specific time (for deserialization)
    pub fn with_time(logical_time: u64, node_id: NodeId) -> Self {
        Self {
            logical_time,
            node_id,
            causal_parents: Vec::new(),
        }
    }

    /// Get current logical time
    pub fn time(&self) -> u64 {
        self.logical_time
    }

    /// Get the node id
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get causal parents
    pub fn causal_parents(&self) -> &[(NodeId, u64)] {
        &self.causal_parents
    }

    /// Increment clock for local event
    pub fn increment(&mut self) -> u64 {
        self.logical_time += 1;
        self.causal_parents.clear();
        self.logical_time
    }

    /// Update clock upon receiving a message (observe remote clock)
    pub fn observe(&mut self, other: &LamportClock) {
        self.logical_time = self.logical_time.max(other.logical_time) + 1;
        self.causal_parents
            .push((other.node_id, other.logical_time));
    }

    /// Observe multiple clocks and update
    pub fn observe_many<'a>(&mut self, others: impl Iterator<Item = &'a LamportClock>) {
        let mut max_time = self.logical_time;

        for other in others {
            max_time = max_time.max(other.logical_time);
            self.causal_parents
                .push((other.node_id, other.logical_time));
        }

        self.logical_time = max_time + 1;
    }

    /// Create a snapshot of current state
    pub fn snapshot(&self) -> LamportClock {
        self.clone()
    }

    /// Check if this clock happened-before another
    ///
    /// Returns true if this clock definitely precedes `other` causally
    pub fn happened_before(&self, other: &LamportClock) -> bool {
        if self.logical_time >= other.logical_time {
            return false;
        }

        // Check if we're in the causal parents
        other
            .causal_parents
            .iter()
            .any(|(node, time)| *node == self.node_id && *time >= self.logical_time)
    }

    /// Check if events are concurrent (neither happened before the other)
    pub fn is_concurrent(&self, other: &LamportClock) -> bool {
        !self.happened_before(other) && !other.happened_before(self)
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.logical_time.to_be_bytes());
        bytes.extend_from_slice(self.node_id.as_bytes());
        bytes.extend_from_slice(&(self.causal_parents.len() as u32).to_be_bytes());

        for (node, time) in &self.causal_parents {
            bytes.extend_from_slice(node.as_bytes());
            bytes.extend_from_slice(&time.to_be_bytes());
        }

        bytes
    }
}

impl PartialOrd for LamportClock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LamportClock {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary: logical time
        match self.logical_time.cmp(&other.logical_time) {
            Ordering::Equal => {
                // Tie-breaker: node id (for total ordering)
                self.node_id.as_bytes().cmp(other.node_id.as_bytes())
            }
            other => other,
        }
    }
}

impl Default for LamportClock {
    fn default() -> Self {
        Self {
            logical_time: 0,
            node_id: NodeId::new([0u8; 32]),
            causal_parents: Vec::new(),
        }
    }
}

// ============================================================================
// Hybrid Logical Clock — per-shard internal state machine
// ============================================================================

/// Per-shard Hybrid Logical Clock state. Mutex-guarded; one instance per
/// shard inside [`ClockManager`].
///
/// HLC update rules:
/// - **tick**: `pt = max(now_ms, last.pt); l = (pt == last.pt) ? last.l + 1 : 0`
/// - **observe(other)**: `pt = max(now_ms, last.pt, other.pt); l =`
///   `tie-break per Kulkarni 2014`
///
/// Logical-counter overflow (>65,535 ticks in one ms on one shard) is handled
/// by bumping `physical_ms` by 1 and resetting the counter to 0; this preserves
/// monotonicity at the cost of a microsecond of wall-clock skew.
struct HlcShard {
    state: parking_lot::Mutex<HlcState>,
    node_id: NodeId,
}

#[derive(Clone, Copy)]
struct HlcState {
    physical_ms: u64,
    logical: u64,
}

impl HlcShard {
    fn new(node_id: NodeId) -> Self {
        Self {
            state: parking_lot::Mutex::new(HlcState {
                physical_ms: 0,
                logical: 0,
            }),
            node_id,
        }
    }

    fn tick(&self) -> LamportClock {
        let wall = now_ms();
        let mut s = self.state.lock();

        if wall > s.physical_ms {
            s.physical_ms = wall;
            s.logical = 0;
        } else if s.logical >= LOGICAL_MASK {
            // Pathological burst overflows the 16-bit logical counter.
            // Bump physical by 1 ms, reset counter — preserves monotonicity.
            s.physical_ms = s.physical_ms.saturating_add(1).min(PHYSICAL_MAX);
            s.logical = 0;
        } else {
            s.logical += 1;
        }

        LamportClock::with_time(pack_hlc(s.physical_ms, s.logical), self.node_id)
    }

    fn now(&self) -> LamportClock {
        let s = self.state.lock();
        LamportClock::with_time(pack_hlc(s.physical_ms, s.logical), self.node_id)
    }

    fn observe(&self, other: &LamportClock) -> LamportClock {
        let wall = now_ms();
        let (other_ms, other_l) = unpack_hlc(other.time());
        let mut s = self.state.lock();

        let new_ms = wall.max(s.physical_ms).max(other_ms);

        let new_l = if new_ms == s.physical_ms && new_ms == other_ms {
            // All three agree: bump max(local, remote) by 1
            s.logical.max(other_l).saturating_add(1)
        } else if new_ms == s.physical_ms {
            s.logical.saturating_add(1)
        } else if new_ms == other_ms {
            other_l.saturating_add(1)
        } else {
            // Wall clock advanced past both endpoints
            0
        };

        if new_l > LOGICAL_MASK {
            // Pathological burst overflows the 16-bit logical counter.
            // Bump physical by 1 ms, reset counter — preserves monotonicity.
            s.physical_ms = new_ms.saturating_add(1).min(PHYSICAL_MAX);
            s.logical = 0;
        } else {
            s.physical_ms = new_ms;
            s.logical = new_l;
        }

        LamportClock::with_time(pack_hlc(s.physical_ms, s.logical), self.node_id)
    }
}

// ============================================================================
// ClockManager — public API, sharded over 256 HlcShards
// ============================================================================

/// Clock manager for a node. Backed by [`NUM_SHARDS`] independent Hybrid
/// Logical Clocks; concurrent ticks from different wallets touch different
/// mutexes and proceed in parallel.
pub struct ClockManager {
    shards: Box<[HlcShard]>,
    node_id: NodeId,
}

impl ClockManager {
    /// Create a new clock manager for a node, with [`NUM_SHARDS`] HLCs.
    pub fn new(node_id: NodeId) -> Self {
        let shards: Vec<HlcShard> = (0..NUM_SHARDS).map(|_| HlcShard::new(node_id)).collect();
        Self {
            shards: shards.into_boxed_slice(),
            node_id,
        }
    }

    /// Number of per-shard HLCs backing this manager (always [`NUM_SHARDS`]).
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// Get current time without incrementing. Returns the maximum HLC
    /// snapshot across all shards — i.e. the most recent timestamp the
    /// node has issued to any wallet.
    pub fn now(&self) -> LamportClock {
        self.shards
            .iter()
            .map(|s| s.now())
            .max()
            .unwrap_or_else(|| LamportClock::new(self.node_id))
    }

    /// Increment and get a new timestamp, **without** wallet context.
    ///
    /// Backward-compatible with the v1.x API. Pinned to shard 0 so the
    /// monotonicity contract (`tick()` < `tick()` < ...) is preserved for
    /// any caller that does not have a wallet in hand.
    ///
    /// **Hot-path callers should prefer [`tick_for_wallet`](Self::tick_for_wallet).**
    /// That API distributes contention across [`NUM_SHARDS`] mutexes and
    /// is what `LedgerManager::create_ledger` and
    /// `LedgerManager::append_to_ledger` use after Phase 1.3.
    ///
    /// Striping `tick()` across shards via a rotor would break monotonicity:
    /// distinct shards have independent HLC state, so two consecutive calls
    /// landing on different shards could produce equal or even decreasing
    /// packed timestamps. Pinning to shard 0 sidesteps this; legacy callers
    /// pay one mutex but they were not the bottleneck.
    pub fn tick(&self) -> LamportClock {
        self.shards[0].tick()
    }

    /// Increment and get a new timestamp for a specific wallet. The shard
    /// is derived deterministically from `blake3(wallet)[0]`, so two ticks
    /// for the same wallet always serialise but two ticks for different
    /// wallets almost always proceed in parallel. **Hot path API for the
    /// per-knot append flow** (`LedgerManager::append_to_ledger`,
    /// `LedgerManager::create_ledger`).
    pub fn tick_for_wallet(&self, wallet: &[u8]) -> LamportClock {
        let shard = shard_for_wallet(wallet) as usize;
        self.shards[shard].tick()
    }

    /// Increment on a caller-chosen shard (0..[`NUM_SHARDS`]). Useful for
    /// tests that need deterministic shard selection and for future callers
    /// (e.g. anchor producers) that want to pin work to a specific shard.
    pub fn tick_for_shard(&self, shard: u8) -> LamportClock {
        self.shards[shard as usize].tick()
    }

    /// Update clock based on a received message. Applied to **all** shards
    /// so subsequent ticks anywhere on the node respect the observed
    /// timestamp. This is rare-path consensus/network code; the 256 mutex
    /// acquires are amortised across the gossip cadence (default ~100 ms,
    /// see `rope-network/src/gossip.rs`), nowhere near the per-knot
    /// throughput envelope.
    pub fn observe(&self, other: &LamportClock) -> LamportClock {
        for shard in self.shards.iter() {
            let _ = shard.observe(other);
        }
        self.now()
    }

    /// Update clock based on multiple received messages. Same semantics as
    /// [`observe`](Self::observe) applied for each input.
    pub fn observe_many<'a>(&self, others: impl Iterator<Item = &'a LamportClock>) -> LamportClock {
        for other in others {
            for shard in self.shards.iter() {
                let _ = shard.observe(other);
            }
        }
        self.now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn make_node_id(id: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        NodeId::new(bytes)
    }

    // ------------------------------------------------------------------
    // LamportClock — wire-format tests preserved verbatim from v1.x
    // ------------------------------------------------------------------

    #[test]
    fn test_clock_increment() {
        let mut clock = LamportClock::new(make_node_id(1));

        assert_eq!(clock.time(), 0);
        clock.increment();
        assert_eq!(clock.time(), 1);
        clock.increment();
        assert_eq!(clock.time(), 2);
    }

    #[test]
    fn test_clock_observe() {
        let mut clock_a = LamportClock::new(make_node_id(1));
        let mut clock_b = LamportClock::new(make_node_id(2));

        // A increments a few times
        clock_a.increment();
        clock_a.increment();
        clock_a.increment();
        assert_eq!(clock_a.time(), 3);

        // B observes A's clock
        clock_b.observe(&clock_a);
        assert_eq!(clock_b.time(), 4); // max(0, 3) + 1
    }

    #[test]
    fn test_clock_ordering() {
        let mut clock_a = LamportClock::new(make_node_id(1));
        let mut clock_b = LamportClock::new(make_node_id(2));

        clock_a.increment();
        clock_b.observe(&clock_a);

        assert!(clock_a < clock_b);
    }

    #[test]
    fn test_clock_manager() {
        let manager = ClockManager::new(make_node_id(1));

        let t1 = manager.tick();
        let t2 = manager.tick();
        let t3 = manager.tick();

        assert!(t1 < t2);
        assert!(t2 < t3);
    }

    // ------------------------------------------------------------------
    // Pack / unpack HLC encoding
    // ------------------------------------------------------------------

    #[test]
    fn pack_unpack_roundtrip() {
        for &(ms, l) in &[
            (0u64, 0u64),
            (1, 0),
            (0, 1),
            (1_700_000_000_000, 0),
            (1_700_000_000_000, LOGICAL_MASK),
            (PHYSICAL_MAX, LOGICAL_MASK),
        ] {
            let packed = pack_hlc(ms, l);
            let (rms, rl) = unpack_hlc(packed);
            assert_eq!((rms, rl), (ms, l), "roundtrip failed for ({}, {})", ms, l);
        }
    }

    #[test]
    fn pack_clamps_overflow_inputs() {
        let packed = pack_hlc(u64::MAX, u64::MAX);
        let (ms, l) = unpack_hlc(packed);
        assert_eq!(ms, PHYSICAL_MAX);
        assert_eq!(l, LOGICAL_MASK);
    }

    #[test]
    fn pack_preserves_lexicographic_order() {
        let a = pack_hlc(1_700_000_000_000, 5);
        let b = pack_hlc(1_700_000_000_000, 6);
        let c = pack_hlc(1_700_000_000_001, 0);
        assert!(a < b, "logical bump must increase packed value");
        assert!(b < c, "ms bump must dominate logical reset");
    }

    // ------------------------------------------------------------------
    // HLC shard-level tests
    // ------------------------------------------------------------------

    #[test]
    fn hlc_shard_tick_is_strictly_monotonic() {
        let shard = HlcShard::new(make_node_id(1));
        let mut last = 0u64;
        for _ in 0..1000 {
            let t = shard.tick().time();
            assert!(
                t > last,
                "shard.tick must strictly increase: {} <= {}",
                t,
                last
            );
            last = t;
        }
    }

    #[test]
    fn hlc_shard_now_does_not_advance_clock() {
        let shard = HlcShard::new(make_node_id(2));
        let _ = shard.tick();
        let n1 = shard.now().time();
        let n2 = shard.now().time();
        assert_eq!(n1, n2, "now() must be idempotent");
    }

    #[test]
    fn hlc_shard_observe_jumps_forward_on_remote_ahead() {
        let shard = HlcShard::new(make_node_id(3));
        let _ = shard.tick();
        let local = shard.now().time();

        // Construct a remote timestamp clearly in the future.
        let (local_ms, _) = unpack_hlc(local);
        let remote_packed = pack_hlc(local_ms + 10_000, 0);
        let remote = LamportClock::with_time(remote_packed, make_node_id(99));

        let after = shard.observe(&remote).time();
        assert!(
            after >= remote_packed,
            "observe must catch up to the remote: after={} remote={}",
            after,
            remote_packed
        );
    }

    // ------------------------------------------------------------------
    // ClockManager — sharded behaviour
    // ------------------------------------------------------------------

    #[test]
    fn manager_has_expected_shard_count() {
        let m = ClockManager::new(make_node_id(1));
        assert_eq!(m.shard_count(), NUM_SHARDS);
    }

    #[test]
    fn tick_for_wallet_is_deterministic_per_wallet() {
        let m = ClockManager::new(make_node_id(1));
        let w = b"some-wallet-bytes-1234567890";
        let a = m.tick_for_wallet(w).time();
        let b = m.tick_for_wallet(w).time();
        assert!(a < b, "same wallet must serialise on its own shard");
    }

    #[test]
    fn shard_for_wallet_is_pure() {
        // Stability across calls; uniform distribution checked elsewhere.
        let w = b"abc";
        assert_eq!(shard_for_wallet(w), shard_for_wallet(w));
    }

    #[test]
    fn shard_for_wallet_distributes_uniformly_enough() {
        // Hash 4096 distinct addresses, count per shard, expect mean of 16
        // per shard with no shard receiving more than 4× the mean. This is
        // a sanity check, not a statistical proof.
        let mut buckets = [0u32; NUM_SHARDS];
        for i in 0u32..4096 {
            let addr = i.to_le_bytes();
            buckets[shard_for_wallet(&addr) as usize] += 1;
        }
        let max = *buckets.iter().max().unwrap();
        let mean = 16;
        assert!(
            max < mean * 4,
            "shard distribution too lumpy: max={} mean={}",
            max,
            mean
        );
    }

    #[test]
    fn legacy_tick_pins_to_shard_zero_for_monotonicity() {
        // The legacy `tick()` API is contractually monotonic. To preserve
        // that without coordinating across shards, it pins to shard 0.
        // Verify: after N ticks, only shard 0 has been advanced.
        let m = ClockManager::new(make_node_id(1));
        for _ in 0..32 {
            let _ = m.tick();
        }
        for (i, s) in m.shards.iter().enumerate() {
            let st = s.state.lock();
            if i == 0 {
                assert!(
                    st.physical_ms > 0,
                    "shard 0 must have advanced after legacy ticks"
                );
            } else {
                assert_eq!(
                    st.physical_ms, 0,
                    "shard {} must remain untouched by legacy tick()",
                    i
                );
            }
        }
    }

    #[test]
    fn now_returns_max_across_shards() {
        let m = ClockManager::new(make_node_id(1));
        // Tick on a few shards
        let _ = m.tick_for_shard(7);
        let _ = m.tick_for_shard(42);
        let _ = m.tick_for_shard(123);

        let now = m.now().time();
        // now must be >= every shard's individual snapshot
        for s in m.shards.iter() {
            assert!(
                now >= s.now().time(),
                "now() must dominate every shard snapshot"
            );
        }
    }

    #[test]
    fn observe_propagates_to_all_shards() {
        let m = ClockManager::new(make_node_id(1));
        // Tick once so we have a baseline physical_ms on shard 0.
        let _ = m.tick_for_shard(0);
        let baseline = m.shards[0].now().time();
        let (baseline_ms, _) = unpack_hlc(baseline);

        // Construct a remote timestamp 30 s ahead.
        let remote_packed = pack_hlc(baseline_ms + 30_000, 0);
        let remote = LamportClock::with_time(remote_packed, make_node_id(99));
        let _ = m.observe(&remote);

        // Every shard should now report physical_ms >= baseline_ms + 30_000
        for (i, s) in m.shards.iter().enumerate() {
            let st = s.state.lock();
            assert!(
                st.physical_ms >= baseline_ms + 30_000,
                "shard {} did not absorb remote observation: {} < {}",
                i,
                st.physical_ms,
                baseline_ms + 30_000
            );
        }
    }

    #[test]
    fn observe_many_propagates_each_input() {
        let m = ClockManager::new(make_node_id(1));
        let _ = m.tick_for_shard(0);
        let baseline_ms = unpack_hlc(m.shards[0].now().time()).0;

        let remotes = [
            LamportClock::with_time(pack_hlc(baseline_ms + 5_000, 0), make_node_id(2)),
            LamportClock::with_time(pack_hlc(baseline_ms + 50_000, 0), make_node_id(3)),
        ];

        let _ = m.observe_many(remotes.iter());

        for s in m.shards.iter() {
            let st = s.state.lock();
            assert!(st.physical_ms >= baseline_ms + 50_000);
        }
    }

    // ------------------------------------------------------------------
    // Concurrency stress
    // ------------------------------------------------------------------

    #[test]
    fn parallel_per_wallet_ticks_are_uncontended_and_monotonic() {
        let m = Arc::new(ClockManager::new(make_node_id(1)));
        let mut handles = Vec::new();
        for w in 0u8..32 {
            let m = m.clone();
            handles.push(thread::spawn(move || {
                let wallet = [w; 20];
                let mut last = 0u64;
                for _ in 0..200 {
                    let t = m.tick_for_wallet(&wallet).time();
                    assert!(
                        t > last,
                        "monotonicity violated on wallet {}: {} <= {}",
                        w,
                        t,
                        last
                    );
                    last = t;
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn parallel_legacy_ticks_are_globally_monotonic_via_shard_zero() {
        // Pinning `tick()` to shard 0 means cross-thread ordering is also
        // strictly monotonic (one mutex serialises every issuer). Pool the
        // samples from every thread, sort, and verify no duplicates.
        let m = Arc::new(ClockManager::new(make_node_id(1)));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let m = m.clone();
            handles.push(thread::spawn(move || {
                let mut samples = Vec::with_capacity(100);
                for _ in 0..100 {
                    samples.push(m.tick().time());
                }
                samples
            }));
        }
        let mut all = Vec::with_capacity(800);
        for h in handles {
            all.extend(h.join().unwrap());
        }
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(
            all.len(),
            n,
            "legacy tick() must be globally monotonic — no duplicates across threads"
        );
    }
}
