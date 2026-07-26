//! `ExecutionPayloadV2` construction and validation helpers.
//!
//! Chain genesis has Shanghai active (no Cancun/blobs — confirmed against
//! the live `genesis.json`), so V2 (post-Shanghai, pre-blob) payloads are
//! the correct wire shape. This mirrors the shape validated by dry-run
//! against BLUE's own Engine API earlier in the same investigation that
//! produced this crate.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::engine_client::EngineClient;

/// Build an `ExecutionPayloadV2` object from a full RPC block (as returned
/// by `eth_getBlockByNumber(n, false)`, i.e. transactions as hashes only)
/// plus the source node's raw transaction bytes for each hash.
pub async fn build_payload_from_block(client: &EngineClient, block: &Value) -> Result<Value> {
    let tx_hashes = block
        .get("transactions")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    let mut raw_txs = Vec::with_capacity(tx_hashes.len());
    for h in &tx_hashes {
        let hash = h
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("transaction hash not a string: {h:?}"))?;
        let raw = client
            .get_raw_transaction(hash)
            .await
            .with_context(|| format!("fetching raw tx {hash}"))?;
        raw_txs.push(Value::String(raw));
    }

    field_check(block, "parentHash")?;
    field_check(block, "stateRoot")?;
    field_check(block, "receiptsRoot")?;

    Ok(json!({
        "parentHash": block["parentHash"],
        "feeRecipient": block.get("miner").cloned().unwrap_or(json!("0x0000000000000000000000000000000000000000")),
        "stateRoot": block["stateRoot"],
        "receiptsRoot": block["receiptsRoot"],
        "logsBloom": block.get("logsBloom").cloned().unwrap_or(json!(format!("0x{}", "00".repeat(256)))),
        "prevRandao": block.get("mixHash").cloned().unwrap_or(json!(format!("0x{}", "00".repeat(32)))),
        "blockNumber": block["number"],
        "gasLimit": block["gasLimit"],
        "gasUsed": block["gasUsed"],
        "timestamp": block["timestamp"],
        "extraData": block.get("extraData").cloned().unwrap_or(json!("0x")),
        "baseFeePerGas": block.get("baseFeePerGas").cloned().unwrap_or(json!("0x0")),
        "blockHash": block["hash"],
        "transactions": raw_txs,
        "withdrawals": block.get("withdrawals").cloned().unwrap_or(json!([])),
    }))
}

fn field_check(block: &Value, field: &str) -> Result<()> {
    if block.get(field).is_none() {
        bail!("block missing required field {field}");
    }
    Ok(())
}

/// Extract the fields a caller commonly needs from a payload/block for
/// logging or forkchoice bookkeeping, without re-parsing the whole object.
pub struct BlockSummary {
    pub number: u64,
    pub hash: String,
    /// Kept for future chain-continuity assertions between consecutively
    /// imported/produced blocks; not yet read by either driver mode.
    #[allow(dead_code)]
    pub parent_hash: String,
    pub tx_count: usize,
}

/// Accepts either the `eth_getBlockByNumber` RPC shape (`number`, `hash`)
/// or the `ExecutionPayloadV2` Engine API shape (`blockNumber`,
/// `blockHash`) — the quorum protocol passes payload-shaped objects
/// around (proposer → attesters → commit), while the follower path deals
/// directly in RPC-shaped blocks, so both are legitimate inputs here.
pub fn summarize(block: &Value) -> Result<BlockSummary> {
    let number_field = if block.get("number").is_some() {
        "number"
    } else {
        "blockNumber"
    };
    let hash_field = if block.get("hash").is_some() {
        "hash"
    } else {
        "blockHash"
    };

    let number = crate::engine_client::parse_hex_u64(&block[number_field])
        .with_context(|| format!("reading {number_field}"))?;
    let hash = block[hash_field]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("block missing {hash_field}"))?
        .to_string();
    let parent_hash = block
        .get("parentHash")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tx_count = block
        .get("transactions")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    Ok(BlockSummary {
        number,
        hash,
        parent_hash,
        tx_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_block(number_hex: &str, tx_hashes: Vec<&str>) -> Value {
        json!({
            "number": number_hex,
            "hash": "0xabc",
            "parentHash": "0xdef",
            "stateRoot": "0x1",
            "receiptsRoot": "0x2",
            "transactionsRoot": "0x3",
            "miner": "0x0000000000000000000000000000000000000000",
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "mixHash": format!("0x{}", "00".repeat(32)),
            "gasLimit": "0x1c9c380",
            "gasUsed": "0x0",
            "timestamp": "0x1",
            "extraData": "0x",
            "baseFeePerGas": "0x3b9aca00",
            "transactions": tx_hashes,
            "withdrawals": [],
        })
    }

    #[test]
    fn test_summarize_extracts_fields() {
        let block = sample_block("0x64", vec!["0x1", "0x2"]);
        let s = summarize(&block).unwrap();
        assert_eq!(s.number, 100);
        assert_eq!(s.hash, "0xabc");
        assert_eq!(s.parent_hash, "0xdef");
        assert_eq!(s.tx_count, 2);
    }

    #[test]
    fn test_summarize_empty_block() {
        let block = sample_block("0x0", vec![]);
        let s = summarize(&block).unwrap();
        assert_eq!(s.number, 0);
        assert_eq!(s.tx_count, 0);
    }

    #[test]
    fn test_summarize_missing_hash_errors() {
        let mut block = sample_block("0x1", vec![]);
        block.as_object_mut().unwrap().remove("hash");
        assert!(summarize(&block).is_err());
    }

    #[test]
    fn test_field_check_missing() {
        let block = json!({"number": "0x1"});
        assert!(field_check(&block, "parentHash").is_err());
    }

    #[test]
    fn test_field_check_present() {
        let block = json!({"parentHash": "0xabc"});
        assert!(field_check(&block, "parentHash").is_ok());
    }
}
