//! Block ingestor: turns one canonical block (full-tx form) + its
//! logs into a set of per-address entries and writes them as a single
//! atomic RocksDB batch.
//!
//! The writer is intentionally allocation-heavy per block (a few
//! HashMaps, some Vec<u8>) rather than trying to be zero-copy - the
//! throughput target is "keep up with the 3s knot cadence and blast
//! through historical backfill", not "match memcpy speed". Simple wins.

use crate::rpc::{
    parse_hex_bytes, parse_hex_h160, parse_hex_h256, parse_hex_u128, parse_hex_u64, RpcClient,
    RpcError,
};
use crate::schema::{LogRef, LogRole, TxRef, TxRole, ADDR_LEN};
use crate::store::Store;
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("rpc: {0}")]
    Rpc(#[from] RpcError),
    #[error("store: {0}")]
    Store(#[from] crate::store::StoreError),
    #[error("block payload malformed: {0}")]
    BadBlock(String),
    #[error("skipped: block {block} not present on chain")]
    BlockMissing { block: u64 },
}

pub type WriteResult<T> = Result<T, WriteError>;

/// Result of ingesting a single block. Callers use this to advance
/// the head-block cursor and log a friendly status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockIngestReport {
    pub block: u64,
    pub block_hash: [u8; 32],
    pub parent_hash: [u8; 32],
    pub tx_count: usize,
    pub log_count: usize,
    pub distinct_addrs: usize,
}

/// Fetch block `block` in full-tx form, fetch its logs, and write
/// every per-address (tx + log) entry as a single atomic batch.
///
/// On success returns the ingest report so callers can (a) verify
/// `parent_hash` against the previous block for reorg detection and
/// (b) bump the head-block cursor.
///
/// **Reorg contract:** the writer records the per-block address set
/// and the canonical block hash inside the same batch as the data
/// entries. That means either the whole block is indexed and the meta
/// entries are present, or nothing was written - the reader will
/// never see a partial state.
pub async fn ingest_block(
    store: &Store,
    rpc: &RpcClient,
    block: u64,
) -> WriteResult<BlockIngestReport> {
    // Fetch the block. `None` = past-the-tip or reorged away.
    let raw = rpc
        .eth_get_block_by_number_full(block)
        .await?
        .ok_or(WriteError::BlockMissing { block })?;

    let block_hash = parse_hex_h256(
        raw.get("hash")
            .ok_or_else(|| WriteError::BadBlock("missing hash".into()))?,
    )?;
    let parent_hash = parse_hex_h256(
        raw.get("parentHash")
            .ok_or_else(|| WriteError::BadBlock("missing parentHash".into()))?,
    )?;
    let block_number = parse_hex_u64(
        raw.get("number")
            .ok_or_else(|| WriteError::BadBlock("missing number".into()))?,
    )?;
    if block_number != block {
        return Err(WriteError::BadBlock(format!(
            "requested block {} but response says {}",
            block, block_number,
        )));
    }
    let block_timestamp: i64 = raw
        .get("timestamp")
        .and_then(|v| parse_hex_u64(v).ok())
        .map(|u| u as i64)
        .unwrap_or(0);
    let empty_txs: Vec<serde_json::Value> = Vec::new();
    let txs = raw
        .get("transactions")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_txs);

    // Fetch logs for the whole block. `eth_getLogs` with the same
    // from/to = block is cheaper than iterating receipts one-by-one.
    let logs = rpc.eth_get_logs(block, block).await?;

    let mut batch = store.write();
    let mut touched_addrs: BTreeSet<[u8; ADDR_LEN]> = BTreeSet::new();
    let mut tx_count = 0;
    let mut log_count = 0;

    // ---- Transactions --------------------------------------------------
    // Fold each tx into (from, to) roles. Self-sends collapse to Both.
    for (i, tx) in txs.iter().enumerate() {
        let tx_hash = parse_hex_h256(
            tx.get("hash")
                .ok_or_else(|| WriteError::BadBlock(format!("tx {} missing hash", i)))?,
        )?;
        let tx_index = tx
            .get("transactionIndex")
            .and_then(|v| parse_hex_u64(v).ok())
            .unwrap_or(i as u64) as u32;
        let from = parse_hex_h160(
            tx.get("from")
                .ok_or_else(|| WriteError::BadBlock(format!("tx {} missing from", i)))?,
        )?;
        let to: Option<[u8; 20]> = match tx.get("to") {
            Some(serde_json::Value::Null) | None => None,
            Some(v) => Some(parse_hex_h160(v)?),
        };
        let value_wei = tx
            .get("value")
            .map(parse_hex_u128)
            .transpose()?
            .unwrap_or(0);
        // Receipt data would give us gas_used + status. We keep the tx
        // summary lean here (status=2 "receipt not indexed yet") and
        // let the reader hydrate on demand. Ingesting receipts for every
        // tx on every block is prohibitively expensive at cold-start
        // backfill scale; Phase 2 (see roadmap) can wire it in with a
        // separate loop that fills the gap.
        let gas_used: u64 = 0;
        let status: u8 = 2;

        let self_send = to.map(|t| t == from).unwrap_or(false);

        // Emit for from.
        {
            let role = if self_send { TxRole::Both } else { TxRole::From };
            let payload = TxRef {
                tx_hash,
                block_hash,
                block_number,
                tx_index,
                block_timestamp,
                from,
                to,
                value_wei,
                gas_used,
                status,
                role,
            };
            batch.put_tx(&from, block_number, tx_index, &payload)?;
            touched_addrs.insert(from);
            tx_count += 1;
        }
        // Emit for to (if distinct and non-null).
        if let Some(to_addr) = to {
            if !self_send {
                let payload = TxRef {
                    tx_hash,
                    block_hash,
                    block_number,
                    tx_index,
                    block_timestamp,
                    from,
                    to,
                    value_wei,
                    gas_used,
                    status,
                    role: TxRole::To,
                };
                batch.put_tx(&to_addr, block_number, tx_index, &payload)?;
                touched_addrs.insert(to_addr);
                tx_count += 1;
            }
        }
    }

    // ---- Logs ---------------------------------------------------------
    // For each log:
    //   - the emitter (log.address) always gets an entry with role=Emitter,
    //   - each indexed topic that decodes to a 20-byte address gets an entry
    //     with role=Topic1/2/3 (topic0 is the event signature; skip it).
    for log in &logs {
        let tx_hash = parse_hex_h256(
            log.get("transactionHash")
                .ok_or_else(|| WriteError::BadBlock("log missing transactionHash".into()))?,
        )?;
        let log_index = log
            .get("logIndex")
            .and_then(|v| parse_hex_u64(v).ok())
            .ok_or_else(|| WriteError::BadBlock("log missing logIndex".into()))? as u32;
        let tx_index = log
            .get("transactionIndex")
            .and_then(|v| parse_hex_u64(v).ok())
            .unwrap_or(0) as u32;
        let emitter = parse_hex_h160(
            log.get("address")
                .ok_or_else(|| WriteError::BadBlock("log missing address".into()))?,
        )?;
        let topics: Vec<[u8; 32]> = match log.get("topics").and_then(|v| v.as_array()) {
            Some(arr) => arr
                .iter()
                .map(parse_hex_h256)
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        let data = match log.get("data") {
            Some(v) => parse_hex_bytes(v)?,
            None => Vec::new(),
        };

        // Base entry for the emitter.
        let base = LogRef {
            tx_hash,
            block_hash,
            block_number,
            block_timestamp,
            tx_index,
            log_index,
            emitter,
            topics: topics.clone(),
            data: data.clone(),
            role: LogRole::Emitter,
        };
        batch.put_log(&emitter, block_number, log_index, &base)?;
        touched_addrs.insert(emitter);
        log_count += 1;

        // Address-typed topics: EVM indexed address parameters are
        // right-padded 32-byte words with the top 12 bytes zero.
        // We decode topics 1..4 as candidate addresses.
        for (i, topic) in topics.iter().enumerate().skip(1) {
            if i > 3 {
                break;
            }
            if !is_padded_address(topic) {
                continue;
            }
            let mut addr = [0u8; ADDR_LEN];
            addr.copy_from_slice(&topic[12..]);
            // De-dup: don't emit a second entry if the topic address
            // is the emitter itself.
            if addr == emitter {
                continue;
            }
            let role = match i {
                1 => LogRole::Topic1,
                2 => LogRole::Topic2,
                _ => LogRole::Topic3,
            };
            let payload = LogRef {
                tx_hash,
                block_hash,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                emitter,
                topics: topics.clone(),
                data: data.clone(),
                role,
            };
            batch.put_log(&addr, block_number, log_index, &payload)?;
            touched_addrs.insert(addr);
            log_count += 1;
        }
    }

    // ---- Meta ---------------------------------------------------------
    let addrs_vec: Vec<[u8; ADDR_LEN]> = touched_addrs.iter().copied().collect();
    batch.put_block_addrs(block_number, &addrs_vec)?;
    batch.put_block_hash(block_number, &block_hash)?;

    // Commit as a single atomic fsync'd unit.
    batch.commit()?;

    tracing::debug!(
        target: "rope_addr_index::writer",
        block = block_number,
        tx_count,
        log_count,
        distinct_addrs = addrs_vec.len(),
        "ingested block",
    );

    Ok(BlockIngestReport {
        block: block_number,
        block_hash,
        parent_hash,
        tx_count,
        log_count,
        distinct_addrs: addrs_vec.len(),
    })
}

/// Reject any 32-byte topic that isn't zero-padded on the top 12 bytes.
/// Prevents false positives when a non-address indexed topic (uint256,
/// bytes32, etc.) happens to have arbitrary bytes in the low 20.
fn is_padded_address(topic: &[u8; 32]) -> bool {
    topic[..12].iter().all(|&b| b == 0)
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_padding_check() {
        let mut good = [0u8; 32];
        good[12] = 0xab;
        assert!(is_padded_address(&good));

        let mut bad = [0u8; 32];
        bad[11] = 1; // last non-zero byte before the address window
        assert!(!is_padded_address(&bad));
    }
}
