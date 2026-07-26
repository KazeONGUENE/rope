//! Production mode — the fixed-interval block-production driver for the
//! primary node (BLUE).
//!
//! This is deliberately the simplest correct thing that removes `--dev`:
//! on a fixed tick (matching the existing `block_time_ms` cadence the
//! rope-node consensus layer already uses — see
//! `quipu-canon-v2-roadmap-5m-tps.mdc` for why real Testimony-quorum-timed
//! production is a separate, later phase), ask Reth to build a payload
//! from whatever is in its own transaction pool, commit it, and advance
//! the head. Functionally equivalent to what `--dev --dev.block-time`
//! already did — the difference is this is now an external, auditable,
//! restartable, independently-deployable process using the same
//! Engine API surface a real consensus client would use, which is the
//! prerequisite for followers being able to correctly mirror it instead
//! of inventing their own blocks.

use anyhow::{Context, Result};
use serde_json::json;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::engine_client::EngineClient;
use crate::payload::summarize;

pub struct ProductionConfig {
    pub tick_interval: Duration,
    pub fee_recipient: String,
}

/// Runs forever, producing one block per tick. A failed tick is logged and
/// retried on the next tick — it never advances the head on a partial or
/// invalid payload, so a transient failure degrades to "no new block this
/// tick" rather than any risk of corrupting the chain.
pub async fn run(local: &EngineClient, cfg: ProductionConfig) -> Result<()> {
    let mut ticker = tokio::time::interval(cfg.tick_interval);
    loop {
        ticker.tick().await;
        if let Err(e) = produce_one(local, &cfg).await {
            error!("production tick failed (no block produced this tick): {e:#}");
        }
    }
}

async fn produce_one(local: &EngineClient, cfg: &ProductionConfig) -> Result<()> {
    let head_num = local.block_number().await.context("block_number")?;
    let head_block = local
        .get_block_by_number(head_num, false)
        .await
        .context("get current head")?
        .ok_or_else(|| anyhow::anyhow!("head block {head_num} vanished"))?;
    let head_summary = summarize(&head_block)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Timestamp must strictly increase; if our tick races ahead of wall
    // clock relative to the previous block, nudge forward by 1s instead of
    // producing an invalid (non-monotonic) payload.
    let head_ts = crate::engine_client::parse_hex_u64(&head_block["timestamp"]).unwrap_or(0);
    let next_ts = std::cmp::max(now, head_ts + 1);

    let attrs = json!({
        "timestamp": format!("0x{:x}", next_ts),
        "prevRandao": format!("0x{}", "00".repeat(32)),
        "suggestedFeeRecipient": cfg.fee_recipient,
        "withdrawals": [],
    });

    let (fc_status, payload_id) = local
        .forkchoice_updated_v2(
            &head_summary.hash,
            &head_summary.hash,
            &head_summary.hash,
            Some(attrs),
        )
        .await
        .context("forkchoiceUpdatedV2 (build request)")?;

    let payload_id = payload_id.ok_or_else(|| {
        anyhow::anyhow!(
            "no payloadId returned; forkchoice status was {:?}",
            fc_status
        )
    })?;

    // Building a payload from the tx pool takes Reth a moment; a short
    // fixed delay before fetching is the standard pattern real consensus
    // clients use for local block-time-scale builds (sub-second on a
    // low-tx chain, well under our multi-second tick interval).
    tokio::time::sleep(Duration::from_millis(300)).await;

    let payload = local
        .get_payload_v2(&payload_id)
        .await
        .context("getPayloadV2")?;
    let execution_payload = payload
        .get("executionPayload")
        .cloned()
        .unwrap_or(payload.clone());

    let new_summary = summarize(&execution_payload)?;

    let status = local
        .new_payload_v2(&execution_payload)
        .await
        .context("newPayloadV2")?;

    if status != "VALID" {
        warn!(
            "produced block {} status={status} — not advancing head",
            new_summary.number
        );
        return Err(anyhow::anyhow!("newPayloadV2 status {status}, refusing to advance head"));
    }

    let (final_status, _) = local
        .forkchoice_updated_v2(&new_summary.hash, &new_summary.hash, &new_summary.hash, None)
        .await
        .context("forkchoiceUpdatedV2 (finalize)")?;

    info!(
        "produced block {} hash={} txs={} status={} fc={:?}",
        new_summary.number,
        new_summary.hash,
        new_summary.tx_count,
        status,
        final_status.get("status")
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_production_config_construct() {
        let cfg = ProductionConfig {
            tick_interval: Duration::from_millis(4200),
            fee_recipient: "0x0000000000000000000000000000000000000000".to_string(),
        };
        assert_eq!(cfg.tick_interval.as_millis(), 4200);
    }
}
