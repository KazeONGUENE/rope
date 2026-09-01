//! RocksDB column families, key encoding, and value shapes for the
//! per-address transaction / log / internal-txn index.
//!
//! # Design
//!
//! Every "an address participated in something at (block, index)" fact
//! is stored as a fixed-length composite key:
//!
//! ```text
//! addr(20) || block_number_be(8) || index_be(4)
//! ```
//!
//! Big-endian on both sub-fields keeps the natural byte-lex order equal
//! to the natural numeric order, so a **reverse RocksDB iterator seeded
//! at `addr || u64::MAX || u32::MAX`** walks newest-first and stops
//! cleanly at the first key whose 20-byte prefix is no longer the
//! target address. That is the mechanism DCScan's paginated tab
//! endpoints will use to answer any address query in O(page_size).
//!
//! The value payload for each fact is a compact `bincode`-serialised
//! reference struct that carries the minimum needed to render a table
//! row without a chain re-fetch (block, index, hash, from/to, wei,
//! status, gas_used, timestamp for txs; topics, data, tx-hash for
//! logs). The full transaction / block bodies stay on Reth; the reader
//! can hydrate on-demand when the user opens a tx detail page.
//!
//! # Reorg safety
//!
//! Because chain reorgs must delete every entry that was written under
//! an orphaned block, the writer additionally records the set of
//! addresses touched by every canonical block into the `meta` CF at
//! key `block_addrs || block_number_be(8)`. On a reorg unwind the
//! reorg handler enumerates that set and deletes the corresponding
//! `(addr, block, *)` prefix from each of the three data CFs, then
//! deletes the `block_addrs` entry itself. The last 128 canonical
//! block hashes are also recorded so the tip follower can detect a
//! reorg by comparing the new block's `parent_hash` against the stored
//! hash for `block_number - 1`.
//!
//! Fixed-length keys let us use a `PrefixExtractor(20)` on all three
//! data CFs, which is the pattern the RocksDB docs recommend for
//! "seek to prefix + iterate" workloads. Value payloads are compressed
//! with LZ4 to trade a few CPU cycles for disk footprint on wide
//! historic ranges.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------
// Column families
// ---------------------------------------------------------------------

/// One entry per (address, block, tx_index) - the address was `from`
/// or `to` of the transaction at that position.
pub const CF_ADDR_TX: &str = "addr_tx";

/// One entry per (address, block, log_index) - the address matched
/// either `log.address` (contract that emitted the log) or one of the
/// indexed topics that decode to a 20-byte address (Transfer.from,
/// Transfer.to, Approval.owner, Approval.spender, …). The value
/// carries the raw log so the reader can decode DCR-20 / ERC-721 /
/// ERC-3643 transfers without a second RPC call.
pub const CF_ADDR_LOG: &str = "addr_log";

/// One entry per (address, block, call_seq) - reserved for internal
/// transactions (traces) once a source is wired (Otterscan
/// `ots_getInternalOperationsByBlockNumber` or geth-style
/// `debug_traceBlockByNumber` + callTracer). Written to at Phase 2;
/// the CF is created at open time so the schema is stable.
pub const CF_ADDR_INTERNAL: &str = "addr_internal";

/// Small key-value bag: current head, backfill cursor, per-block
/// address sets (for reorg cleanup), last-128 canonical block hashes,
/// schema version, and any coarse counters exposed by
/// `/api/v2/index/status`.
pub const CF_META: &str = "meta";

/// All CFs opened at start-up. Kept as an array so both the writer
/// (read-write) and the reader (read-only) can loop over the same
/// list, and so the systemd unit's `--reset-index` fast-path knows
/// exactly which CFs to drop.
pub const ALL_CFS: &[&str] = &[CF_ADDR_TX, CF_ADDR_LOG, CF_ADDR_INTERNAL, CF_META];

// ---------------------------------------------------------------------
// Key encoding
// ---------------------------------------------------------------------

pub const ADDR_LEN: usize = 20;
pub const KEY_LEN: usize = ADDR_LEN + 8 + 4; // addr || u64 block || u32 idx

/// Build a composite `addr || block_be || idx_be` key. Panics only if
/// `addr` is not exactly 20 bytes - callers are expected to pre-normalise
/// via [`normalise_address_bytes`].
#[inline]
pub fn encode_key(addr: &[u8; ADDR_LEN], block: u64, idx: u32) -> [u8; KEY_LEN] {
    let mut k = [0u8; KEY_LEN];
    k[..ADDR_LEN].copy_from_slice(addr);
    k[ADDR_LEN..ADDR_LEN + 8].copy_from_slice(&block.to_be_bytes());
    k[ADDR_LEN + 8..].copy_from_slice(&idx.to_be_bytes());
    k
}

/// The upper-bound seek key for a given address - one past any legal
/// key for that address. Used to seed reverse iterators newest-first.
#[inline]
pub fn upper_bound(addr: &[u8; ADDR_LEN]) -> [u8; KEY_LEN] {
    encode_key(addr, u64::MAX, u32::MAX)
}

/// The lower-bound key for a given address - the first byte of the
/// address prefix. Iterators must halt when the current key's
/// first-20-bytes stops matching this.
#[inline]
pub fn lower_bound(addr: &[u8; ADDR_LEN]) -> [u8; ADDR_LEN] {
    *addr
}

/// Decode a composite key back to its parts. Returns `None` if the
/// slice length is wrong.
#[inline]
pub fn decode_key(k: &[u8]) -> Option<([u8; ADDR_LEN], u64, u32)> {
    if k.len() != KEY_LEN {
        return None;
    }
    let mut addr = [0u8; ADDR_LEN];
    addr.copy_from_slice(&k[..ADDR_LEN]);
    let mut b = [0u8; 8];
    b.copy_from_slice(&k[ADDR_LEN..ADDR_LEN + 8]);
    let block = u64::from_be_bytes(b);
    let mut i = [0u8; 4];
    i.copy_from_slice(&k[ADDR_LEN + 8..]);
    let idx = u32::from_be_bytes(i);
    Some((addr, block, idx))
}

/// Parse a `0x`-prefixed or bare hex string into a fixed 20-byte
/// address. Case-insensitive. Returns `None` for any malformed input;
/// the reader / writer treat that as "skip this fact" rather than
/// panic.
pub fn normalise_address(s: &str) -> Option<[u8; ADDR_LEN]> {
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    if stripped.len() != 40 {
        return None;
    }
    let mut out = [0u8; ADDR_LEN];
    for i in 0..ADDR_LEN {
        let hi = hex_nibble(stripped.as_bytes()[i * 2])?;
        let lo = hex_nibble(stripped.as_bytes()[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// Coerce a raw byte slice of length 20 into a fixed array. Used when
/// decoding indexed log topics (right-most 20 bytes of a 32-byte word).
pub fn normalise_address_bytes(b: &[u8]) -> Option<[u8; ADDR_LEN]> {
    if b.len() != ADDR_LEN {
        return None;
    }
    let mut out = [0u8; ADDR_LEN];
    out.copy_from_slice(b);
    Some(out)
}

/// Render a 20-byte address back to canonical lower-case hex with the
/// `0x` prefix. The reader hands these strings to axum handlers.
pub fn format_address(addr: &[u8; ADDR_LEN]) -> String {
    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for b in addr {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Value payloads
// ---------------------------------------------------------------------

/// Compact reference to a transaction. Enough to render a table row
/// (from, to, value, status, gas, timestamp) plus the tx-hash so the
/// reader can hydrate the full body on click.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxRef {
    /// 32-byte tx hash (raw bytes, not hex).
    pub tx_hash: [u8; 32],
    /// 32-byte block hash - pinned to detect reorg drift server-side
    /// when the reader hydrates the full tx via RPC.
    pub block_hash: [u8; 32],
    pub block_number: u64,
    pub tx_index: u32,
    /// Unix seconds; 0 if the writer failed to parse the block
    /// timestamp (never expected in practice).
    pub block_timestamp: i64,
    /// From address (always populated).
    pub from: [u8; 20],
    /// To address; `None` for contract-creation txs.
    pub to: Option<[u8; 20]>,
    /// Value in wei (u128 fits every realistic native transfer;
    /// 2^128 wei is ~340 undecillion FAT, comfortably above the
    /// asymptotic max supply of 18e9 FAT).
    pub value_wei: u128,
    /// Gas used (0 if the writer indexes txs before receipts are
    /// available; the reader may fall back to eth_getTransactionReceipt).
    pub gas_used: u64,
    /// Transaction receipt status: `1` = success, `0` = revert, `2` =
    /// receipt not yet indexed (writer wrote the tx summary before
    /// enriching with the receipt).
    pub status: u8,
    /// Which role the indexed address played: `from`, `to`, or
    /// `both` (self-send). Lets the reader colour rows without a
    /// second address comparison.
    pub role: TxRole,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TxRole {
    From,
    To,
    Both,
}

/// Compact reference to an event log emitted by / relevant to an
/// address. Preserves the full topics + data so the reader can decode
/// DCR-20 `Transfer(from, to, value)` and `Approval(owner, spender,
/// value)` without a second RPC round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogRef {
    pub tx_hash: [u8; 32],
    pub block_hash: [u8; 32],
    pub block_number: u64,
    pub block_timestamp: i64,
    pub tx_index: u32,
    pub log_index: u32,
    /// The contract that emitted the log.
    pub emitter: [u8; 20],
    /// Up to 4 topics per EVM spec.
    pub topics: Vec<[u8; 32]>,
    /// Raw ABI-encoded data payload.
    pub data: Vec<u8>,
    /// Why this log matched this address: `emitter` (address ==
    /// log.address, i.e. someone hit a method on this contract),
    /// `topic1` / `topic2` / `topic3` (address decodes from that
    /// indexed topic).
    pub role: LogRole,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LogRole {
    Emitter,
    Topic1,
    Topic2,
    Topic3,
}

/// Placeholder for the internal-txn payload. The Phase 2 tracer will
/// fill this in. Kept in the schema now so we don't need a data-file
/// migration later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalRef {
    pub tx_hash: [u8; 32],
    pub block_number: u64,
    pub call_seq: u32,
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub value_wei: u128,
    /// `CALL`, `STATICCALL`, `DELEGATECALL`, `CREATE`, `SELFDESTRUCT`.
    pub op: String,
    /// `1` = success, `0` = revert.
    pub status: u8,
}

// ---------------------------------------------------------------------
// meta CF keys
// ---------------------------------------------------------------------

/// `meta[b"schema_version"]` = little-endian u32. Bumped when key
/// shapes or payload structs change in a breaking way. The reader
/// refuses to open a store whose schema version doesn't match this
/// constant, forcing an operator-initiated `--reset-index` rebuild.
pub const SCHEMA_VERSION: u32 = 1;
pub const META_KEY_SCHEMA_VERSION: &[u8] = b"schema_version";

/// `meta[b"head_block"]` = the highest block number the writer has
/// fully ingested and fsync'd. The reader may serve stale-tolerant
/// answers past that height by falling back to the legacy RPC scan.
pub const META_KEY_HEAD_BLOCK: &[u8] = b"head_block";

/// `meta[b"backfill_low_water"]` = the lowest block number that has
/// been ingested by the historical backfiller. When this reaches 0
/// the whole chain is indexed.
pub const META_KEY_BACKFILL_LOW: &[u8] = b"backfill_low_water";

/// `meta[b"backfill_high_water"]` = the highest block number the
/// backfiller was told to cover. Usually the tip at the moment the
/// service first started; frozen thereafter.
pub const META_KEY_BACKFILL_HIGH: &[u8] = b"backfill_high_water";

/// Prefix for per-block address-set keys: `b"block_addrs" || block_be(8)`.
/// Value is `bincode(Vec<[u8;20]>)`.
pub const META_KEY_BLOCK_ADDRS_PREFIX: &[u8] = b"block_addrs";

/// Prefix for per-block canonical-hash keys: `b"block_hash" || block_be(8)`.
/// Value is `[u8; 32]`. Only the last 128 blocks are retained.
pub const META_KEY_BLOCK_HASH_PREFIX: &[u8] = b"block_hash";

/// How many canonical block hashes to retain for reorg detection.
/// Datachain Rope's target reorg depth is single-digit blocks; 128
/// blocks (~6 min at 3s knot cadence) is a safety margin.
pub const HASH_RETENTION_BLOCKS: u64 = 128;

/// Build the meta-CF key for a per-block address set.
pub fn meta_block_addrs_key(block: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(META_KEY_BLOCK_ADDRS_PREFIX.len() + 8);
    k.extend_from_slice(META_KEY_BLOCK_ADDRS_PREFIX);
    k.extend_from_slice(&block.to_be_bytes());
    k
}

/// Build the meta-CF key for a per-block canonical hash.
pub fn meta_block_hash_key(block: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(META_KEY_BLOCK_HASH_PREFIX.len() + 8);
    k.extend_from_slice(META_KEY_BLOCK_HASH_PREFIX);
    k.extend_from_slice(&block.to_be_bytes());
    k
}

// ---------------------------------------------------------------------
// Cursor encoding
// ---------------------------------------------------------------------

/// Opaque cursor handed back to clients so they can page a large
/// result set without leaking the internal key layout. Encoded as
/// base64(bincode(Cursor)).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cursor {
    pub block: u64,
    pub idx: u32,
}

impl Cursor {
    pub fn encode(&self) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let bytes = bincode::serialize(self).expect("cursor serialisation is infallible");
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub fn decode(s: &str) -> Option<Self> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let bytes = URL_SAFE_NO_PAD.decode(s.as_bytes()).ok()?;
        bincode::deserialize::<Cursor>(&bytes).ok()
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip() {
        let addr = [1u8; ADDR_LEN];
        let k = encode_key(&addr, 12_345_678, 42);
        let (a, b, i) = decode_key(&k).expect("decode");
        assert_eq!(a, addr);
        assert_eq!(b, 12_345_678);
        assert_eq!(i, 42);
    }

    #[test]
    fn key_lex_order_matches_numeric_order() {
        // Two entries for the same address at (100, 0) and (100, 1) -
        // the (100, 1) key must sort strictly after (100, 0).
        let addr = [7u8; ADDR_LEN];
        let a = encode_key(&addr, 100, 0);
        let b = encode_key(&addr, 100, 1);
        assert!(a < b, "same-block idx ordering broken: {a:?} !< {b:?}");

        // Cross-block: (99, u32::MAX) must sort strictly before (100, 0).
        let c = encode_key(&addr, 99, u32::MAX);
        let d = encode_key(&addr, 100, 0);
        assert!(c < d, "cross-block block ordering broken");
    }

    #[test]
    fn upper_bound_is_max_of_addr_range() {
        let addr = [3u8; ADDR_LEN];
        let ub = upper_bound(&addr);
        let any = encode_key(&addr, u64::MAX - 1, u32::MAX - 1);
        assert!(any < ub);
    }

    #[test]
    fn addresses_are_separated_by_prefix() {
        // Two different addresses must never produce interleaved keys -
        // that is the invariant that makes prefix-bounded reverse
        // iteration correct.
        let addr_a = [0x11u8; ADDR_LEN];
        let addr_b = [0x12u8; ADDR_LEN];
        let key_a_high = encode_key(&addr_a, u64::MAX, u32::MAX);
        let key_b_low = encode_key(&addr_b, 0, 0);
        assert!(key_a_high < key_b_low, "prefix separation broken");
    }

    #[test]
    fn normalise_address_accepts_both_prefixes_and_cases() {
        let a = normalise_address("0xdeadBEEF00000000000000000000000000000001").unwrap();
        let b = normalise_address("deadbeef00000000000000000000000000000001").unwrap();
        let c = normalise_address("DEADBEEF00000000000000000000000000000001").unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a[0], 0xde);
        assert_eq!(a[19], 0x01);
    }

    #[test]
    fn normalise_address_rejects_bad_input() {
        assert!(normalise_address("").is_none());
        assert!(normalise_address("0xdead").is_none());
        assert!(normalise_address("0xZZeadBEEF00000000000000000000000000000001").is_none());
    }

    #[test]
    fn format_address_is_lowercase_and_prefixed() {
        let addr = [0xab, 0xcd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(format_address(&addr), "0xabcd000000000000000000000000000000000000");
    }

    #[test]
    fn cursor_roundtrip() {
        let c = Cursor {
            block: 42,
            idx: 7,
        };
        let enc = c.encode();
        let dec = Cursor::decode(&enc).unwrap();
        assert_eq!(c, dec);
    }

    #[test]
    fn tx_ref_bincode_stable_size() {
        let r = TxRef {
            tx_hash: [1u8; 32],
            block_hash: [2u8; 32],
            block_number: 100,
            tx_index: 3,
            block_timestamp: 1700000000,
            from: [3u8; 20],
            to: Some([4u8; 20]),
            value_wei: 12345,
            gas_used: 21000,
            status: 1,
            role: TxRole::From,
        };
        let bytes = bincode::serialize(&r).unwrap();
        let round: TxRef = bincode::deserialize(&bytes).unwrap();
        assert_eq!(r, round);
    }
}
