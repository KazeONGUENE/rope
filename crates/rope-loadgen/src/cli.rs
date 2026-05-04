//! CLI surface — clap derive types and shared option groups.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "rope-loadgen",
    version,
    about = "Quipu Canon v2.0 in-process throughput / latency / recovery harness",
    long_about = "Drives synthetic workloads against the LedgerStore (and, in a \
                  future patch, LedgerManager) to measure the throughput unlocked \
                  by Phase 1.1–1.5. Output is JSON to stdout, human summary to \
                  stderr."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
// All three variants currently start with `Store` because every Phase 1
// scenario targets the LedgerStore. As soon as `manager-write` lands
// (which targets LedgerManager), this prefix will diverge naturally.
#[allow(clippy::enum_variant_names)]
pub enum Command {
    /// Drive synthetic appends against `LedgerStore` and report
    /// throughput + latency percentiles + durability wait.
    StoreWrite(StoreWriteArgs),

    /// Open an existing RocksDB-backed `LedgerStore` and time the
    /// cold-recovery snapshot rebuild.
    StoreRecover(StoreRecoverArgs),

    /// Interleave put_descriptor / append / mark_deleted / get_chain
    /// against `LedgerStore` to model real-world mixed load.
    StoreMixed(StoreMixedArgs),

    /// Drive `LedgerManager::append_to_ledger` end-to-end. Exercises
    /// every Phase 1 piece together: P1.1 sharded lattice, P1.2 head
    /// lock, P1.3 per-shard HLC, P1.4 OES key cache, P1.5 LedgerStore
    /// mirror.
    ManagerWrite(ManagerWriteArgs),

    /// Quipu Canon v2.0 Phase 2.C — measure the speedup of
    /// `HybridVerifier::verify_batch` over a serial loop of
    /// `HybridVerifier::verify`. Drives an N-item synthetic batch of
    /// hybrid-signed knots (Ed25519 + Dilithium3) and reports ops/s
    /// plus mean-per-item µs for each path.
    VerifyBatch(VerifyBatchArgs),
}

/// How to choose the wallet for each op — drives the contention shape.
#[derive(ValueEnum, Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
    /// Every thread, every op, hits the same single wallet. Maximum
    /// per-wallet head-lock contention (P1.2). Worst-case lattice
    /// contention if/when wired through LedgerManager.
    Same,
    /// Wallets are partitioned across threads — each thread owns a
    /// disjoint slice of the wallet pool. Zero head-lock contention;
    /// pure parallelism test.
    Partitioned,
    /// Each op picks a random wallet from the pool. Realistic mixed
    /// shape — measures average-case contention.
    Random,
}

/// In-memory `LedgerStore::new()` vs disk-backed `LedgerStore::open()`.
#[derive(ValueEnum, Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Pure in-memory mirror, no persistence (v1.x default).
    Memory,
    /// RocksDB-backed via the WriteBatch background flusher (Phase 1.5).
    Rocksdb,
}

/// Common knobs shared by `store-write` and `store-mixed`.
#[derive(Args, Debug, Clone)]
pub struct CommonWorkloadArgs {
    /// Number of worker threads.
    #[arg(short = 't', long, default_value_t = 8)]
    pub threads: usize,

    /// Total ops across the whole workload (split evenly across
    /// threads). Each thread runs `ops / threads` ops.
    #[arg(short = 'o', long, default_value_t = 100_000)]
    pub ops: usize,

    /// Distinct wallets to populate before timing starts.
    #[arg(short = 'w', long, default_value_t = 1_000)]
    pub wallets: usize,

    /// Wallet-selection scenario.
    #[arg(short = 's', long, value_enum, default_value_t = Scenario::Partitioned)]
    pub scenario: Scenario,

    /// Storage backend.
    #[arg(short = 'm', long, value_enum, default_value_t = Mode::Memory)]
    pub mode: Mode,

    /// When `--mode rocksdb`, persist to this path. If omitted, a
    /// fresh tempdir is created and removed at exit.
    #[arg(long)]
    pub db_path: Option<PathBuf>,

    /// When `--mode rocksdb`, after the workload call
    /// `await_all_durable` and report the wait duration as part of
    /// the latency / throughput accounting.
    #[arg(long, default_value_t = true)]
    pub await_durable: bool,

    /// RNG seed for reproducible runs.
    #[arg(long, default_value_t = 0xDA7A_C4A1_2718_28A1u64)]
    pub seed: u64,
}

#[derive(Args, Debug)]
pub struct StoreWriteArgs {
    #[command(flatten)]
    pub common: CommonWorkloadArgs,

    /// If set, time only the appends — pre-create all descriptors
    /// before the timer starts. Default: descriptors created lazily
    /// during the timed phase.
    #[arg(long)]
    pub prelude_descriptors: bool,
}

#[derive(Args, Debug)]
pub struct StoreRecoverArgs {
    /// Path to an existing RocksDB-backed `LedgerStore`. Must have
    /// been written by a previous run (e.g. `store-write --mode
    /// rocksdb --db-path X`).
    #[arg(long)]
    pub db_path: PathBuf,

    /// Repeat the cold-open this many times (each iteration drops the
    /// store and reopens) for a more stable timing.
    #[arg(short = 'n', long, default_value_t = 3)]
    pub iterations: usize,
}

/// Args for the `manager-write` subcommand.
///
/// `manager-write` always pre-creates one ledger per wallet (untimed),
/// then drives `LedgerManager::append_to_ledger` against each. The
/// timed phase therefore only measures appends — the create_ledger cost
/// is captured but reported separately.
#[derive(Args, Debug, Clone)]
pub struct ManagerWriteArgs {
    #[command(flatten)]
    pub common: CommonWorkloadArgs,

    /// Bytes per `InteractionRecord.data` payload. Bigger payloads
    /// stress the encryption + slicing path harder.
    #[arg(long, default_value_t = 256)]
    pub payload_bytes: usize,

    /// If > 1, rotate the OES generation every N appends to measure
    /// P1.4 cache miss cost. `1` means hit cache on every append; `0`
    /// disables rotation entirely (always hit). Default: never rotate.
    #[arg(long, default_value_t = 0)]
    pub oes_rotate_every: u64,
}

/// Args for the `verify-batch` subcommand (Quipu Canon v2.0 Phase 2.C).
#[derive(Args, Debug, Clone)]
pub struct VerifyBatchArgs {
    /// Total number of items to generate and verify on each path.
    #[arg(short = 'n', long, default_value_t = 256)]
    pub items: usize,

    /// Number of distinct keypairs to use across the batch. With
    /// `--keys 1`, every item shares the same public key — best case
    /// for the parsed-PK cache. Default: one key per item (worst case
    /// for the cache, fairest comparison).
    #[arg(short = 'k', long, default_value_t = 0)]
    pub keys: usize,

    /// Bytes per signed message.
    #[arg(short = 'p', long, default_value_t = 256)]
    pub payload_bytes: usize,

    /// How many measurement iterations to take (each iteration runs
    /// both the serial and the batch path; results are averaged).
    #[arg(short = 'i', long, default_value_t = 5)]
    pub iterations: usize,

    /// Reset the parsed-PK cache before each iteration. Default:
    /// keep the cache warm (matches steady-state node operation).
    #[arg(long)]
    pub cold_cache: bool,

    /// RNG seed for reproducible runs.
    #[arg(long, default_value_t = 0xDA7A_C4A1_2718_28A1u64)]
    pub seed: u64,
}

#[derive(Args, Debug)]
pub struct StoreMixedArgs {
    #[command(flatten)]
    pub common: CommonWorkloadArgs,

    /// Probability weights — appends, put_descriptor, mark_deleted,
    /// get_chain. Floats; not required to sum to 1 (normalised
    /// internally).
    #[arg(long, default_value_t = 0.70)]
    pub weight_append: f64,
    #[arg(long, default_value_t = 0.10)]
    pub weight_put_descriptor: f64,
    #[arg(long, default_value_t = 0.05)]
    pub weight_mark_deleted: f64,
    #[arg(long, default_value_t = 0.15)]
    pub weight_get_chain: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_all_subcommands() {
        // `clap`'s `debug_assert` catches malformed CLI definitions —
        // simply asserting that the command builds is enough to fail
        // the test if someone breaks the spec.
        Cli::command().debug_assert();
    }

    #[test]
    fn store_write_accepts_basic_flags() {
        let cli = Cli::try_parse_from([
            "rope-loadgen",
            "store-write",
            "--threads",
            "4",
            "--ops",
            "1000",
            "--wallets",
            "10",
            "--scenario",
            "same",
            "--mode",
            "memory",
            "--prelude-descriptors",
        ])
        .expect("parse");
        match cli.command {
            Command::StoreWrite(a) => {
                assert_eq!(a.common.threads, 4);
                assert_eq!(a.common.ops, 1000);
                assert_eq!(a.common.wallets, 10);
                assert!(matches!(a.common.scenario, Scenario::Same));
                assert!(matches!(a.common.mode, Mode::Memory));
                assert!(a.prelude_descriptors);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn store_recover_requires_db_path() {
        let err = Cli::try_parse_from(["rope-loadgen", "store-recover"]);
        assert!(err.is_err(), "store-recover must require --db-path");
    }

    #[test]
    fn store_recover_accepts_path_and_iterations() {
        let cli = Cli::try_parse_from([
            "rope-loadgen",
            "store-recover",
            "--db-path",
            "/tmp/x",
            "-n",
            "5",
        ])
        .expect("parse");
        match cli.command {
            Command::StoreRecover(a) => {
                assert_eq!(a.iterations, 5);
                assert_eq!(a.db_path.to_str(), Some("/tmp/x"));
            }
            _ => panic!("wrong subcommand"),
        }
    }
}
