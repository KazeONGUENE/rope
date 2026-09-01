# rope-addr-indexer - operator runbook (Workstream C)

**Status:** code-complete, `cargo test -p rope-addr-index` = **23/23 green**, `cargo check --workspace` clean. Not yet deployed to rope-vps. Feature-flag-off by default; DCScan continues to answer address queries via the legacy RPC-scan path until an operator sets `ADDR_INDEX_PATH` on the `dc-explorer` unit.

**Purpose:** eliminate the residual 504 Gateway Timeout on address pages for high-history wallets (bot pools, treasuries, DCSwap router). Answers `Transactions`, `Internal Txns`, `Token Transfers (DCR-20)`, and `Events` tabs in O(page_size) reverse-iterator seeks against a per-address RocksDB index instead of scanning O(N) blocks via JSON-RPC on every request.

Reference handover: `.cursor/rules/handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc` §9 (the 504 fix that this crate closes for good).

---

## 1. Deploy (fresh install on rope-vps)

Every step is idempotent and reversible. The indexer runs entirely alongside the current stack (rope-node, reth-rope, dc-explorer) and does not touch any of them until §3 flips the read-flag.

```bash
# 1.1 - build the release binaries locally, ship via git or rsync (workspace
#       policy is build-on-jammy for OS-parity across the fleet - see
#       deploy/scripts/deploy-fleet.sh from the 2026-06-12 V11 hot-fix).
ssh rope-vps 'cd /home/ubuntu/datachain-rope && \
  cargo build --release -p rope-addr-index --bin rope-addr-indexer'
ls -la /home/ubuntu/datachain-rope/target/release/rope-addr-indexer   # binary present

# 1.2 - install the systemd unit + example config.
sudo cp /home/ubuntu/datachain-rope/deploy/rope-addr-indexer.service \
  /etc/systemd/system/rope-addr-indexer.service
sudo cp /home/ubuntu/datachain-rope/deploy/rope-addr-indexer.example.toml \
  /etc/rope-addr-indexer.toml
sudo chmod 0644 /etc/rope-addr-indexer.toml
sudo chown root:root /etc/rope-addr-indexer.toml

# 1.3 - create the data directory that ReadWritePaths= binds over.
sudo mkdir -p /var/lib/rope-addr-index
sudo chown ubuntu:ubuntu /var/lib/rope-addr-index
sudo chmod 0750 /var/lib/rope-addr-index

# 1.4 - sanity: print the resolved config without touching RocksDB.
sudo -u ubuntu /home/ubuntu/datachain-rope/target/release/rope-addr-indexer \
  --config /etc/rope-addr-indexer.toml --dry-run

# 1.5 - enable + start (writes only, reads still on the legacy path).
sudo systemctl daemon-reload
sudo systemctl enable --now rope-addr-indexer.service
systemctl is-active rope-addr-indexer.service   # expect: active
```

Watch the first minute of the journal for the following canonical log lines:

```bash
journalctl -u rope-addr-indexer -f
# expect within 2-5 s:
#   INFO rope_addr_indexer: resolved config data_dir=... rpc_urls=[...] ...
#   INFO rope_addr_indexer: reth tip at startup tip=<N>
#   INFO rope_addr_indexer: connected to Datachain Rope mainnet chain_id=271828
# then, on the tip follower:
#   INFO rope_addr_indexer::tip: ingested block=<N+1> addrs=... txs=... logs=...
# and, on the backfiller:
#   INFO rope_addr_indexer::backfill: ingested block=<N-1> addrs=... txs=... logs=...
```

If the tip follower ingests forward while the backfiller works backward, everything is healthy. Backfill completion is signalled by:

```
INFO rope_addr_indexer::backfill: reached floor=0 - backfill complete
```

Full backfill of a chain with ~3.7 M anchor knots and modest tx/log density takes on the order of an hour on rope-vps' spinning disk; the indexer is I/O-bound on Reth's `eth_getBlockByNumber` + `eth_getLogs`, not on RocksDB writes.

---

## 2. Verify

```bash
# 2.1 - store on disk.
sudo du -sh /var/lib/rope-addr-index   # expect: growing steadily during backfill
sudo ls /var/lib/rope-addr-index       # expect: OPTIONS-*, LOCK, MANIFEST-*, CURRENT, LOG, OPTIONS, SST files

# 2.2 - meta-cf progress (via a one-off dry-run open, does not race the live writer).
sudo -u ubuntu /home/ubuntu/datachain-rope/target/release/rope-addr-indexer \
  --config /etc/rope-addr-indexer.toml --dry-run
# The service itself already logs head_block / backfill_low_water on every tick.
# For an operator-visible dashboard, dc-explorer can expose a /api/v1/addr-index/status
# endpoint that opens the same store read-only (Phase 2, not required for this deploy).

# 2.3 - reorg-safety spot check (the indexer keeps the last 128 canonical
# block hashes and unwinds any orphaned block by consulting the per-block
# address set stored in the meta CF). No manual action required; verified
# by the reorg::tests::unwind_removes_all_traces_of_a_block unit test in CI.
```

---

## 3. Turn reads on (operator gate)

The switch is a single environment variable on the `dc-explorer` unit. When set, dc-explorer opens the same RocksDB read-only and serves address / transactions / events tabs from the index; when unset, dc-explorer keeps using the legacy RPC-scan path.

```bash
# 3.1 - add the flag to whichever env file dc-explorer already reads
#       (production is /opt/datachain-rope/code/deploy/.env on rope-vps).
sudo tee -a /opt/datachain-rope/code/deploy/.env <<'EOF'

# rope-addr-index (2026-08-11 handover Workstream C).
ADDR_INDEX_PATH=/var/lib/rope-addr-index
EOF

# 3.2 - restart dc-explorer to pick it up.
sudo systemctl restart dc-explorer.service
systemctl is-active dc-explorer.service

# 3.3 - external verification (from any workstation).
curl -sSI 'https://dcscan.io/api/v1/accounts/0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195/transactions?page=1&limit=25'
# expect: HTTP 200 in <200 ms even for high-history wallets (was 8 s / graceful timeout).
```

**Rollback (instant, no data loss):**

```bash
sudo sed -i '/ADDR_INDEX_PATH=/d' /opt/datachain-rope/code/deploy/.env
sudo systemctl restart dc-explorer.service
```

dc-explorer immediately reverts to the legacy RPC-scan path. The indexer keeps running; the store on disk is untouched.

---

## 4. Common ops

### Restart

Idempotent; safe at any moment. Backfill low-water and canonical-hash retention are persisted after every block, so a restart resumes exactly where it stopped.

```bash
sudo systemctl restart rope-addr-indexer.service
```

### Change RPC failover order

Edit `/etc/rope-addr-indexer.toml`, `sudo systemctl restart rope-addr-indexer.service`. Config is only read at startup; there is no hot-reload path (deliberate - RPC URL changes are rare and the restart is cheap).

### Staged backfill floor

Instead of walking all the way to genesis on first deploy, override the floor:

```bash
# /etc/rope-addr-indexer.toml
backfill_floor = 3700000   # last ~50k blocks first; prove pipeline

# then, once satisfied:
backfill_floor = 0
sudo systemctl restart rope-addr-indexer.service
```

### Nuke and re-backfill (rare)

The `--reset-index` CLI flag deletes the data directory contents before opening the store. Protected by a hard-coded refusal to touch any path whose last segment does not contain `"index"` or `"rope"`, so a mis-typed `data_dir` cannot delete an unrelated database.

```bash
sudo systemctl stop rope-addr-indexer.service
# read-only dc-explorer flag also flipped off first if reads were live:
sudo sed -i '/ADDR_INDEX_PATH=/d' /opt/datachain-rope/code/deploy/.env
sudo systemctl restart dc-explorer.service

sudo -u ubuntu /home/ubuntu/datachain-rope/target/release/rope-addr-indexer \
  --config /etc/rope-addr-indexer.toml --reset-index
# The binary logs the reset, deletes files, then exits. Restart the service
# to re-open a blank store and begin the backfill.
sudo systemctl start rope-addr-indexer.service
```

Do not `rm -rf /var/lib/rope-addr-index` while the service is running - RocksDB refuses to delete files another process still has open.

---

## 5. Diagnostics

| Symptom | Where to look | Likely cause |
|---|---|---|
| Service crash-loops on start | `journalctl -u rope-addr-indexer -n 100 --no-pager` | Bad config (typo in `data_dir`, RPC URL scheme missing `http://`), or RocksDB lock left by an unclean shutdown (RocksDB releases the lock on next open unless the FS still holds it - restart usually fixes) |
| Tip follower stalls | Journal shows `tip fetch failed` and no `ingested` lines | All 4 RPC URLs failing simultaneously (verify with `curl -sSI http://127.0.0.1:8545/`); check that `datachain-rope.service` is up |
| Backfill never advances | Journal shows repeated `ingest failed for block=<N>` | Same as above - almost always an RPC issue upstream. The tip follower will keep making forward progress meanwhile |
| Reorg unwind loop | Journal shows many `reorg detected` lines in a row | Only possible if Reth is itself reorging that hard; the indexer caps unwind depth at 64 blocks per tick, so the worst case is delayed forward progress, not data loss |
| Store grows unusually fast | `sudo du -sh /var/lib/rope-addr-index` doubles overnight | Legitimate for a hot phase (bot cohort catching up); if it persists, check for a runaway log emitter on-chain |
| dc-explorer reads stale data after `ADDR_INDEX_PATH` flip | Journal for dc-explorer + a spot `curl` against `/api/v1/accounts/…/transactions` | dc-explorer's read-only handle caches the store snapshot at open; restarting `dc-explorer.service` re-opens with the current on-disk state |

---

## 6. Cost profile

- **CPU**: negligible during tip-follow (one `eth_blockNumber` + one `eth_getBlockByNumber` per 2 s). During backfill: sustained ~10-30 %% of one core on rope-vps, RocksDB compaction dominates.
- **RAM**: cap is 1 GB in the systemd unit. Steady-state is ~100-200 MB (RocksDB block cache + one in-flight WriteBatch).
- **Disk**: on the order of hundreds of MB per year of chain history at current density. Compaction is per-address prefix so writes are sequential within each address, which is friendly to spinning disks.
- **Net**: BLUE loopback dominates, so ~0 external bandwidth in steady state. During a failover, ~few kB per block against GREEN / DO.

---

## 7. Why this is safe to ship dark

The invariant this crate depends on is `eth_getBlockByNumber(N, true)` on Reth returning a block whose `parentHash` matches the hash of block `N-1` we recorded at ingest time. That is the same invariant every other component (rope-node, dc-explorer's `/api/v1/stats`, DCScan's transaction pages) already relies on. If Reth breaks it, everything downstream breaks - not just the address index.

The index does not fork the source of truth. It only projects a subset of the chain (per-address (block, index) -> (tx_hash, category)) into RocksDB. Every downstream reader (dc-explorer, dcscan) can always ignore the index and re-derive the same data via JSON-RPC. That is exactly how the feature-flag-off rollout works: with `ADDR_INDEX_PATH` unset, the index is a write-only sidecar; setting the flag simply says "prefer the index if it has data, fall back to RPC otherwise" - and dc-explorer's timeout guard from the 2026-08-11 fix (§9 of the handover) is unchanged, so a stale / corrupted index cannot revive the 504.

Rollback is a two-line env edit; there is no on-chain state, no user-facing state, and no coordination requirement with any other project.
