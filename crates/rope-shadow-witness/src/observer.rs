//! Observation loop. Periodically polls the canonical rope-node for
//! strings and their knots and applies any new knots and tombstones to
//! the v2 shadow chain.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::chain::ShadowChain;
use crate::client::RpcClient;
use crate::config::ShadowWitnessConfig;
use crate::error::ShadowWitnessResult;
use crate::store::parse_string_id_hex;

pub struct Observer {
    client: Arc<RpcClient>,
    chain: Arc<ShadowChain>,
    config: ShadowWitnessConfig,
    known_wallets: Arc<Mutex<HashSet<String>>>,
}

impl Observer {
    pub fn new(client: Arc<RpcClient>, chain: Arc<ShadowChain>, config: ShadowWitnessConfig) -> Self {
        Self {
            client,
            chain,
            config,
            known_wallets: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Run the observation loop until cancelled.
    pub async fn run(self: Arc<Self>) {
        let interval = Duration::from_secs(self.config.poll_interval_secs);
        info!(
            interval_secs = self.config.poll_interval_secs,
            upstream = %self.config.upstream_rpc_url,
            "shadow witness: starting observation loop"
        );

        loop {
            if let Err(e) = self.run_once().await {
                error!(error = %e, "shadow witness: observation round failed");
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Single observation round. Public for testability.
    pub async fn run_once(&self) -> ShadowWitnessResult<RoundStats> {
        let mut stats = RoundStats::default();

        let wallets = self.discover_wallets().await?;
        stats.wallets_observed = wallets.len();

        for wallet in wallets {
            match self.process_wallet(&wallet).await {
                Ok(applied) => {
                    stats.knots_applied += applied;
                }
                Err(e) => {
                    warn!(wallet = %wallet, error = %e, "shadow witness: wallet round failed");
                    stats.wallets_failed += 1;
                }
            }
        }

        info!(
            wallets = stats.wallets_observed,
            knots_applied = stats.knots_applied,
            wallets_failed = stats.wallets_failed,
            "shadow witness: round complete"
        );

        Ok(stats)
    }

    async fn discover_wallets(&self) -> ShadowWitnessResult<Vec<String>> {
        let mut all = self.known_wallets.lock().await.clone();
        let listed = self
            .client
            .list_strings("wallet", 0, self.config.strings_per_round)
            .await?;
        for entry in listed {
            if let Some(addr) = entry.wallet_address {
                if !addr.is_empty() {
                    all.insert(addr);
                }
            }
        }
        let merged: Vec<String> = all.iter().cloned().collect();
        let mut known = self.known_wallets.lock().await;
        *known = all;
        Ok(merged)
    }

    async fn process_wallet(&self, wallet: &str) -> ShadowWitnessResult<usize> {
        let knots = self.client.get_string_with_knots(wallet).await?;
        if knots.is_empty() {
            return Ok(0);
        }

        let string_id = knots[0].string_id.clone();
        if string_id.is_empty() {
            return Ok(0);
        }
        let string_id_bytes = parse_string_id_hex(&string_id)?;
        let head = self.chain.store().get_head(&string_id_bytes)?;
        let head_event_id = head.as_ref().map(|h| h.latest_event_id);

        let mut applied = 0usize;
        for knot in &knots {
            let mut should_apply = true;
            if let Some(head_id) = head_event_id {
                if !knot.is_tombstone && knot.knot_index <= head_id {
                    if let Some(existing) = self
                        .chain
                        .store()
                        .get_entry(&string_id_bytes, knot.knot_index)?
                    {
                        if existing.is_tombstone == knot.is_tombstone {
                            should_apply = false;
                        }
                    }
                }
            }
            if !should_apply {
                continue;
            }
            match self.chain.apply_observed(knot) {
                Ok(true) => applied += 1,
                Ok(false) => {}
                Err(e) => {
                    debug!(
                        wallet = %wallet,
                        event_id = knot.knot_index,
                        error = %e,
                        "shadow witness: knot application skipped"
                    );
                }
            }
        }
        Ok(applied)
    }
}

#[derive(Clone, Debug, Default)]
pub struct RoundStats {
    pub wallets_observed: usize,
    pub knots_applied: usize,
    pub wallets_failed: usize,
}
