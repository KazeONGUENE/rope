//! Tip follower + historical backfiller.
//!
//! * [`follow_tip`] is a long-lived loop: every `poll_interval` it
//!   fetches `eth_blockNumber` and, if the chain has advanced past
//!   the persisted head, ingests each new block one at a time. Before
//!   each ingest it verifies the block's `parentHash` against the
//!   canonical hash we stored for `block - 1`; on mismatch it drops
//!   into a controlled reorg unwind.
//! * [`backfill_range`] fills the gap between an operator-provided
//!   start block and the tip-at-service-start. It runs on a separate
//!   task with its own progress cursor stored in `META_KEY_BACKFILL_LOW`.
//!
//! Both loops share the [`Store`] and [`RpcClient`] handles.

use crate::reorg::unwind_block;
use crate::rpc::RpcClient;
use crate::schema::HASH_RETENTION_BLOCKS;
use crate::store::{Store, StoreError};
use crate::writer::{ingest_block, WriteError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum FollowError {
    #[error("rpc: {0}")]
    Rpc(#[from] crate::rpc::RpcError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("writer: {0}")]
    Writer(#[from] WriteError),
    #[error("reorg protection budget exceeded ({0} unwinds without finding fork point)")]
    ReorgBudgetExceeded(u32),
}

/// Max reorg unwind depth before we give up and require operator
/// intervention. Datachain Rope's consensus finality is measured in
/// single-digit knots; a 64-knot unwind would be catastrophically
/// unusual and should page a human immediately.
pub const MAX_REORG_DEPTH: u32 = 64;

/// Tip-follow the chain until `stop` flips to `true`. Ingests one
/// block per `poll_interval` on average when the chain is idle,
/// bursts to catch up when it's behind.
pub async fn follow_tip(
    store: Arc<Store>,
    rpc: RpcClient,
    poll_interval: Duration,
    stop: Arc<AtomicBool>,
) -> Result<(), FollowError> {
    loop {
        if stop.load(Ordering::Relaxed) {
            tracing::info!(target: "rope_addr_index::tip", "stop requested; exiting tip loop");
            return Ok(());
        }
        match tip_tick(&store, &rpc).await {
            Ok(TipReport { new_head, ingested }) => {
                if ingested > 0 {
                    tracing::info!(
                        target: "rope_addr_index::tip",
                        new_head,
                        ingested,
                        "tip advanced",
                    );
                }
            }
            Err(FollowError::Rpc(e)) => {
                tracing::warn!(target: "rope_addr_index::tip", error = %e, "rpc failure; will retry");
            }
            Err(FollowError::Writer(WriteError::BlockMissing { block })) => {
                // Node briefly returned null for a block we thought
                // was there (e.g. Reth GC pause). Back off one tick.
                tracing::warn!(target: "rope_addr_index::tip", block, "block reported missing; retrying next tick");
            }
            Err(FollowError::ReorgBudgetExceeded(n)) => {
                tracing::error!(target: "rope_addr_index::tip", depth = n, "reorg budget exceeded; halting tip follow");
                return Err(FollowError::ReorgBudgetExceeded(n));
            }
            Err(other) => {
                tracing::error!(target: "rope_addr_index::tip", error = %other, "unrecoverable tip error");
                return Err(other);
            }
        }
        tokio::time::sleep(poll_interval).await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TipReport {
    new_head: u64,
    ingested: u64,
}

async fn tip_tick(store: &Store, rpc: &RpcClient) -> Result<TipReport, FollowError> {
    let tip = rpc.eth_block_number().await?;
    let head = store.head_block()?.unwrap_or(0);
    if tip <= head {
        return Ok(TipReport {
            new_head: head,
            ingested: 0,
        });
    }
    // Ingest one block at a time. This keeps the reorg guard rails
    // tight and bounds the size of any single write batch.
    let mut cursor = head + 1;
    let mut ingested = 0u64;
    while cursor <= tip {
        // Reorg check for cursor - 1: does the block at (cursor)'s
        // `parentHash` match the canonical hash we stored?
        // We already have block(cursor) fetched inside ingest_block,
        // but running the parent-hash check separately lets us bail
        // BEFORE writing anything. Trade one extra header fetch per
        // block for reorg safety - cheap.
        if cursor > 1 {
            if let Some(expected_parent) = store.canonical_hash(cursor - 1)? {
                let hdr = rpc.eth_get_block_by_number_full(cursor).await?;
                if let Some(hdr_val) = hdr {
                    let parent_hash_field = hdr_val
                        .get("parentHash")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    if let Ok(actual_parent) = crate::rpc::parse_hex_h256(&parent_hash_field) {
                        if actual_parent != expected_parent {
                            // Reorg. Walk backwards, unwinding, until the
                            // stored canonical hash matches the fetched
                            // block's ancestry.
                            handle_reorg(store, rpc, cursor - 1).await?;
                            // Restart tick - head has moved backwards.
                            let new_head = store.head_block()?.unwrap_or(0);
                            return Ok(TipReport {
                                new_head,
                                ingested,
                            });
                        }
                    }
                }
            }
        }
        // Ingest the block. This writes atomically and records the
        // canonical hash + touched-addrs set inside the same batch.
        let _report = ingest_block(store, rpc, cursor).await?;
        // Advance persisted head.
        {
            let mut w = store.write();
            w.set_head_block(cursor)?;
            w.commit()?;
        }
        // Prune hash retention (best-effort; failure is non-fatal).
        prune_old_hashes(store, cursor);
        ingested += 1;
        cursor += 1;
    }
    Ok(TipReport {
        new_head: cursor - 1,
        ingested,
    })
}

/// Walk backwards from `start_block` unwinding orphaned blocks until
/// a stored canonical hash matches the RPC ancestry. Bounded by
/// [`MAX_REORG_DEPTH`] to prevent runaway loops on a broken node.
async fn handle_reorg(
    store: &Store,
    rpc: &RpcClient,
    start_block: u64,
) -> Result<u64, FollowError> {
    tracing::warn!(target: "rope_addr_index::tip", start_block, "reorg detected; unwinding");
    let mut cur = start_block;
    for depth in 0..MAX_REORG_DEPTH {
        let _ = depth;
        unwind_block(store, cur)?;
        if cur == 0 {
            break;
        }
        // Peek the new canonical block at `cur` via RPC - its
        // parentHash tells us if we've now caught up to the fork
        // point (parentHash == our stored hash at cur - 1).
        match rpc.eth_get_block_by_number_full(cur).await? {
            Some(val) => {
                let parent_field = val
                    .get("parentHash")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                if let Ok(parent) = crate::rpc::parse_hex_h256(&parent_field) {
                    if let Some(stored_parent) = store.canonical_hash(cur - 1)? {
                        if parent == stored_parent {
                            // Fork point found - persist cur-1 as new head
                            // and let the tip loop re-ingest cur+.
                            let mut w = store.write();
                            w.set_head_block(cur - 1)?;
                            w.commit()?;
                            tracing::info!(
                                target: "rope_addr_index::tip",
                                fork_point = cur - 1,
                                "reorg fork point located",
                            );
                            return Ok(cur - 1);
                        }
                    }
                }
            }
            None => {
                // Block still missing on the node; move one earlier.
            }
        }
        cur = cur.saturating_sub(1);
    }
    Err(FollowError::ReorgBudgetExceeded(MAX_REORG_DEPTH))
}

/// Delete canonical-hash entries older than `HASH_RETENTION_BLOCKS`
/// behind the current cursor. Non-fatal: any failure is logged and
/// swallowed because losing a hash entry only means we lose reorg
/// coverage for that ancient block (which we've decided is fine).
fn prune_old_hashes(store: &Store, cursor: u64) {
    if cursor <= HASH_RETENTION_BLOCKS {
        return;
    }
    let cutoff = cursor - HASH_RETENTION_BLOCKS;
    let mut w = store.write();
    if let Err(e) = w.delete_block_hash(cutoff) {
        tracing::debug!(target: "rope_addr_index::tip", error = %e, cutoff, "prune hash delete failed");
        return;
    }
    if let Err(e) = w.commit() {
        tracing::debug!(target: "rope_addr_index::tip", error = %e, cutoff, "prune hash commit failed");
    }
}

/// Backfill the historical range `[floor, ceiling]` inclusive,
/// newest-first. Progress is persisted in `META_KEY_BACKFILL_LOW`
/// after every successful block so an operator restart is cheap.
pub async fn backfill_range(
    store: Arc<Store>,
    rpc: RpcClient,
    floor: u64,
    ceiling: u64,
    stop: Arc<AtomicBool>,
) -> Result<(), FollowError> {
    if floor > ceiling {
        return Ok(());
    }
    // Set the high-water mark once (idempotent - no-op on repeat).
    {
        let mut w = store.write();
        if store.backfill_high_water()?.is_none() {
            w.set_backfill_high_water(ceiling)?;
        }
        w.commit()?;
    }

    // Resume from the persisted low-water if it's inside our range.
    let mut cursor = match store.backfill_low_water()? {
        Some(saved) if saved > floor && saved <= ceiling => saved,
        _ => ceiling,
    };

    while cursor >= floor {
        if stop.load(Ordering::Relaxed) {
            tracing::info!(target: "rope_addr_index::backfill", "stop requested; exiting backfill");
            return Ok(());
        }
        // Skip if we've already ingested this block (idempotent).
        if store.block_addrs(cursor)?.is_some() {
            if cursor == 0 {
                break;
            }
            cursor -= 1;
            continue;
        }
        match ingest_block(&store, &rpc, cursor).await {
            Ok(_) => {
                let mut w = store.write();
                w.set_backfill_low_water(cursor)?;
                w.commit()?;
                if cursor % 1000 == 0 {
                    tracing::info!(
                        target: "rope_addr_index::backfill",
                        cursor,
                        floor,
                        "backfill progress",
                    );
                }
            }
            Err(WriteError::BlockMissing { block }) => {
                tracing::warn!(target: "rope_addr_index::backfill", block, "block reported missing during backfill; skipping");
            }
            Err(other) => {
                tracing::warn!(target: "rope_addr_index::backfill", error = %other, cursor, "backfill error; will retry after backoff");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        }
        if cursor == 0 {
            break;
        }
        cursor -= 1;
    }
    tracing::info!(target: "rope_addr_index::backfill", floor, ceiling, "backfill complete");
    Ok(())
}
