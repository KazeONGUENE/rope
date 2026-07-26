//! Follower mode — replaces each node's own `--dev` auto-miner with a
//! faithful, continuous replication of the upstream (BLUE) chain via the
//! Engine API. This is the low-risk half of the cutover: it only ever
//! *reads* from the upstream node and *imports* into the local one. It
//! can never cause BLUE to diverge or lose data, because it never writes
//! to BLUE.

use anyhow::{Context, Result};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::engine_client::EngineClient;
use crate::payload::{build_payload_from_block, summarize};

pub struct FollowerConfig {
    pub poll_interval: Duration,
    pub max_batch: u64,
}

impl Default for FollowerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(1000),
            max_batch: 500,
        }
    }
}

/// Runs forever, importing new blocks from `upstream` into `local` as they
/// appear. Never panics on a single bad block — logs and retries so a
/// transient upstream hiccup can't take the follower service down.
pub async fn run(local: &EngineClient, upstream: &EngineClient, cfg: FollowerConfig) -> Result<()> {
    loop {
        match sync_once(local, upstream, &cfg).await {
            Ok(imported) => {
                if imported == 0 {
                    tokio::time::sleep(cfg.poll_interval).await;
                }
            }
            Err(e) => {
                error!("follower sync iteration failed: {e:#}");
                tokio::time::sleep(cfg.poll_interval).await;
            }
        }
    }
}

/// Imports up to `cfg.max_batch` blocks. Returns how many were imported.
async fn sync_once(local: &EngineClient, upstream: &EngineClient, cfg: &FollowerConfig) -> Result<u64> {
    let local_head = local.block_number().await.context("local block_number")?;
    let upstream_head = upstream.block_number().await.context("upstream block_number")?;

    if local_head >= upstream_head {
        return Ok(0);
    }

    let end = std::cmp::min(upstream_head, local_head + cfg.max_batch);
    let mut imported = 0u64;

    for n in (local_head + 1)..=end {
        let block = upstream
            .get_block_by_number(n, false)
            .await
            .with_context(|| format!("fetch upstream block {n}"))?
            .ok_or_else(|| anyhow::anyhow!("upstream missing block {n} (was there right before)"))?;

        let summary = summarize(&block)?;
        let payload = build_payload_from_block(upstream, &block)
            .await
            .with_context(|| format!("build payload for block {n}"))?;

        let status = local
            .new_payload_v2(&payload)
            .await
            .with_context(|| format!("newPayloadV2 for block {n}"))?;

        if status != "VALID" && status != "SYNCING" {
            warn!(
                "block {n} ({}) rejected by local Reth: status={status}",
                summary.hash
            );
            return Err(anyhow::anyhow!(
                "block {n} rejected with status {status} — halting import to avoid building on a bad head"
            ));
        }

        let (fc_status, _) = local
            .forkchoice_updated_v2(&summary.hash, &summary.hash, &summary.hash, None)
            .await
            .with_context(|| format!("forkchoiceUpdatedV2 for block {n}"))?;

        info!(
            "follower imported block {n} hash={} txs={} status={} fc={:?}",
            summary.hash, summary.tx_count, status, fc_status.get("status")
        );

        imported += 1;
    }

    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_follower_config_default_is_sane() {
        let cfg = FollowerConfig::default();
        assert!(cfg.poll_interval.as_millis() > 0);
        assert!(cfg.max_batch > 0);
    }
}
