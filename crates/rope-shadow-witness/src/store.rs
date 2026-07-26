//! RocksDB persistence for the v2 shadow chain.
//!
//! Two column families:
//!
//! - `chain`: key = `string_id_bytes (32) || event_id_be (8)`, value
//!   = bincode of [`ShadowChainEntry`]. One row per observed knot.
//! - `heads`: key = `string_id_bytes (32)`, value = bincode of
//!   [`ShadowChainHead`]. The latest event_id and `h_i` per string.
//!
//! Bincode is used internally because (a) it is already a workspace
//! dep and (b) the on-disk format is private to this crate (no other
//! consumer reads it). Cross-process consumers go through the JSON-RPC
//! server in [`crate::server`], which serialises with `serde_json`.

use std::path::Path;
use std::sync::Arc;

use rocksdb::{BoundColumnFamily, ColumnFamilyDescriptor, Options, DB};

use crate::error::{ShadowWitnessError, ShadowWitnessResult};
use crate::{ShadowChainEntry, ShadowChainHead};

const CF_CHAIN: &str = "chain";
const CF_HEADS: &str = "heads";

pub struct ShadowChainStore {
    db: Arc<DB>,
}

impl ShadowChainStore {
    /// Open or create a shadow store at `path`. Both column families
    /// are auto-created if missing.
    pub fn open<P: AsRef<Path>>(path: P) -> ShadowWitnessResult<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cf_descriptors = vec![
            ColumnFamilyDescriptor::new(CF_CHAIN, Options::default()),
            ColumnFamilyDescriptor::new(CF_HEADS, Options::default()),
        ];

        let db = DB::open_cf_descriptors(&opts, path, cf_descriptors)?;

        Ok(Self { db: Arc::new(db) })
    }

    fn cf_chain(&self) -> Arc<BoundColumnFamily<'_>> {
        self.db
            .cf_handle(CF_CHAIN)
            .expect("chain column family was created at open()")
    }

    fn cf_heads(&self) -> Arc<BoundColumnFamily<'_>> {
        self.db
            .cf_handle(CF_HEADS)
            .expect("heads column family was created at open()")
    }

    fn chain_key(string_id: &[u8; 32], event_id: u64) -> [u8; 40] {
        let mut k = [0u8; 40];
        k[..32].copy_from_slice(string_id);
        k[32..].copy_from_slice(&event_id.to_be_bytes());
        k
    }

    /// Atomically write an entry to the chain CF and update the heads
    /// CF to point at it.
    pub fn put_entry_and_advance_head(
        &self,
        string_id_bytes: &[u8; 32],
        entry: &ShadowChainEntry,
        new_head: &ShadowChainHead,
    ) -> ShadowWitnessResult<()> {
        let cf_chain = self.cf_chain();
        let cf_heads = self.cf_heads();
        let mut batch = rocksdb::WriteBatch::default();

        let chain_key = Self::chain_key(string_id_bytes, entry.event_id);
        let entry_bytes = serde_json::to_vec(entry)?;
        batch.put_cf(&cf_chain, chain_key, entry_bytes);

        let head_bytes = serde_json::to_vec(new_head)?;
        batch.put_cf(&cf_heads, string_id_bytes, head_bytes);

        self.db.write(batch)?;
        Ok(())
    }

    pub fn get_head(
        &self,
        string_id_bytes: &[u8; 32],
    ) -> ShadowWitnessResult<Option<ShadowChainHead>> {
        let cf = self.cf_heads();
        match self.db.get_cf(&cf, string_id_bytes)? {
            None => Ok(None),
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        }
    }

    pub fn get_entry(
        &self,
        string_id_bytes: &[u8; 32],
        event_id: u64,
    ) -> ShadowWitnessResult<Option<ShadowChainEntry>> {
        let cf = self.cf_chain();
        let key = Self::chain_key(string_id_bytes, event_id);
        match self.db.get_cf(&cf, key)? {
            None => Ok(None),
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        }
    }

    /// Walk a window of the v2 chain for one string.
    ///
    /// Returns at most `limit` entries starting at `offset`, in
    /// ascending `event_id` order.
    pub fn walk_chain(
        &self,
        string_id_bytes: &[u8; 32],
        offset: usize,
        limit: usize,
    ) -> ShadowWitnessResult<Vec<ShadowChainEntry>> {
        let cf = self.cf_chain();
        let prefix_start = Self::chain_key(string_id_bytes, 0);
        let prefix_end_event = u64::MAX;
        let prefix_end = Self::chain_key(string_id_bytes, prefix_end_event);

        let mut out = Vec::with_capacity(limit.min(1024));
        let mut skipped = 0usize;

        let iter = self.db.iterator_cf(
            &cf,
            rocksdb::IteratorMode::From(&prefix_start, rocksdb::Direction::Forward),
        );
        for item in iter {
            let (k, v) = item?;
            if k.as_ref() > prefix_end.as_slice() {
                break;
            }
            if &k[..32] != string_id_bytes {
                break;
            }
            if skipped < offset {
                skipped += 1;
                continue;
            }
            if out.len() >= limit {
                break;
            }
            let entry: ShadowChainEntry = serde_json::from_slice(&v)?;
            out.push(entry);
        }

        Ok(out)
    }

    /// Aggregate count: total entries across all strings.
    ///
    /// Walks the chain column family. Used by `rope_v2_status` and the
    /// promotion health gate. Cost is O(N) over the chain CF; intended
    /// for occasional polling, not per-RPC-call evaluation.
    pub fn total_entries(&self) -> ShadowWitnessResult<u64> {
        let cf = self.cf_chain();
        let mut n = 0u64;
        let iter = self
            .db
            .iterator_cf(&cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let _ = item?;
            n += 1;
        }
        Ok(n)
    }

    /// Aggregate count: total distinct strings (one head per string).
    pub fn total_strings(&self) -> ShadowWitnessResult<u64> {
        let cf = self.cf_heads();
        let mut n = 0u64;
        let iter = self
            .db
            .iterator_cf(&cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let _ = item?;
            n += 1;
        }
        Ok(n)
    }

    /// Earliest `updated_at_unix` across all heads.
    ///
    /// Returns 0 when the store is empty. Used by the health gate to
    /// measure the actual soak duration (time since first observation),
    /// which survives binary refreshes and process restarts and is
    /// therefore the appropriate "soak window" anchor for the
    /// promotion gate (rather than systemd's `ActiveEnterTimestamp`,
    /// which resets every redeploy).
    pub fn first_observed_at_unix(&self) -> ShadowWitnessResult<i64> {
        let cf = self.cf_heads();
        let mut earliest: i64 = i64::MAX;
        let iter = self
            .db
            .iterator_cf(&cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let (_, v) = item?;
            let head: ShadowChainHead = serde_json::from_slice(&v)?;
            if head.updated_at_unix < earliest && head.updated_at_unix > 0 {
                earliest = head.updated_at_unix;
            }
        }
        Ok(if earliest == i64::MAX { 0 } else { earliest })
    }

    /// Most recent `updated_at_unix` across all heads.
    ///
    /// Returns 0 when the store is empty. Used by the health gate to
    /// detect a stalled witness (last observation more than N seconds ago).
    pub fn last_observed_at_unix(&self) -> ShadowWitnessResult<i64> {
        let cf = self.cf_heads();
        let mut latest: i64 = 0;
        let iter = self
            .db
            .iterator_cf(&cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let (_, v) = item?;
            let head: ShadowChainHead = serde_json::from_slice(&v)?;
            if head.updated_at_unix > latest {
                latest = head.updated_at_unix;
            }
        }
        Ok(latest)
    }

    /// Count entries on a string. Useful for stats endpoints.
    pub fn count_entries(&self, string_id_bytes: &[u8; 32]) -> ShadowWitnessResult<u64> {
        let cf = self.cf_chain();
        let prefix_start = Self::chain_key(string_id_bytes, 0);
        let prefix_end = Self::chain_key(string_id_bytes, u64::MAX);

        let mut n = 0u64;
        let iter = self.db.iterator_cf(
            &cf,
            rocksdb::IteratorMode::From(&prefix_start, rocksdb::Direction::Forward),
        );
        for item in iter {
            let (k, _) = item?;
            if k.as_ref() > prefix_end.as_slice() {
                break;
            }
            if &k[..32] != string_id_bytes {
                break;
            }
            n += 1;
        }
        Ok(n)
    }
}

/// Decode a `0x`-prefixed hex string into a fixed-length 32-byte
/// canonical string identifier.
pub fn parse_string_id_hex(s: &str) -> ShadowWitnessResult<[u8; 32]> {
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(stripped)?;
    if bytes.len() != 32 {
        return Err(ShadowWitnessError::Internal(format!(
            "string_id must be 32 bytes after hex decode, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rope_core::knot_hash::{EventMetadataHash, KnotHash};

    fn sample_entry(event_id: u64) -> ShadowChainEntry {
        ShadowChainEntry {
            string_id: "0x".to_string() + &"aa".repeat(32),
            event_id,
            event_type: "append".to_string(),
            event_metadata_hash: EventMetadataHash([1u8; 32]),
            knot_hash: KnotHash([(event_id as u8).wrapping_add(2); 32]),
            previous_hash: KnotHash([(event_id as u8).wrapping_add(1); 32]),
            is_tombstone: false,
            observed_at_unix: 1700000000 + event_id as i64,
        }
    }

    fn sample_head(event_id: u64, hash: KnotHash) -> ShadowChainHead {
        ShadowChainHead {
            latest_event_id: event_id,
            latest_knot_hash: hash,
            updated_at_unix: 1700000000 + event_id as i64,
        }
    }

    #[test]
    fn open_creates_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let s = ShadowChainStore::open(dir.path()).unwrap();
        let id = [0xAA; 32];
        assert!(s.get_head(&id).unwrap().is_none());
        assert_eq!(s.count_entries(&id).unwrap(), 0);
    }

    #[test]
    fn put_and_get_entry() {
        let dir = tempfile::tempdir().unwrap();
        let s = ShadowChainStore::open(dir.path()).unwrap();
        let id = [0xAA; 32];
        let e0 = sample_entry(0);
        let head = sample_head(0, e0.knot_hash);
        s.put_entry_and_advance_head(&id, &e0, &head).unwrap();

        let got = s.get_entry(&id, 0).unwrap().unwrap();
        assert_eq!(got.event_id, 0);
        assert_eq!(got.knot_hash, e0.knot_hash);

        let head_got = s.get_head(&id).unwrap().unwrap();
        assert_eq!(head_got.latest_event_id, 0);
    }

    #[test]
    fn walk_chain_returns_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let s = ShadowChainStore::open(dir.path()).unwrap();
        let id = [0xAA; 32];
        for i in 0..10u64 {
            let e = sample_entry(i);
            let h = sample_head(i, e.knot_hash);
            s.put_entry_and_advance_head(&id, &e, &h).unwrap();
        }
        let window = s.walk_chain(&id, 2, 5).unwrap();
        assert_eq!(window.len(), 5);
        assert_eq!(window[0].event_id, 2);
        assert_eq!(window[4].event_id, 6);
    }

    #[test]
    fn walks_only_one_string() {
        let dir = tempfile::tempdir().unwrap();
        let s = ShadowChainStore::open(dir.path()).unwrap();
        let id_a = [0xAA; 32];
        let id_b = [0xBB; 32];
        let e0 = sample_entry(0);
        let h0 = sample_head(0, e0.knot_hash);
        s.put_entry_and_advance_head(&id_a, &e0, &h0).unwrap();
        s.put_entry_and_advance_head(&id_b, &e0, &h0).unwrap();
        assert_eq!(s.count_entries(&id_a).unwrap(), 1);
        assert_eq!(s.count_entries(&id_b).unwrap(), 1);
        let walk_a = s.walk_chain(&id_a, 0, 100).unwrap();
        assert_eq!(walk_a.len(), 1);
    }

    #[test]
    fn parse_string_id_round_trip() {
        let id = "0x".to_string() + &"ab".repeat(32);
        let bytes = parse_string_id_hex(&id).unwrap();
        assert_eq!(bytes, [0xAB; 32]);
    }

    #[test]
    fn parse_string_id_rejects_short() {
        let id = "0x1234";
        assert!(parse_string_id_hex(id).is_err());
    }
}
