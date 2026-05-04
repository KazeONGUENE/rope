# `rope-loadgen` — Quipu Canon v2.0 throughput / latency / recovery harness

In-process load generator that drives synthetic workloads against the
`LedgerStore` (and, in a follow-up patch, `LedgerManager`). It measures
throughput, latency percentiles, and cold-recovery time so operators can
quantify the wins from the five Phase 1 architectural pieces.

Spec reference: `docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §10.1.

## What each Phase 1 piece this harness exercises

| Phase 1 piece                        | `store-write` | `store-recover` | `store-mixed` |
| ------------------------------------ | :-----------: | :-------------: | :-----------: |
| **P1.1** Sharded `StringLattice`     |     —         |       —         |       —       |
| **P1.2** Per-wallet head-string lock |     ✓         |       —         |       ✓       |
| **P1.3** Per-shard HLC               |     ✓ (indirect) |    —         |       ✓ (indirect) |
| **P1.4** OES key cache               |     —         |       —         |       —       |
| **P1.5** RocksDB persistence         |     ✓         |       ✓         |       ✓       |

P1.1 and P1.4 are exercised at the `LedgerManager` layer, which the
harness does not currently drive — they are covered by a follow-up
`manager-write` subcommand.

## Build

```bash
cargo build --release -p rope-loadgen
```

## Subcommands

### `store-write` — synthetic appends

```bash
# In-memory, 8 threads × 100k ops / 1k wallets, partitioned (no contention)
cargo run --release -p rope-loadgen -- store-write \
  --threads 8 --ops 100000 --wallets 1000 \
  --scenario partitioned --mode memory

# RocksDB-backed, same shape, persisted to a tempdir, awaiting durability
cargo run --release -p rope-loadgen -- store-write \
  --threads 8 --ops 100000 --wallets 1000 \
  --scenario partitioned --mode rocksdb

# Same scenario (max head-lock contention via P1.2)
cargo run --release -p rope-loadgen -- store-write \
  --threads 16 --ops 100000 --wallets 1000 --scenario same --mode memory
```

Flags:

| Flag                    | Default | Meaning                                                                |
| ----------------------- | :-----: | ---------------------------------------------------------------------- |
| `-t, --threads`         | 8       | Worker threads                                                         |
| `-o, --ops`             | 100000  | Total ops, split evenly across threads                                 |
| `-w, --wallets`         | 1000    | Distinct wallet pool                                                   |
| `-s, --scenario`        | partitioned | `same`, `partitioned`, or `random` wallet selection                |
| `-m, --mode`            | memory  | `memory` or `rocksdb`                                                  |
| `--db-path`             | (tempdir) | When `--mode rocksdb`, persist here instead of a tempdir             |
| `--await-durable`       | true    | After the timed phase, block until every write is fsync'd              |
| `--prelude-descriptors` | false   | Pre-create descriptors before the timed phase (untimed)                |
| `--seed`                | (constant) | RNG seed for reproducibility                                        |

### `store-recover` — cold-open + recovery snapshot rebuild

```bash
# 1) Generate a database
cargo run --release -p rope-loadgen -- store-write \
  --mode rocksdb --db-path /tmp/rope-loadgen-state \
  --threads 8 --ops 1000000 --wallets 50000 --prelude-descriptors

# 2) Time how long opening + recovering it takes
cargo run --release -p rope-loadgen -- store-recover \
  --db-path /tmp/rope-loadgen-state -n 5
```

This is the cost the operator sees as "node startup time" after a
restart. Important to track because P1.5 trades a small per-write
overhead for a recovery cost that scales linearly with disk state size.

### `store-mixed` — interleaved real-world load

```bash
cargo run --release -p rope-loadgen -- store-mixed \
  --threads 8 --ops 100000 --wallets 5000 \
  --weight-append 0.70 --weight-put-descriptor 0.10 \
  --weight-mark-deleted 0.05 --weight-get-chain 0.15 \
  --mode rocksdb
```

Default weights model a typical agent fleet: 70% appends, 10%
descriptor upserts, 5% deletions, 15% chain reads.

## Output format

stdout: a single JSON object suitable for `jq`. stderr: a
human-readable summary plus structured `tracing` logs.

Example output (truncated):

```json
{
  "scenario_kind": "store-write",
  "mode": "rocksdb",
  "scenario": "partitioned",
  "threads": 8,
  "ops_total": 100000,
  "wallets": 1000,
  "elapsed_ms": 152.34,
  "durability_wait_ms": 11.20,
  "throughput_ops_per_sec": 656_543,
  "throughput_inc_durability_ops_per_sec": 612_215,
  "latency": {
    "samples": 100000,
    "mean_us": 12.1,
    "p50_us": 9.5,
    "p95_us": 28.6,
    "p99_us": 65.3,
    "p999_us": 142.0,
    "max_us": 982.4
  }
}
```

## Reading the results

- **`throughput_ops_per_sec`** is the raw work rate — most useful for
  comparing scenarios.
- **`throughput_inc_durability_ops_per_sec`** includes the final
  `await_all_durable` wait — closer to the rate a caller demanding
  strict durability would observe.
- **`latency.p99_us`** highlights tail-latency from contention (P1.2)
  and from the RocksDB flush tick (~10 ms ceiling on `p99` is normal
  for `--mode rocksdb`).
- **`durability_wait_ms`** should always be < 20 ms for healthy runs;
  larger values indicate the flush queue is backed up, which is the
  canonical signal that the watermark needs more parallelism.

## Useful comparisons

```bash
# Quantify P1.5 (RocksDB) overhead
for mode in memory rocksdb; do
  cargo run --release -q -p rope-loadgen -- store-write \
    --threads 8 --ops 200000 --wallets 5000 --scenario partitioned --mode $mode \
    | jq -c '{mode, throughput_ops_per_sec, latency_p99_us: .latency.p99_us}'
done

# Quantify P1.2 (head-lock) contention cost
for s in same partitioned random; do
  cargo run --release -q -p rope-loadgen -- store-write \
    --threads 16 --ops 200000 --wallets 1000 --scenario $s --mode memory \
    | jq -c '{scenario, throughput_ops_per_sec, latency_p99_us: .latency.p99_us}'
done
```

## CI integration

`rope-loadgen` exits 0 on success, 1 on workload error, 2 on output
serialisation error. CI can pin a regression budget on
`throughput_ops_per_sec` and `latency.p99_us` from the JSON output and
fail the build if either crosses the threshold.
