//! Reorg unwinder. Uses the per-block address set + per-block
//! canonical hash we write in the same batch as the data entries to
//! delete every trace of an orphaned block from the three data CFs.
//!
//! Datachain Rope's target reorg depth is single-digit knots. We
//! retain the last 128 canonical hashes so we can detect a reorg by
//! comparing an incoming block's `parentHash` against what we stored
//! at `block - 1`. On mismatch the tip follower calls
//! [`unwind_block`] repeatedly until the fork point is found.

use crate::schema::{CF_ADDR_INTERNAL, CF_ADDR_LOG, CF_ADDR_TX};
use crate::store::{Store, StoreError};

/// Unwind everything that was written under canonical block `block`.
/// Idempotent: re-running on a block that has already been unwound
/// (or never existed) is a no-op.
pub fn unwind_block(store: &Store, block: u64) -> Result<UnwindReport, StoreError> {
    let addrs = match store.block_addrs(block)? {
        Some(a) => a,
        None => {
            return Ok(UnwindReport {
                block,
                deleted_addrs: 0,
                had_hash: false,
            });
        }
    };

    let mut batch = store.write();
    for addr in &addrs {
        batch.delete_addr_block(CF_ADDR_TX, addr, block)?;
        batch.delete_addr_block(CF_ADDR_LOG, addr, block)?;
        batch.delete_addr_block(CF_ADDR_INTERNAL, addr, block)?;
    }
    batch.delete_block_addrs(block)?;
    let had_hash = store.canonical_hash(block)?.is_some();
    if had_hash {
        batch.delete_block_hash(block)?;
    }
    batch.commit()?;

    tracing::warn!(
        target: "rope_addr_index::reorg",
        block,
        deleted_addrs = addrs.len(),
        "unwound canonical block",
    );

    Ok(UnwindReport {
        block,
        deleted_addrs: addrs.len(),
        had_hash,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnwindReport {
    pub block: u64,
    pub deleted_addrs: usize,
    pub had_hash: bool,
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{TxRef, TxRole};

    #[test]
    fn unwind_removes_all_traces_of_a_block() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open_rw(dir.path()).unwrap();
        let addr_a = [1u8; 20];
        let addr_b = [2u8; 20];

        // Simulate the writer: put txs for two addrs on block 100 + meta.
        {
            let mut w = store.write();
            let payload = TxRef {
                tx_hash: [9u8; 32],
                block_hash: [8u8; 32],
                block_number: 100,
                tx_index: 0,
                block_timestamp: 0,
                from: addr_a,
                to: Some(addr_b),
                value_wei: 42,
                gas_used: 21000,
                status: 1,
                role: TxRole::From,
            };
            w.put_tx(&addr_a, 100, 0, &payload).unwrap();
            let mut payload_b = payload.clone();
            payload_b.role = TxRole::To;
            w.put_tx(&addr_b, 100, 0, &payload_b).unwrap();
            w.put_block_addrs(100, &[addr_a, addr_b]).unwrap();
            w.put_block_hash(100, &[8u8; 32]).unwrap();
            w.commit().unwrap();
        }

        // Also put an entry on block 101 that must survive.
        {
            let mut w = store.write();
            let payload = TxRef {
                tx_hash: [7u8; 32],
                block_hash: [6u8; 32],
                block_number: 101,
                tx_index: 0,
                block_timestamp: 0,
                from: addr_a,
                to: None,
                value_wei: 1,
                gas_used: 21000,
                status: 1,
                role: TxRole::From,
            };
            w.put_tx(&addr_a, 101, 0, &payload).unwrap();
            w.put_block_addrs(101, &[addr_a]).unwrap();
            w.put_block_hash(101, &[6u8; 32]).unwrap();
            w.commit().unwrap();
        }

        let report = unwind_block(&store, 100).unwrap();
        assert_eq!(report.block, 100);
        assert_eq!(report.deleted_addrs, 2);
        assert!(report.had_hash);
        assert!(store.block_addrs(100).unwrap().is_none());
        assert!(store.canonical_hash(100).unwrap().is_none());

        // Block 101 entries should be intact.
        assert!(store.block_addrs(101).unwrap().is_some());
        assert_eq!(store.canonical_hash(101).unwrap(), Some([6u8; 32]));

        // Re-unwind must be a no-op.
        let report2 = unwind_block(&store, 100).unwrap();
        assert_eq!(report2.deleted_addrs, 0);
        assert!(!report2.had_hash);
    }
}
