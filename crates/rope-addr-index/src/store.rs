//! RocksDB store lifecycle for the per-address index.
//!
//! Opens the database (creating the column families defined in
//! [`crate::schema`] on first launch), verifies the on-disk schema
//! version matches the code, and exposes typed read / write helpers
//! that the writer, reader, and reorg handler all share.
//!
//! Both the indexer service (read-write) and the DCScan reader
//! (read-only) hit the same on-disk layout via [`Store::open_rw`] and
//! [`Store::open_ro`] respectively. The read-only handle lets multiple
//! processes co-exist safely against a live indexer without racing
//! writes.

use crate::schema::{
    self, meta_block_addrs_key, meta_block_hash_key, ALL_CFS, CF_ADDR_INTERNAL, CF_ADDR_LOG,
    CF_ADDR_TX, CF_META, META_KEY_BACKFILL_HIGH, META_KEY_BACKFILL_LOW, META_KEY_HEAD_BLOCK,
    META_KEY_SCHEMA_VERSION, SCHEMA_VERSION,
};
use rocksdb::{
    BoundColumnFamily, ColumnFamilyDescriptor, DBCompressionType, Options, SliceTransform,
    WriteBatch, WriteOptions, DB,
};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("rocksdb error: {0}")]
    Db(#[from] rocksdb::Error),
    #[error("bincode error: {0}")]
    Bincode(#[from] Box<bincode::ErrorKind>),
    #[error("missing column family {0}")]
    MissingCf(&'static str),
    #[error("on-disk schema version {found} does not match code version {expected}; run with `--reset-index` to rebuild")]
    SchemaMismatch { found: u32, expected: u32 },
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Handle to the opened RocksDB store. Cheap to clone via `Arc`.
#[derive(Clone)]
pub struct Store {
    db: Arc<DB>,
}

impl Store {
    /// Open (or create) the store read-write. Called by the indexer
    /// service exactly once at start-up.
    pub fn open_rw(path: &Path) -> StoreResult<Self> {
        let (db_opts, cf_descriptors) = build_options();
        let db = DB::open_cf_descriptors(&db_opts, path, cf_descriptors)?;
        let store = Store { db: Arc::new(db) };
        store.check_or_stamp_schema()?;
        Ok(store)
    }

    /// Open the store read-only. Called by DCScan (the reader
    /// process). RocksDB supports concurrent read-only opens against
    /// a single writer without any coordination on our side.
    pub fn open_ro(path: &Path) -> StoreResult<Self> {
        let (db_opts, cf_descriptors) = build_options();
        let cf_names: Vec<&str> = cf_descriptors.iter().map(|d| d.name()).collect();
        let db = DB::open_cf_for_read_only(&db_opts, path, cf_names, false)?;
        let store = Store { db: Arc::new(db) };
        // Best-effort check: a fresh read-only handle with no schema
        // key yet is treated as an empty (still-warming) store and
        // reader falls back to legacy paths - do not hard-fail here.
        if let Ok(Some(v)) = store.raw_meta(META_KEY_SCHEMA_VERSION) {
            let found = decode_u32(&v).unwrap_or(0);
            if found != SCHEMA_VERSION {
                return Err(StoreError::SchemaMismatch {
                    found,
                    expected: SCHEMA_VERSION,
                });
            }
        }
        Ok(store)
    }

    // ---------- schema stamping ----------

    fn check_or_stamp_schema(&self) -> StoreResult<()> {
        let cf = self.cf(CF_META)?;
        match self.db.get_cf(&cf, META_KEY_SCHEMA_VERSION)? {
            Some(v) => {
                let found = decode_u32(&v).unwrap_or(0);
                if found != SCHEMA_VERSION {
                    return Err(StoreError::SchemaMismatch {
                        found,
                        expected: SCHEMA_VERSION,
                    });
                }
                Ok(())
            }
            None => {
                self.db.put_cf(
                    &cf,
                    META_KEY_SCHEMA_VERSION,
                    SCHEMA_VERSION.to_le_bytes(),
                )?;
                Ok(())
            }
        }
    }

    // ---------- typed meta accessors ----------

    pub fn head_block(&self) -> StoreResult<Option<u64>> {
        self.get_meta_u64(META_KEY_HEAD_BLOCK)
    }

    pub fn backfill_low_water(&self) -> StoreResult<Option<u64>> {
        self.get_meta_u64(META_KEY_BACKFILL_LOW)
    }

    pub fn backfill_high_water(&self) -> StoreResult<Option<u64>> {
        self.get_meta_u64(META_KEY_BACKFILL_HIGH)
    }

    /// Fetch the canonical block hash we recorded for `block`, if any.
    pub fn canonical_hash(&self, block: u64) -> StoreResult<Option<[u8; 32]>> {
        let cf = self.cf(CF_META)?;
        let v = self.db.get_cf(&cf, meta_block_hash_key(block))?;
        Ok(v.and_then(|bytes| {
            if bytes.len() == 32 {
                let mut out = [0u8; 32];
                out.copy_from_slice(&bytes);
                Some(out)
            } else {
                None
            }
        }))
    }

    /// Fetch the set of addresses recorded as touched by `block`, if any.
    pub fn block_addrs(&self, block: u64) -> StoreResult<Option<Vec<[u8; 20]>>> {
        let cf = self.cf(CF_META)?;
        let v = self.db.get_cf(&cf, meta_block_addrs_key(block))?;
        match v {
            Some(bytes) => {
                let addrs: Vec<[u8; 20]> = bincode::deserialize(&bytes)?;
                Ok(Some(addrs))
            }
            None => Ok(None),
        }
    }

    // ---------- write batch construction ----------

    /// Build a write handle. The caller populates a WriteBatch through
    /// the helper methods, then commits it as an atomic fsync'd unit.
    pub fn write(&self) -> WriteHandle<'_> {
        WriteHandle {
            store: self,
            batch: WriteBatch::default(),
        }
    }

    // ---------- reader-side helpers (used by the AddressIndex reader) ----------

    pub fn db(&self) -> &Arc<DB> {
        &self.db
    }

    /// Look up a column family handle by name. Returns an `Arc<BoundColumnFamily>`
    /// because rocksdb 0.21 uses multi-threaded CF semantics; the handle is
    /// cheap to clone and implements `AsColumnFamilyRef` so it plugs directly
    /// into `put_cf` / `get_cf` / `iterator_cf_opt` / `delete_range_cf` calls.
    pub fn cf(&self, name: &'static str) -> StoreResult<Arc<BoundColumnFamily<'_>>> {
        self.db.cf_handle(name).ok_or(StoreError::MissingCf(name))
    }

    // ---------- private ----------

    fn get_meta_u64(&self, key: &[u8]) -> StoreResult<Option<u64>> {
        let cf = self.cf(CF_META)?;
        let v = self.db.get_cf(&cf, key)?;
        Ok(v.and_then(|bytes| decode_u64(&bytes)))
    }

    fn raw_meta(&self, key: &[u8]) -> StoreResult<Option<Vec<u8>>> {
        let cf = self.cf(CF_META)?;
        Ok(self.db.get_cf(&cf, key)?)
    }
}

/// Batched-write API. All writes for a single canonical block should
/// land in one `WriteHandle` and commit as one fsync'd batch; this
/// guarantees either "the whole block is indexed" or "nothing from
/// this block is indexed", matching the reorg handler's expectations.
pub struct WriteHandle<'a> {
    store: &'a Store,
    batch: WriteBatch,
}

impl<'a> WriteHandle<'a> {
    /// Record a per-address transaction entry.
    pub fn put_tx(
        &mut self,
        addr: &[u8; 20],
        block: u64,
        idx: u32,
        payload: &schema::TxRef,
    ) -> StoreResult<()> {
        let cf = self.store.cf(CF_ADDR_TX)?;
        let key = schema::encode_key(addr, block, idx);
        let value = bincode::serialize(payload)?;
        self.batch.put_cf(&cf, key, value);
        Ok(())
    }

    /// Record a per-address log entry.
    pub fn put_log(
        &mut self,
        addr: &[u8; 20],
        block: u64,
        idx: u32,
        payload: &schema::LogRef,
    ) -> StoreResult<()> {
        let cf = self.store.cf(CF_ADDR_LOG)?;
        let key = schema::encode_key(addr, block, idx);
        let value = bincode::serialize(payload)?;
        self.batch.put_cf(&cf, key, value);
        Ok(())
    }

    /// Record a per-address internal-txn entry (Phase 2).
    pub fn put_internal(
        &mut self,
        addr: &[u8; 20],
        block: u64,
        idx: u32,
        payload: &schema::InternalRef,
    ) -> StoreResult<()> {
        let cf = self.store.cf(CF_ADDR_INTERNAL)?;
        let key = schema::encode_key(addr, block, idx);
        let value = bincode::serialize(payload)?;
        self.batch.put_cf(&cf, key, value);
        Ok(())
    }

    /// Record the set of addresses touched by a block. Enables O(N)
    /// reorg cleanup - the reorg handler enumerates this set and
    /// deletes the corresponding (addr, block, *) prefix from each
    /// data CF instead of scanning every address in the database.
    pub fn put_block_addrs(&mut self, block: u64, addrs: &[[u8; 20]]) -> StoreResult<()> {
        let cf = self.store.cf(CF_META)?;
        let key = meta_block_addrs_key(block);
        let value = bincode::serialize(addrs)?;
        self.batch.put_cf(&cf, key, value);
        Ok(())
    }

    /// Record the canonical block hash for reorg detection.
    pub fn put_block_hash(&mut self, block: u64, hash: &[u8; 32]) -> StoreResult<()> {
        let cf = self.store.cf(CF_META)?;
        self.batch.put_cf(&cf, meta_block_hash_key(block), hash);
        Ok(())
    }

    /// Delete the per-block address set (called during reorg unwind
    /// and during hash-retention pruning).
    pub fn delete_block_addrs(&mut self, block: u64) -> StoreResult<()> {
        let cf = self.store.cf(CF_META)?;
        self.batch.delete_cf(&cf, meta_block_addrs_key(block));
        Ok(())
    }

    /// Delete the per-block canonical-hash entry.
    pub fn delete_block_hash(&mut self, block: u64) -> StoreResult<()> {
        let cf = self.store.cf(CF_META)?;
        self.batch.delete_cf(&cf, meta_block_hash_key(block));
        Ok(())
    }

    /// Delete every entry in the given CF for `(addr, block, *)`.
    /// Uses `delete_range_cf` to avoid a full scan; the range is
    /// bounded on both ends by fixed-length composite keys.
    pub fn delete_addr_block(
        &mut self,
        cf_name: &'static str,
        addr: &[u8; 20],
        block: u64,
    ) -> StoreResult<()> {
        let cf = self.store.cf(cf_name)?;
        // Lower key inclusive: (addr, block, 0)
        let from = schema::encode_key(addr, block, 0);
        // Upper key exclusive: (addr, block + 1, 0) - or (addr, block, u32::MAX + 1)
        // We use block + 1 to keep the same prefix-extractor shape.
        let to_block = block.saturating_add(1);
        let to = schema::encode_key(addr, to_block, 0);
        self.batch.delete_range_cf(&cf, from, to);
        Ok(())
    }

    /// Bump the persisted head block. Called at the end of a
    /// successful tip-follow tick.
    pub fn set_head_block(&mut self, block: u64) -> StoreResult<()> {
        let cf = self.store.cf(CF_META)?;
        self.batch
            .put_cf(&cf, META_KEY_HEAD_BLOCK, block.to_le_bytes());
        Ok(())
    }

    /// Set the backfill low-water mark (highest block *not yet*
    /// backfilled minus one - i.e. the next block to attempt).
    pub fn set_backfill_low_water(&mut self, block: u64) -> StoreResult<()> {
        let cf = self.store.cf(CF_META)?;
        self.batch
            .put_cf(&cf, META_KEY_BACKFILL_LOW, block.to_le_bytes());
        Ok(())
    }

    /// Set the backfill high-water mark once when the service first
    /// starts - the tip at that moment.
    pub fn set_backfill_high_water(&mut self, block: u64) -> StoreResult<()> {
        let cf = self.store.cf(CF_META)?;
        self.batch
            .put_cf(&cf, META_KEY_BACKFILL_HIGH, block.to_le_bytes());
        Ok(())
    }

    /// Commit the batch with fsync. On success the caller may treat
    /// every put/delete as durable.
    pub fn commit(self) -> StoreResult<()> {
        let mut opts = WriteOptions::default();
        opts.set_sync(true);
        self.store.db.write_opt(self.batch, &opts)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------

fn build_options() -> (Options, Vec<ColumnFamilyDescriptor>) {
    let mut db_opts = Options::default();
    db_opts.create_if_missing(true);
    db_opts.create_missing_column_families(true);
    // Modest, production-safe defaults; tuning knobs left to future work.
    db_opts.increase_parallelism(2);
    db_opts.set_max_background_jobs(4);

    let cf_descriptors: Vec<ColumnFamilyDescriptor> = ALL_CFS
        .iter()
        .map(|name| {
            let mut cf_opts = Options::default();
            cf_opts.set_compression_type(DBCompressionType::Lz4);
            // Fixed-length address prefix - matches encode_key layout.
            // Every CF except `meta` uses (addr || block || idx) keys.
            if *name != CF_META {
                cf_opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(
                    schema::ADDR_LEN,
                ));
            }
            ColumnFamilyDescriptor::new(*name, cf_opts)
        })
        .collect();

    (db_opts, cf_descriptors)
}

// ---------------------------------------------------------------------
// tiny endian helpers
// ---------------------------------------------------------------------

fn decode_u64(b: &[u8]) -> Option<u64> {
    if b.len() != 8 {
        return None;
    }
    let mut arr = [0u8; 8];
    arr.copy_from_slice(b);
    Some(u64::from_le_bytes(arr))
}

fn decode_u32(b: &[u8]) -> Option<u32> {
    if b.len() != 4 {
        return None;
    }
    let mut arr = [0u8; 4];
    arr.copy_from_slice(b);
    Some(u32::from_le_bytes(arr))
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{format_address, TxRef, TxRole};

    fn tmpdir() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("tempdir")
    }

    #[test]
    fn open_creates_all_cfs_and_stamps_schema() {
        let dir = tmpdir();
        let store = Store::open_rw(dir.path()).unwrap();
        // Schema version must be readable back.
        let cf = store.cf(CF_META).unwrap();
        let raw = store.db.get_cf(&cf, META_KEY_SCHEMA_VERSION).unwrap();
        assert!(raw.is_some());
        assert_eq!(decode_u32(&raw.unwrap()).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn open_ro_after_rw_shares_data() {
        let dir = tmpdir();
        {
            let store = Store::open_rw(dir.path()).unwrap();
            let mut w = store.write();
            w.set_head_block(1234).unwrap();
            w.commit().unwrap();
        }
        // Reopen as RO - this is what dc-explorer does.
        let ro = Store::open_ro(dir.path()).unwrap();
        assert_eq!(ro.head_block().unwrap(), Some(1234));
    }

    #[test]
    fn write_and_delete_range_removes_all_addr_block_entries() {
        let dir = tmpdir();
        let store = Store::open_rw(dir.path()).unwrap();
        let addr = [9u8; 20];
        let payload = TxRef {
            tx_hash: [1u8; 32],
            block_hash: [2u8; 32],
            block_number: 100,
            tx_index: 0,
            block_timestamp: 0,
            from: addr,
            to: None,
            value_wei: 0,
            gas_used: 0,
            status: 1,
            role: TxRole::From,
        };
        // Write 5 entries for the same address on block 100.
        {
            let mut w = store.write();
            for i in 0..5 {
                let mut p = payload.clone();
                p.tx_index = i;
                w.put_tx(&addr, 100, i, &p).unwrap();
            }
            w.commit().unwrap();
        }
        // And one on block 101 (must survive the delete).
        {
            let mut w = store.write();
            w.put_tx(&addr, 101, 0, &payload).unwrap();
            w.commit().unwrap();
        }
        // Delete the block-100 range.
        {
            let mut w = store.write();
            w.delete_addr_block(CF_ADDR_TX, &addr, 100).unwrap();
            w.commit().unwrap();
        }
        // Count remaining entries via a full prefix scan.
        let cf = store.cf(CF_ADDR_TX).unwrap();
        let iter = store.db.prefix_iterator_cf(&cf, addr);
        let remaining: Vec<_> = iter.collect();
        assert_eq!(
            remaining.len(),
            1,
            "block-100 rows should be gone, only block-101 survives (addr={})",
            format_address(&addr),
        );
    }

    #[test]
    fn schema_mismatch_is_detected_on_reopen() {
        let dir = tmpdir();
        {
            let store = Store::open_rw(dir.path()).unwrap();
            // Poke a bad schema version in.
            let cf = store.cf(CF_META).unwrap();
            store
                .db
                .put_cf(&cf, META_KEY_SCHEMA_VERSION, 999u32.to_le_bytes())
                .unwrap();
        }
        let res = Store::open_rw(dir.path());
        match res {
            Ok(_) => panic!("expected SchemaMismatch, got Ok"),
            Err(StoreError::SchemaMismatch { found, expected }) => {
                assert_eq!(found, 999);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            Err(other) => panic!("expected SchemaMismatch, got {other:?}"),
        }
    }
}
