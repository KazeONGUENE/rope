//! `rope-loadgen` — Quipu Canon v2.0 throughput / latency / recovery
//! harness for the in-process `LedgerStore` and (in a future patch)
//! `LedgerManager`.
//!
//! Spec reference: `docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §10.1.
//!
//! ## Subcommands
//!
//! - `store-write`   — drive synthetic appends against `LedgerStore`,
//!   measure throughput and latency percentiles.
//! - `store-recover` — open an existing RocksDB-backed store and time
//!   the cold-recovery snapshot rebuild.
//! - `store-mixed`   — interleave put_descriptor / append /
//!   mark_deleted / get_chain to model real-world load.
//!
//! Output goes to stdout as a single JSON object (machine-parseable
//! for CI), with a human-readable summary on stderr.
//!
//! ## What each subcommand exercises
//!
//! Phase 1 piece           | store-write | store-recover | store-mixed |
//! ----------------------- | :---------: | :-----------: | :---------: |
//! P1.1 sharded lattice    |             |               |             |
//!  (indirect via LedgerManager wiring — covered in a follow-up bench) |
//! P1.2 head-string lock   |     X       |               |     X       |
//! P1.3 per-shard HLC      |     X       |               |     X       |
//! P1.4 OES key cache      |             |               |             |
//!  (LedgerManager-only — covered in a follow-up bench)                |
//! P1.5 RocksDB persistence|     X       |       X       |     X       |
//!
//! Combine `--scenario {same|partitioned|random}` with `--mode
//! {memory|rocksdb}` to compare contention and durability cost.

mod cli;
mod report;
mod runner;
mod scenarios;

use clap::Parser;
use cli::{Cli, Command};

fn main() -> std::process::ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,rope_loadgen=info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let cli = Cli::parse();

    let result = match cli.command {
        Command::StoreWrite(args) => scenarios::store_write::run(args),
        Command::StoreRecover(args) => scenarios::store_recover::run(args),
        Command::StoreMixed(args) => scenarios::store_mixed::run(args),
        Command::ManagerWrite(args) => scenarios::manager_write::run(args),
    };

    match result {
        Ok(report) => {
            // Machine-parseable on stdout; human summary on stderr.
            match serde_json::to_string_pretty(&report) {
                Ok(json) => println!("{}", json),
                Err(e) => {
                    eprintln!("error: serialising report failed: {e}");
                    return std::process::ExitCode::from(2);
                }
            }
            eprintln!();
            eprintln!("{}", report.human_summary());
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::from(1)
        }
    }
}
