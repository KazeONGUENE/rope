# Residual erpc 5xx — P1 critique response (2026-07-27)

## Verdict on P1-as-shipped

Ack-after-enqueue **alone** was a mask risk: the enqueue channel was
**unbounded** (`mpsc::channel`), so a wedged flusher could grow memory
while RPCs returned success. That is worse than a hang the watchdog can
see.

**Target state (now in source):** fast honest overload — bounded queue +
`try_send` → JSON-RPC `-32005` with `Retry-After: 1` in the message —
not “zero 5xx” by hiding stalls.

## Measurements that decided the critique

| Question | Finding |
|---|---|
| Is the enqueue channel bounded? | **Was unbounded.** Now `sync_channel` default cap **8192** (`ROPE_LEDGER_QUEUE_CAP`). |
| Flusher error after ack? | Enqueue failures were **swallowed** (`LedgerStore::enqueue` logged and returned `0`). Now propagated; QueueFull never updates the in-memory mirror. Disk-full after a successful enqueue remains the ack-after-enqueue crash window (≤ flusher tick) — callers that need stronger guarantees set `ROPE_SYNC_DURABILITY=1`. |
| ~14 min interval? | **Watchdog math**, not a chain epoch: median 840s ≈ `STARTUP_GRACE_S(600)` + 2×2min fail threshold. Hang often starts earlier (~4–6 min loopback timeout). “200 blocks × 4.2s” is a red herring. |
| RocksDB write stalls? | Current + historical `LOG` greps showed **`total-stops: 0` / no stall lines**. Still a ranked suspect under load; not confirmed as the 2026-07-27 driver. |
| OES key cache TTL? | **No TTL**; derivation is outside the write lock. Mass-expiry largely ruled out. |
| Append then erase ordering? | Single FIFO flusher + per-wallet head lock: erase/untie enqueue after that wallet’s appends; sync wait on GDPR paths. |

## What is in source now (P1.1)

| Change | Detail |
|---|---|
| Bounded enqueue | `RocksPersistence` uses `mpsc::sync_channel`; `try_send` → `RocksError::QueueFull` |
| No swallowed failures | `LedgerStore` mutating APIs return `Result`; enqueue-before-mirror |
| Honest overload RPC | `OVERLOAD: … Retry-After: 1` → JSON-RPC **`-32005`** on create/append |
| Ack-after-enqueue (default) | Still default for create/append; only safe with the bound |
| GDPR always sync | `erase` / `untie` still `await_all_durable`; timeout hard-errors |
| Watchdog dump-only | `ROPE_WATCHDOG_DUMP_ONLY=1` — forensics, no SIGKILL; prefers `eu-stack` |
| Symbols build | Single `release-syms` build + `objcopy --only-keep-debug` / `--add-gnu-debuglink` |

## P1.2 — structural lock fixes (evaluated and applied)

Playbook assessment: **relevant**. Architecture (256-shard lattice, RocksDB actor,
`-32005` fast-reject) was already right; residual hang matched three leftover
contention points on the append path. Implemented in source; tests green
(`rope-core` personal_ledger 19/19 + lattice 16/16; `rope-node --lib` 147/147).

| Problem | Fix | Where |
|---|---|---|
| `StringRegistry` process-wide write convoy | **256-shard registry** — entity maps by `id_bytes[0]`, knot→owner by `StringId[0]`; appends to different shards no longer share one `RwLock` | `crates/rope-core/src/personal_ledger.rs` (`REGISTRY_SHARDS`) |
| Head lock held across durability wait | **`drop(head_guard)` before `await_durable` / sync waits** on create / append / erase / untie | `crates/rope-node/src/ledger_manager.rs` |
| Finality BFS / sweep inside `add_string` | **`schedule_finality()` → background actor** (coalescing channel); sync fallback if actor not started; node calls `lattice.start_finality_actor()` | `crates/rope-core/src/lattice.rs`, `crates/rope-node/src/node.rs` |

**Do not claim the hang is solved** until a post-deploy soak (or a dump-only
stack) shows the futex convoy is gone. P1.1 still applies for overload /
forensics; P1.2 is the structural attempt at the wedge root cause.

## P1.3 — StringLattice lock decoupling + BLUE-pin Quipu reads

Hang dump `rope-node-hang-2026-07-27T062606Z` (post-P1.2 wedge) ruled out
RocksDB stalls and Tokio pool exhaustion (26 threads; flusher idle). Confirmed
root cause: `StringLattice` RwLock inversion —

- `update_finality` held `pending.read()` / `anchors.read()` across BFS
- `check_anchor_creation` still ran inside `add_string` (`anchors.write()`)
- concurrent `walk_*` + `add_string` stacked on shard locks

| Fix | Where |
|---|---|
| `check_anchor_creation` off `add_string` → `anchor_candidates` + maintenance actor | `lattice.rs` |
| Snapshot pending ids / anchor ids before BFS; never hold those locks across `get_parents` | `lattice.rs` |
| Document lock hierarchy on `add_string` | `lattice.rs` |
| Pin **all** `rope_*` methods to BLUE (`rpc_primary_only`) — Quipu ledger is not replicated | `deploy/nginx/conf.d/njs/rpc_router.js` |

Keep `ROPE_WATCHDOG_DUMP_ONLY=1` until a post-P1.3 soak shows no lattice convoy.

## Architecture note — Quipu is not iso across the 4-node committee

EVM/Reth state is quorum-replicated. Quipu personal ledgers (`~/.rope/ledger_db`)
are **per-node** and write-pinned to BLUE. GREEN/DO1/DO2 near-empty ledgers are
expected until an explicit ledger-replication protocol exists. Do not fail over
`rope_globalStats` / other `rope_*` reads.

## P1.4 — DashMap for lattice walks (proactive)

No post-P1.3 wedge was observed before this change (BLUE stayed healthy after
07:19Z). Proceeding with the deferred walk/append convoy fix anyway:

| Change | Detail |
|---|---|
| Hot maps → `DashMap` / `DashSet` | `strings`, `complements`, `parents`, `children`, `erased`, `tombstones` |
| `pending` stays `RwLock<BTreeMap>` | time-ordered; P1.3 already snapshots before BFS |
| Publish order | complements → parents → pending → **strings last** |
| Walks | entry-level locks only (`walk_string_with_tombstones`, `walk_ledger_chain`) |

## Ranked suspects (post-P1.4)

1. Confirm dumps no longer show whole-map `RawRwLock` piles on `add_string` / `walk_*`.
2. If wedge returns, inspect maintenance actor + `pending` / `anchors` only.
3. ~~RocksDB~~ / ~~Tokio 512~~ / ~~StringRegistry~~ / ~~anchors/pending lock inversion~~ / ~~whole-map shard RwLock on walks~~ — ruled out or fixed.

## P1.4 soak monitoring (production-adapted)

Draft Prometheus/DashMap/jemalloc collectors were **not** ported. Production
reality:

| Draft idea | Production decision |
|---|---|
| `tikv-jemallocator` + `mallctl` | **Skip** — rope-node does not use jemalloc |
| `MonitoredDashMap` / shard `try_write` collector | **Skip** — metrics server has no `Arc` into `StringLattice`; scrape sampling would need a mid-soak rebuild for little soak value |
| `parking_lot` `deadlock_detection` feature | **Skip** — rebuild + runtime cost; dump-only watchdog already captures wedges |
| Process RSS / threads | **Ship** — scrape-time `/proc/self` gauges on existing `127.0.0.1:9090/metrics` |
| Ops soak samples | **Ship** — `deploy/scripts/p14-soak-monitor.sh` |

### What to watch (1–2 h, keep `ROPE_WATCHDOG_DUMP_ONLY=1`)

1. **Ledger invariant** — public `rope_globalStats` ≈ BLUE (`~79` strings / `~57k` knots, `invariant_holds: true`), **never** DO1 residue (`2` / `3`).
2. **Router isolation** — `rpc_router.js` `needsPrimaryOnly` includes all `rope_*` → `@rpc_primary` (BLUE). Verified post-P1.3.
3. **Latency** — loopback `eth_blockNumber` stays low-ms; append overload → immediate `-32005` / `Retry-After: 1`, not rising futex waits / HTTP 504.
4. **Memory** — `process_resident_memory_bytes` on `:9090/metrics` or soak-script `rss_kb` column; watch for unbounded climb under append bursts.
5. **Dumps** — zero new `rope-node-hang-*` under dump-only = lattice convoy structurally resolved for this window.

```bash
# On BLUE — run soak (default 2h / 30s samples)
sudo install -m 755 deploy/scripts/p14-soak-monitor.sh \
  /opt/datachain-rope/scripts/p14-soak-monitor.sh
nohup bash /opt/datachain-rope/scripts/p14-soak-monitor.sh \
  >/tmp/p14-soak-nohup.out 2>&1 &

# Live process gauges (after metrics.rs deploy + restart)
curl -sS http://127.0.0.1:9090/metrics | grep -E '^process_(resident|virtual|threads)'
```

## P1.4 soak result + HA follow-up (2026-07-28)

2h soak completed dump-free. Afterwards `ROPE_WATCHDOG_DUMP_ONLY=1` was left on
cron and a later BLUE wedge produced MetaMask “Unable to connect” with no
self-heal. Fixed 2026-07-28:

| Control | Location |
|---|---|
| 30s fleet HA timer (detect + restart) | `erpc-fleet-ha.timer` on BLUE |
| Dump-only cron removed; escalate if re-enabled | `rope-node-watchdog.sh` |
| eth_* read timeout 3s → GREEN/DO | `datachain.network.conf` `@rpc_failover` |
| Fleet status | `https://erpc.datachain.network/v1/fleet-status` |
| DNS watcher (BLUE edge down → DO1) | DO rpc-1 `erpc-dns-failover.timer` |

Writer promote across committee nodes remains deferred (silent-unmined risk).
See `.cursor/rules/handover-to-dcswap-erpc-ha-autonomous-failover-2026-07-28.mdc`.

## Deploy checklist (BLUE)

```bash
# 0. Optional: diagnose before killing — on the VPS
#    echo 'ROPE_WATCHDOG_DUMP_ONLY=1' | sudo tee /etc/systemd/system/datachain-rope.service.d/60-watchdog-dump-only.env
#    # or export in the cron wrapper; then: if ping fails, dumps land without restart.
#    Prefer: sudo apt-get install -y elfutils   # for eu-stack

# 1. Sync source + watchdog
rsync -avz --exclude target/ --exclude .git/ \
  ./datachain-rope/ rope-vps:/home/ubuntu/datachain-rope/
rsync -avz datachain-rope/deploy/scripts/rope-node-watchdog.sh \
  rope-vps:/opt/datachain-rope/scripts/rope-node-watchdog.sh

# 2. Build ONCE (prefer off-box or nice'd); split symbols from the same binary
ssh rope-vps 'export PATH=$HOME/.cargo/bin:$PATH && cd /home/ubuntu/datachain-rope && \
  nice -n 15 cargo build --profile release-syms -p rope-cli && \
  cp -a target/release-syms/rope ~/backup-rope-$(date -u +%Y%m%dT%H%MZ) && \
  (test -f target/release/rope.syms && cp -a target/release/rope.syms ~/backup-rope.syms-$(date -u +%Y%m%dT%H%MZ) || true) && \
  objcopy --only-keep-debug target/release-syms/rope target/release/rope.syms && \
  objcopy --strip-debug --add-gnu-debuglink=target/release/rope.syms \
    target/release-syms/rope target/release/rope'

# 3. Restart (do NOT set ROPE_SYNC_DURABILITY unless auditing)
#    Optional: ROPE_LEDGER_QUEUE_CAP=8192 (default)
ssh rope-vps 'sudo systemctl restart datachain-rope.service'

# 4. Soak ≥60 min with positive signals, not only absence of RESTARTING:
#    - journal / metrics: -32005 OVERLOAD rate
#    - p99 RPC latency on loopback eth_blockNumber + append
#    - RocksDB LOG stall counters
#    - queue pressure (QueueFull errors under burst)
```

## Operator knobs

| Env | Default | Meaning |
|---|---|---|
| `ROPE_LEDGER_QUEUE_CAP` | `8192` | Bounded flusher enqueue depth |
| `ROPE_SYNC_DURABILITY` | unset (ack-after-enqueue) | `1` = RPC waits for fsync |
| `ROPE_WATCHDOG_DUMP_ONLY` | unset (restart) | `1` = dump, do not kill |

## Quipu Phase 1 note

Phase 1 (sharded lattice, head locks, HLC, OES cache, RocksDB) is already
default-on. Do not invent new lock schemes in the v1 write path outside
labelled Phase work. Remaining scale work is Phase 2/3/4 ops.
