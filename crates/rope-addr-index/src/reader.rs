//! Read-side API. What DCScan (`rope-explorer`) calls to answer
//! `/api/v2/addresses/:addr/transactions?cursor=...` and its cousins.
//!
//! The reader opens the same on-disk RocksDB store the writer created,
//! but read-only. Multiple `AddressIndex` handles may co-exist against
//! one live writer - RocksDB supports concurrent read-only opens
//! without any coordination on our side.
//!
//! # Iteration model
//!
//! For a given address, walk the CF backwards from
//! [`crate::schema::upper_bound`] (the seek-past-any-key sentinel), stop
//! when the current key's 20-byte prefix no longer matches the target
//! address. That gives newest-first order in O(page_size).
//!
//! Cursors are opaque: `base64(bincode(Cursor { block, idx }))`. The
//! reader clamps a client-supplied cursor to the address's live upper
//! bound so a stale cursor from before a reorg unwind still resolves to
//! valid results (it just skips over the deleted range).

use crate::schema::{
    decode_key, encode_key, format_address, normalise_address, upper_bound, Cursor, LogRef,
    TxRef, ADDR_LEN, CF_ADDR_LOG, CF_ADDR_TX,
};
use crate::store::{Store, StoreError};
use rocksdb::{Direction, IteratorMode, ReadOptions};
use std::sync::Arc;
use thiserror::Error;

/// Largest page an API caller may request in one hop. Prevents a
/// pathological `limit=1_000_000` from occupying the reader thread for
/// seconds and starving the tokio runtime.
pub const MAX_PAGE_SIZE: usize = 500;

/// Default page size when the caller omits `limit`.
pub const DEFAULT_PAGE_SIZE: usize = 25;

#[derive(Debug, Error)]
pub enum ReadError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("invalid address: must be 20-byte 0x-prefixed hex")]
    BadAddress,
    #[error("invalid cursor: not decodable")]
    BadCursor,
    #[error("bincode: {0}")]
    Bincode(#[from] Box<bincode::ErrorKind>),
}

pub type ReadResult<T> = Result<T, ReadError>;

/// Handle to the read-only address index. Cheap to clone (an `Arc`).
#[derive(Clone)]
pub struct AddressIndex {
    store: Arc<Store>,
}

impl AddressIndex {
    /// Wrap a pre-opened read-only store. Callers construct the store
    /// via `Store::open_ro(path)` and share it across the axum handlers.
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// The tip the writer has fsync'd. Handlers can compare against
    /// `eth_blockNumber` to decide whether to serve indexed results or
    /// fall back to legacy RPC scans (e.g. writer is still warming).
    pub fn head_block(&self) -> ReadResult<Option<u64>> {
        Ok(self.store.head_block()?)
    }

    /// Coarse status object for `/api/v2/index/status`.
    pub fn status(&self) -> ReadResult<IndexStatus> {
        Ok(IndexStatus {
            head_block: self.store.head_block()?,
            backfill_low_water: self.store.backfill_low_water()?,
            backfill_high_water: self.store.backfill_high_water()?,
        })
    }

    /// Page of transactions where `addr` was `from`, `to`, or `both`.
    /// Newest first. Optional `cursor` continues from a previous page.
    pub fn transactions(
        &self,
        addr: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> ReadResult<Page<TxRef>> {
        let a = normalise_address(addr).ok_or(ReadError::BadAddress)?;
        self.paged_scan::<TxRef>(CF_ADDR_TX, &a, limit, cursor)
    }

    /// Page of event logs relevant to `addr`. Newest first.
    pub fn logs(
        &self,
        addr: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> ReadResult<Page<LogRef>> {
        let a = normalise_address(addr).ok_or(ReadError::BadAddress)?;
        self.paged_scan::<LogRef>(CF_ADDR_LOG, &a, limit, cursor)
    }

    /// Total distinct blocks in which `addr` participated as a tx
    /// party. Bounded scan; use `count_transactions_bounded` for the
    /// exact call shape.
    pub fn count_transactions_bounded(
        &self,
        addr: &str,
        max: usize,
    ) -> ReadResult<CountResult> {
        let a = normalise_address(addr).ok_or(ReadError::BadAddress)?;
        self.bounded_count(CF_ADDR_TX, &a, max)
    }

    // -----------------------------------------------------------------
    // internals
    // -----------------------------------------------------------------

    fn paged_scan<T>(
        &self,
        cf_name: &'static str,
        addr: &[u8; ADDR_LEN],
        limit: usize,
        cursor: Option<&str>,
    ) -> ReadResult<Page<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let cf = self.store.cf(cf_name)?;

        // Seek key: cursor-supplied or "one past every key for this addr".
        let seek_from = match cursor {
            Some(s) => {
                let c = Cursor::decode(s).ok_or(ReadError::BadCursor)?;
                encode_key(addr, c.block, c.idx)
            }
            None => upper_bound(addr),
        };

        // Reverse iterate bounded by the address prefix. `iterate_upper_bound`
        // and `iterate_lower_bound` keep RocksDB from wandering into the
        // next address's keys.
        let mut read_opts = ReadOptions::default();
        // Lower bound = first legal key for this addr.
        read_opts.set_iterate_lower_bound(encode_key(addr, 0, 0).to_vec());
        // Upper bound (exclusive) = first legal key for addr + 1.
        // Compute by incrementing the 20-byte prefix; if the addr is
        // 0xff...ff (impossible in practice) fall back to unbounded.
        if let Some(ub) = next_addr_prefix(addr) {
            let mut upper = [0u8; ADDR_LEN + 12];
            upper[..ADDR_LEN].copy_from_slice(&ub);
            read_opts.set_iterate_upper_bound(upper.to_vec());
        }
        // Do not fill the block cache with historical scans that will
        // never be read back (a real Etherscan-style hot address gets
        // hit constantly and we don't want deep scrolls to evict warm
        // tip data).
        read_opts.fill_cache(false);

        let mode = IteratorMode::From(&seek_from, Direction::Reverse);
        let iter = self.store.db().iterator_cf_opt(&cf, read_opts, mode);

        let mut items: Vec<T> = Vec::with_capacity(limit);
        let mut next_cursor: Option<Cursor> = None;
        for entry in iter {
            let (k, v) = match entry {
                Ok(kv) => kv,
                Err(e) => return Err(ReadError::Store(StoreError::Db(e))),
            };
            // Prefix check - belt AND braces even though the iterator
            // is bounded, in case RocksDB ever returns one extra key
            // outside the bound (has been known to happen with older
            // versions of the crate).
            if k.len() < ADDR_LEN || &k[..ADDR_LEN] != &addr[..] {
                break;
            }
            // Cursor exclusion: if the caller passed a cursor, that
            // exact `(block, idx)` was the last item on the previous
            // page - don't emit it again.
            if let Some(cur) = cursor {
                let c = Cursor::decode(cur).ok_or(ReadError::BadCursor)?;
                let this_key_block = u64::from_be_bytes({
                    let mut b = [0u8; 8];
                    b.copy_from_slice(&k[ADDR_LEN..ADDR_LEN + 8]);
                    b
                });
                let this_key_idx = u32::from_be_bytes({
                    let mut b = [0u8; 4];
                    b.copy_from_slice(&k[ADDR_LEN + 8..]);
                    b
                });
                if this_key_block == c.block && this_key_idx == c.idx {
                    continue;
                }
            }

            if items.len() == limit {
                // Emit next-page cursor from the first key past the
                // page's tail and stop.
                if let Some((_, block, idx)) = decode_key(&k) {
                    // The next page should start from THIS key inclusive,
                    // but because the iterator is `Direction::Reverse`
                    // and the cursor semantics are "the last item of the
                    // previous page", we encode the previous item's
                    // (block, idx) as the cursor. We already stored that
                    // when we appended the last item; look it up from
                    // the last-appended payload instead.
                    let _ = (block, idx);
                }
                break;
            }
            let payload: T = bincode::deserialize(&v)?;
            items.push(payload);
            // Track this key so we can emit a cursor pointing at it.
            if let Some((_, block, idx)) = decode_key(&k) {
                next_cursor = Some(Cursor { block, idx });
            }
        }

        // If we filled the page, `next_cursor` is the cursor for the
        // next hop. If we drained the range early, no next page exists.
        let has_more = items.len() == limit;
        let cursor_out = if has_more {
            next_cursor.as_ref().map(Cursor::encode)
        } else {
            None
        };

        Ok(Page {
            address: format_address(addr),
            items,
            next_cursor: cursor_out,
        })
    }

    fn bounded_count(
        &self,
        cf_name: &'static str,
        addr: &[u8; ADDR_LEN],
        max: usize,
    ) -> ReadResult<CountResult> {
        let cf = self.store.cf(cf_name)?;
        let mut read_opts = ReadOptions::default();
        read_opts.set_iterate_lower_bound(encode_key(addr, 0, 0).to_vec());
        if let Some(ub) = next_addr_prefix(addr) {
            let mut upper = [0u8; ADDR_LEN + 12];
            upper[..ADDR_LEN].copy_from_slice(&ub);
            read_opts.set_iterate_upper_bound(upper.to_vec());
        }
        read_opts.fill_cache(false);
        let mode = IteratorMode::From(&upper_bound(addr), Direction::Reverse);
        let iter = self.store.db().iterator_cf_opt(&cf, read_opts, mode);

        let mut count = 0usize;
        for entry in iter {
            let (k, _v) = match entry {
                Ok(kv) => kv,
                Err(e) => return Err(ReadError::Store(StoreError::Db(e))),
            };
            if k.len() < ADDR_LEN || &k[..ADDR_LEN] != &addr[..] {
                break;
            }
            count += 1;
            if count >= max {
                return Ok(CountResult {
                    count,
                    exact: false,
                });
            }
        }
        Ok(CountResult { count, exact: true })
    }
}

/// One page of results.
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub address: String,
    pub items: Vec<T>,
    /// Encoded cursor to fetch the next page, or `None` when the
    /// address has no further history.
    pub next_cursor: Option<String>,
}

/// Return of `count_transactions_bounded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountResult {
    /// Number of entries observed.
    pub count: usize,
    /// `true` if the scan drained the address's history; `false` if
    /// the caller-supplied `max` cap was reached first.
    pub exact: bool,
}

/// Status snapshot exposed at `/api/v2/index/status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStatus {
    pub head_block: Option<u64>,
    pub backfill_low_water: Option<u64>,
    pub backfill_high_water: Option<u64>,
}

/// Given a 20-byte address, return the "address + 1" 20-byte prefix
/// (`None` if the address is 0xff...ff). Used to construct exclusive
/// upper bounds for prefix-scoped RocksDB iterators.
fn next_addr_prefix(addr: &[u8; ADDR_LEN]) -> Option<[u8; ADDR_LEN]> {
    let mut out = *addr;
    for i in (0..ADDR_LEN).rev() {
        if out[i] == 0xff {
            out[i] = 0x00;
        } else {
            out[i] += 1;
            return Some(out);
        }
    }
    None
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{TxRef, TxRole};
    use std::sync::Arc;

    fn make_tx(from: [u8; 20], to: Option<[u8; 20]>, block: u64, idx: u32) -> TxRef {
        TxRef {
            tx_hash: [0xaa; 32],
            block_hash: [0xbb; 32],
            block_number: block,
            tx_index: idx,
            block_timestamp: 1_000_000 + block as i64,
            from,
            to,
            value_wei: 100 * block as u128,
            gas_used: 21000,
            status: 1,
            role: TxRole::From,
        }
    }

    /// Populate a store with `n` transactions for `addr`, one per block
    /// from block 1 to n inclusive.
    fn seed_store(dir: &tempfile::TempDir, addr: [u8; 20], n: u64) -> Arc<Store> {
        let store = Store::open_rw(dir.path()).unwrap();
        for block in 1..=n {
            let mut w = store.write();
            let tx = make_tx(addr, Some([0x99; 20]), block, 0);
            w.put_tx(&addr, block, 0, &tx).unwrap();
            w.put_block_addrs(block, &[addr, [0x99; 20]]).unwrap();
            w.commit().unwrap();
        }
        Arc::new(store)
    }

    #[test]
    fn transactions_newest_first_and_pageable() {
        let dir = tempfile::TempDir::new().unwrap();
        let addr = [0x11; 20];
        let store = seed_store(&dir, addr, 12);
        // Drop the writer BEFORE opening the RO handle: RocksDB primary
        // locks are exclusive, so we open RW → write → drop → open RO.
        drop(store);
        let ro = Arc::new(Store::open_ro(dir.path()).unwrap());
        let idx = AddressIndex::new(ro);

        // Page 1 (5 items, newest first)
        let addr_hex = format_address(&addr);
        let p1 = idx.transactions(&addr_hex, 5, None).unwrap();
        assert_eq!(p1.items.len(), 5);
        // Newest first: blocks 12, 11, 10, 9, 8
        let blocks: Vec<u64> = p1.items.iter().map(|t| t.block_number).collect();
        assert_eq!(blocks, vec![12, 11, 10, 9, 8]);
        assert!(p1.next_cursor.is_some(), "expected next page cursor");

        // Page 2 continues cleanly
        let p2 = idx
            .transactions(&addr_hex, 5, p1.next_cursor.as_deref())
            .unwrap();
        let blocks2: Vec<u64> = p2.items.iter().map(|t| t.block_number).collect();
        assert_eq!(blocks2, vec![7, 6, 5, 4, 3]);
        assert!(p2.next_cursor.is_some());

        // Page 3 is short (last 2 items) and closes the cursor.
        let p3 = idx
            .transactions(&addr_hex, 5, p2.next_cursor.as_deref())
            .unwrap();
        let blocks3: Vec<u64> = p3.items.iter().map(|t| t.block_number).collect();
        assert_eq!(blocks3, vec![2, 1]);
        assert!(p3.next_cursor.is_none());
    }

    #[test]
    fn transactions_ignores_other_addresses() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open_rw(dir.path()).unwrap();
        let addr_a = [0x11; 20];
        let addr_b = [0x12; 20];
        {
            let mut w = store.write();
            let tx_a = make_tx(addr_a, None, 100, 0);
            w.put_tx(&addr_a, 100, 0, &tx_a).unwrap();
            let mut tx_b = make_tx(addr_b, None, 100, 0);
            tx_b.from = addr_b;
            w.put_tx(&addr_b, 100, 0, &tx_b).unwrap();
            w.commit().unwrap();
        }
        drop(store);

        let ro = Arc::new(Store::open_ro(dir.path()).unwrap());
        let idx = AddressIndex::new(ro);

        let p = idx.transactions(&format_address(&addr_a), 50, None).unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].from, addr_a);
    }

    #[test]
    fn bounded_count_reports_exact_or_capped() {
        let dir = tempfile::TempDir::new().unwrap();
        let addr = [0x22; 20];
        let store = seed_store(&dir, addr, 10);
        drop(store);
        let ro = Arc::new(Store::open_ro(dir.path()).unwrap());
        let idx = AddressIndex::new(ro);

        // Under cap → exact.
        let c1 = idx
            .count_transactions_bounded(&format_address(&addr), 100)
            .unwrap();
        assert_eq!(c1.count, 10);
        assert!(c1.exact);

        // Cap reached → not exact.
        let c2 = idx
            .count_transactions_bounded(&format_address(&addr), 3)
            .unwrap();
        assert_eq!(c2.count, 3);
        assert!(!c2.exact);
    }

    #[test]
    fn head_block_and_status_reflect_writer() {
        let dir = tempfile::TempDir::new().unwrap();
        {
            let store = Store::open_rw(dir.path()).unwrap();
            let mut w = store.write();
            w.set_head_block(2024).unwrap();
            w.set_backfill_low_water(500).unwrap();
            w.set_backfill_high_water(2024).unwrap();
            w.commit().unwrap();
        }
        let ro = Arc::new(Store::open_ro(dir.path()).unwrap());
        let idx = AddressIndex::new(ro);
        let st = idx.status().unwrap();
        assert_eq!(st.head_block, Some(2024));
        assert_eq!(st.backfill_low_water, Some(500));
        assert_eq!(st.backfill_high_water, Some(2024));
    }

    #[test]
    fn next_addr_prefix_wraps() {
        assert_eq!(next_addr_prefix(&[0u8; 20]).unwrap()[19], 1);
        let mut all_ff = [0u8; 20];
        for b in &mut all_ff {
            *b = 0xff;
        }
        assert!(next_addr_prefix(&all_ff).is_none());
    }
}
