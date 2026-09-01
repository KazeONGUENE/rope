//! `rope-addr-indexer` - the long-running service that reads canonical
//! blocks + logs from a Reth node (BLUE / GREEN / DO-rpc-*) and writes
//! them into the per-address RocksDB index used by DCScan.
//!
//! # What it does
//!
//! Two tokio tasks under one graceful-shutdown supervisor:
//!
//! 1. **Tip follower** - polls `eth_blockNumber` every `tip_poll_secs`
//!    and ingests each new canonical block one at a time. Reorg-safe
//!    by construction: each ingest verifies the block's `parentHash`
//!    against the canonical hash we recorded at `block - 1` and, on
//!    mismatch, unwinds the orphaned range before continuing.
//!
//! 2. **Historical backfiller** - walks the range `[floor, tip_at_start]`
//!    newest-first, ingesting any block whose per-block address set is
//!    not yet in RocksDB. Progress is persisted in
//!    `meta[b"backfill_low_water"]` after every successful block, so an
//!    operator restart is cheap.
//!
//! Both tasks share one [`Store`](rope_addr_index::Store) handle (RW)
//! and one [`RpcClient`](rope_addr_index::RpcClient) with per-URL
//! failover. On SIGINT/SIGTERM the supervisor flips a shared stop flag,
//! waits for each task to reach its next reorg-safe boundary, and
//! exits cleanly. Every in-flight `WriteBatch` is either fsync'd or
//! discarded - RocksDB `WriteBatch::commit` is atomic, so the reader
//! never observes a partial block.
//!
//! # Configuration
//!
//! Two sources, in priority order:
//!
//! 1. **CLI flags** - `--config PATH`, `--reset-index`,
//!    `--backfill-floor N`, `--log-level=info`.
//! 2. **TOML file** - the shape below. Every field is optional; the
//!    binary supplies sane production defaults derived from the
//!    2026-08-11 handover.
//!
//! ```toml
//! # /etc/rope-addr-indexer.toml
//! data_dir      = "/var/lib/rope-addr-index"
//! rpc_urls      = [
//!   "http://127.0.0.1:8545",           # BLUE loopback (primary)
//!   "http://92.243.25.119:8545",       # GREEN
//!   "http://157.230.18.45:8545",       # DO-rpc-1
//!   "http://167.172.106.174:8545",     # DO-rpc-2
//! ]
//! rpc_timeout_secs = 10
//! tip_poll_secs    = 2
//! backfill_floor   = 0        # go all the way to genesis
//! ```
//!
//! # Rollout posture
//!
//! Ships behind a feature-flag-off default: `dc-explorer` continues to
//! answer every address query via the legacy RPC-scan path until the
//! operator sets `ADDR_INDEX_PATH=/var/lib/rope-addr-index` in
//! `/etc/dc-explorer.env` and restarts the service. This binary can
//! therefore run for hours or days on a fresh backfill without any
//! user-facing effect; enabling reads is a one-flag flip once the
//! backfiller reports `backfill_low_water = 0`.

use anyhow::{bail, Context, Result};
use clap::Parser;
use rope_addr_index::{
    rpc::RpcClient,
    store::Store,
    tip::{backfill_range, follow_tip},
};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(
    name = "rope-addr-indexer",
    about = "Persistent per-address transaction / log index for Datachain Rope",
    long_about = None,
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, value_name = "PATH", default_value = "/etc/rope-addr-indexer.toml")]
    config: PathBuf,

    /// Drop every column family and start from a blank index. Requires
    /// the operator to have `sudo systemctl stop dc-explorer` first if
    /// the reader is bound to this store - RocksDB refuses to delete
    /// files another process still has open. Off by default.
    #[arg(long)]
    reset_index: bool,

    /// Override the backfill floor from the config. Useful for a
    /// staged rollout: start with `--backfill-floor <last-100k>` to
    /// prove the pipeline before letting it walk all the way to
    /// genesis.
    #[arg(long, value_name = "BLOCK")]
    backfill_floor: Option<u64>,

    /// Override the log level. Same syntax as `RUST_LOG`.
    #[arg(long, value_name = "SPEC", default_value = "rope_addr_index=info,warn")]
    log_level: String,

    /// Print the resolved configuration + intended startup plan and
    /// exit without touching RocksDB. Useful when writing the
    /// systemd unit for the first time.
    #[arg(long)]
    dry_run: bool,
}

// ---------------------------------------------------------------------
// TOML config
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    data_dir: Option<PathBuf>,
    #[serde(default)]
    rpc_urls: Option<Vec<String>>,
    #[serde(default)]
    rpc_timeout_secs: Option<u64>,
    #[serde(default)]
    tip_poll_secs: Option<u64>,
    #[serde(default)]
    backfill_floor: Option<u64>,
}

#[derive(Debug, Clone)]
struct ResolvedConfig {
    data_dir: PathBuf,
    rpc_urls: Vec<String>,
    rpc_timeout: Duration,
    tip_poll: Duration,
    backfill_floor: u64,
}

impl ResolvedConfig {
    fn resolve(cli: &Cli, file: FileConfig) -> Result<Self> {
        let data_dir = file
            .data_dir
            .unwrap_or_else(|| PathBuf::from("/var/lib/rope-addr-index"));
        let rpc_urls = file
            .rpc_urls
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                // Production default: BLUE loopback first, then the
                // read-failover ring (matches the erpc-fleet-ha path
                // documented in the 2026-07-28 HA handover).
                vec![
                    "http://127.0.0.1:8545".into(),
                    "http://92.243.25.119:8545".into(),
                    "http://157.230.18.45:8545".into(),
                    "http://167.172.106.174:8545".into(),
                ]
            });
        let rpc_timeout = Duration::from_secs(file.rpc_timeout_secs.unwrap_or(10));
        let tip_poll = Duration::from_secs(file.tip_poll_secs.unwrap_or(2));
        let backfill_floor = cli
            .backfill_floor
            .or(file.backfill_floor)
            .unwrap_or(0);
        Ok(Self {
            data_dir,
            rpc_urls,
            rpc_timeout,
            tip_poll,
            backfill_floor,
        })
    }
}

// ---------------------------------------------------------------------
// main
// ---------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.log_level);

    let file_cfg = load_config(&cli.config)?;
    let cfg = ResolvedConfig::resolve(&cli, file_cfg)?;

    info!(
        target: "rope_addr_indexer",
        data_dir = %cfg.data_dir.display(),
        rpc_urls = ?cfg.rpc_urls,
        rpc_timeout = ?cfg.rpc_timeout,
        tip_poll = ?cfg.tip_poll,
        backfill_floor = cfg.backfill_floor,
        reset_index = cli.reset_index,
        dry_run = cli.dry_run,
        "resolved config",
    );

    if cli.dry_run {
        info!(target: "rope_addr_indexer", "dry run - exiting before RocksDB open");
        return Ok(());
    }

    // ---- reset (destructive; operator-gated) ------------------------
    if cli.reset_index {
        reset_index_dir(&cfg.data_dir)?;
    }

    // ---- open store + rpc -------------------------------------------
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| {
        format!("create data dir {}", cfg.data_dir.display())
    })?;
    let store = Arc::new(
        Store::open_rw(&cfg.data_dir).with_context(|| "open rocksdb store")?,
    );
    let rpc = RpcClient::new(cfg.rpc_urls.clone(), cfg.rpc_timeout)
        .with_context(|| "build rpc client")?;

    // Peek at the tip once so backfill knows its ceiling. If the RPC
    // is unhealthy at startup, log and continue - the tip follower
    // will retry on its own schedule; backfill_range is a no-op if
    // ceiling is 0.
    let tip_at_start = match rpc.eth_block_number().await {
        Ok(n) => {
            info!(target: "rope_addr_indexer", tip = n, "reth tip at startup");
            n
        }
        Err(e) => {
            warn!(target: "rope_addr_indexer", error = %e, "startup tip fetch failed; backfill will run on next successful tip");
            0
        }
    };

    // Sanity: warn if the chain-id doesn't match Datachain Rope. Not
    // a hard-fail - the operator may want to point at a testnet - but
    // it should be visible in the journal so a misconfig is obvious.
    if let Ok(cid) = rpc.eth_chain_id().await {
        if cid != 271_828 {
            warn!(
                target: "rope_addr_indexer",
                chain_id = cid,
                expected = 271_828,
                "connected node is not Datachain Rope; continuing anyway",
            );
        } else {
            info!(target: "rope_addr_indexer", chain_id = cid, "connected to Datachain Rope mainnet");
        }
    }

    // ---- shared stop flag + signals ---------------------------------
    let stop = Arc::new(AtomicBool::new(false));
    spawn_signal_handler(stop.clone());

    // ---- tasks -------------------------------------------------------
    let tip_handle = tokio::spawn({
        let store = store.clone();
        let rpc = rpc.clone();
        let stop = stop.clone();
        let poll = cfg.tip_poll;
        async move {
            match follow_tip(store, rpc, poll, stop).await {
                Ok(()) => info!(target: "rope_addr_indexer::tip_task", "tip follower exited cleanly"),
                Err(e) => error!(target: "rope_addr_indexer::tip_task", error = %e, "tip follower halted"),
            }
        }
    });

    let backfill_handle = tokio::spawn({
        let store = store.clone();
        let rpc = rpc.clone();
        let stop = stop.clone();
        let floor = cfg.backfill_floor;
        let ceiling = tip_at_start;
        async move {
            if ceiling == 0 {
                info!(target: "rope_addr_indexer::backfill_task", "skipping backfill: tip at startup was 0");
                return;
            }
            match backfill_range(store, rpc, floor, ceiling, stop).await {
                Ok(()) => info!(
                    target: "rope_addr_indexer::backfill_task",
                    floor,
                    ceiling,
                    "backfill task exited cleanly",
                ),
                Err(e) => error!(
                    target: "rope_addr_indexer::backfill_task",
                    error = %e,
                    "backfill task halted",
                ),
            }
        }
    });

    // ---- supervise ---------------------------------------------------
    let (tip_res, backfill_res) = tokio::join!(tip_handle, backfill_handle);
    if let Err(e) = tip_res {
        error!(target: "rope_addr_indexer", error = %e, "tip task panicked");
    }
    if let Err(e) = backfill_res {
        error!(target: "rope_addr_indexer", error = %e, "backfill task panicked");
    }

    info!(target: "rope_addr_indexer", "supervisor exit");
    Ok(())
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

fn init_tracing(spec: &str) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(spec));
    // JSON logs would be nicer for the journal but the workspace already
    // uses the pretty formatter for compatibility with existing rope-node
    // journal grep patterns.
    fmt().with_env_filter(filter).with_target(true).init();
}

fn load_config(path: &Path) -> Result<FileConfig> {
    // Empty / missing config is legal - the resolver falls back to the
    // hard-coded production defaults.
    if !path.exists() {
        info!(target: "rope_addr_indexer", "no config at {}; using defaults", path.display());
        return Ok(FileConfig {
            data_dir: None,
            rpc_urls: None,
            rpc_timeout_secs: None,
            tip_poll_secs: None,
            backfill_floor: None,
        });
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read config {}", path.display()))?;
    let cfg: FileConfig = toml::from_str(&text)
        .with_context(|| format!("parse config {}", path.display()))?;
    Ok(cfg)
}

fn reset_index_dir(dir: &Path) -> Result<()> {
    // Nuke and re-create the data directory contents. This is the
    // simplest correct reset path: RocksDB's own `drop_cf` on a live
    // open handle is racy and multi-threaded-cf feature semantics
    // differ across the version matrix, so we sidestep both by having
    // no live handle at all when the physical files disappear.
    //
    // Safety gates below refuse to delete anything that looks
    // suspiciously like a shared / system path, so a mis-typed
    // `data_dir` in `/etc/rope-addr-indexer.toml` cannot torch
    // unrelated data.
    if !dir.exists() {
        info!(target: "rope_addr_indexer", "reset: no data dir at {}, nothing to reset", dir.display());
        return Ok(());
    }
    guard_reset_path(dir)?;
    warn!(target: "rope_addr_indexer", data_dir = %dir.display(), "reset_index requested - removing directory contents");
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("read data dir {}", dir.display()))?
    {
        let entry = entry.context("iterate data dir")?;
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("remove subdir {}", path.display()))?;
        } else {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove file {}", path.display()))?;
        }
    }
    info!(target: "rope_addr_indexer", "reset complete; store will be re-created on next open");
    Ok(())
}

/// Refuse to reset paths that look like they might not be the address
/// index. Deliberately conservative: an operator who really needs to
/// reset can always `rm -rf` themselves and re-run without `--reset-index`.
fn guard_reset_path(dir: &Path) -> Result<()> {
    let canonical = dir
        .canonicalize()
        .with_context(|| format!("canonicalize {}", dir.display()))?;
    let s = canonical.to_string_lossy();
    // Reject obvious system roots.
    let forbidden_exact: &[&str] = &["/", "/var", "/var/lib", "/etc", "/opt", "/usr", "/home"];
    for f in forbidden_exact {
        if s.as_ref() == *f {
            bail!(
                "refusing to reset {}: system path is on the forbidden list",
                canonical.display()
            );
        }
    }
    // Require the last path segment to contain "index" or "rope"
    // so a mis-typed `data_dir` pointed at (say) `/var/lib/postgresql`
    // does not delete a database.
    let last = canonical
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !last.contains("index") && !last.contains("rope") {
        bail!(
            "refusing to reset {}: last path segment {:?} contains neither \"index\" nor \"rope\"",
            canonical.display(),
            last
        );
    }
    Ok(())
}

fn spawn_signal_handler(stop: Arc<AtomicBool>) {
    tokio::spawn(async move {
        // SIGINT (Ctrl-C).
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
            info!(target: "rope_addr_indexer::signals", "received SIGINT");
        };
        // SIGTERM (systemd stop).
        #[cfg(unix)]
        let sigterm = async {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut s) => {
                    let _ = s.recv().await;
                    info!(target: "rope_addr_indexer::signals", "received SIGTERM");
                }
                Err(e) => {
                    warn!(target: "rope_addr_indexer::signals", error = %e, "SIGTERM handler install failed");
                }
            }
        };
        #[cfg(not(unix))]
        let sigterm = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm => {}
        }
        stop.store(true, Ordering::Relaxed);
        info!(target: "rope_addr_indexer::signals", "stop flag set; awaiting task drain");
    });
}
