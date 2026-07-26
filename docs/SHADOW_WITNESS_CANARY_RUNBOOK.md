# Shadow-Witness Canary Runbook

**Status.** Live as of 2026-05-09T09:16:07 UTC on `datachain-rpc-1` (DigitalOcean tertiary, Frankfurt). Soak window: 7 days.

**Spec source.** §6.1.1 of `papers/Datachain_Rope_Quipu_Proto_Computer_Anthropological_Paper.md`
**Architecture memo.** `docs/KNOT_HASH_V2_WITNESS_SHADOW_DESIGN.md`
**Crate.** `crates/rope-shadow-witness/`

---

## 1. What is deployed

| Resource | Value |
|---|---|
| Host | `datachain-rpc-1` (`157.230.18.45`, Ubuntu 22.04) |
| Binary | `/usr/local/bin/rope-shadow-witness` (11 MB, x86_64 ELF, glibc 2.35) |
| Config | `/etc/rope-shadow-witness/config.toml` |
| Data | `/var/lib/rope-shadow-witness/data/` (RocksDB) |
| Systemd unit | `/etc/systemd/system/rope-shadow-witness.service` |
| Local RPC bind | `127.0.0.1:8556` (loopback only; not exposed publicly) |
| Upstream | `https://erpc.datachain.network` (canonical chain via nginx failover) |
| Build root | `/root/shadow-build-root/` (300 KB sources; `target/` cache `~5 GB`) |

The canary observes the canonical chain via the public RPC (BLUE -> GREEN -> DO failover), not the local rope-node. This makes the witness independent of any single rope-node's health.

## 2. How to check health

```bash
# SSH in
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45

# Service status
systemctl status rope-shadow-witness

# Recent rounds
journalctl -u rope-shadow-witness -n 50 --no-pager

# Disk usage of the v2 chain store
du -sh /var/lib/rope-shadow-witness/data/

# Status RPC (machine-readable)
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"rope_v2_status","params":[],"id":1}' \
  http://127.0.0.1:8556
```

Healthy round looks like:

```
round complete wallets=64 knots_applied=4 wallets_failed=0
```

`wallets_failed` should be near zero. Transient single-wallet failures during upstream RPC degradation are expected; sustained `wallets_failed > 5%` of `wallets` is a signal.

## 3. How to read the v2 chain

```bash
# Walk the v2 chain for a specific string (use the canonical genesis_string_id from rope_listStrings)
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{
    "jsonrpc":"2.0",
    "method":"rope_v2_walkChain",
    "params":["0x9950adefe3af9739456cb8af4440049ec6e832b505691acf5b5a5409a570ce97", 0, 10],
    "id":1
  }' \
  http://127.0.0.1:8556 | python3 -m json.tool

# Look up one specific knot's v2 hash
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{
    "jsonrpc":"2.0",
    "method":"rope_v2_knotHash",
    "params":["0x9950ade...","42"],
    "id":1
  }' \
  http://127.0.0.1:8556
```

Output fields per entry:

- `event_id`: position of the knot in its string
- `event_type`: `"append"` or `"erasure"`
- `is_tombstone`: matches `event_type` for v0.1
- `knot_hash`: `h_i` per §6.1.1 (BLAKE3, hex, 32 bytes)
- `previous_hash`: `h_{i-1}`; equals `0x000...0` for `event_id = 0`
- `event_metadata_hash`: BLAKE3 over the v0.1 metadata (see §5)
- `observed_at_unix`: when this entry was tied; operational metadata, NOT in the §6.1.1 hash

The chain invariant: `entry[i].previous_hash == entry[i-1].knot_hash` for all `i >= 1`. This is verified by the included integration test and observable in the live RPC output.

## 4. How to roll back

The canary affects nothing on the canonical chain. Rollback is one command:

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 \
  'systemctl stop rope-shadow-witness && systemctl disable rope-shadow-witness'
```

To completely remove (after stop):

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 'bash -s' << 'EOF'
systemctl stop rope-shadow-witness 2>/dev/null
systemctl disable rope-shadow-witness 2>/dev/null
rm -f /etc/systemd/system/rope-shadow-witness.service
systemctl daemon-reload
rm -f /usr/local/bin/rope-shadow-witness
rm -rf /etc/rope-shadow-witness
rm -rf /var/lib/rope-shadow-witness
rm -rf /root/shadow-build-root
echo "removed"
EOF
```

This leaves the canonical chain entirely unaffected. The rope-node and reth-rope services on `datachain-rpc-1` are untouched throughout this canary's life cycle.

## 5. v0.1 fidelity scope (carried from the architecture memo)

The witness consumes only public RPC, so the §6.1.1 `EventMetadata` it constructs uses observable proxies for fields the canonical RPC does not yet expose:

| §6.1.1 field | v0.1 source | Future hardening |
|---|---|---|
| `event_id` | `knot_index` from `rope_getStringWithKnots` | Same |
| `event_type` | `"append"` (active) or `"erasure"` (tombstone) | Extended controlled vocabulary |
| `timestamp_bytes` | empty (active); `untied_at` big-endian (erasure) | RPC extension exposing knot timestamp |
| `witness_ids` | empty | RPC extension exposing per-knot signature set |
| `testimony_quorum` | 0 | RPC extension exposing consensus rule |
| `oes_key_shred_destinations` | empty | OES module exposure |
| `authorisation_proof` | empty (active); tombstone `audit_hash` (erasure) | RPC extension or in-process subscription |

The chain-continuity-under-erasure property is preserved at v0.1: tombstones are applied via `rope_core::knot_hash::tombstone_preimage`, and successor-knot continuity does not depend on the proxied fields.

## 6. Soak success criteria (next 7 days)

- Service remains `Active: active (running)` continuously (auto-restart on failure is enabled with `RestartSec=10`).
- `wallets_failed` per round consistently < 5% of `wallets`.
- Memory growth stays bounded (the RocksDB store's compaction handles long-term growth; expect linear growth in disk proportional to chain growth, ~1 KB per knot, so ~150 KB / s at production rate of ~150 knots/s observed today).
- No log lines at `WARN` or `ERROR` level except for transient upstream RPC blips.
- Determinism check (manual, mid-soak): on a separate machine, run an independent shadow witness against the same upstream and compare a sample of `rope_v2_walkChain` outputs against the canary's. Hashes must agree exactly. (This is the cross-witness-agreement property the architecture memo claims.)

If all criteria hold for 7 days, an automated promotion fires (see §6.5).

### 6.5 Automated promotion at canary T+7d

Status: **armed**, fires once at `2026-05-16T09:16:07 UTC`. Operator kill-switch available.

| Component | Path on BLUE (`rope-vps`) | Role |
|---|---|---|
| Health gate | `/usr/local/bin/canary-health-gate.sh` | SSHes to canary, evaluates 9 rows, exits 0 (PASS) or 1 (FAIL) |
| Per-target deploy | `/usr/local/bin/deploy-shadow-witness.sh` | Idempotent build+install+smoke on one target (`local` or `remote`) |
| Orchestrator | `/usr/local/bin/promote-shadow-witness.sh` | gate -> BLUE deploy -> GREEN deploy; logs to `/var/log/rope-shadow-witness-promotion.log` |
| One-shot service | `/etc/systemd/system/rope-shadow-witness-promote.service` | Runs the orchestrator |
| Calendar timer | `/etc/systemd/system/rope-shadow-witness-promote.timer` | `OnCalendar=2026-05-16 09:16:07 UTC` |
| Operator kill-switch | `/etc/rope-shadow-witness/promotion-disabled` (touch to disable) | Service unit refuses to start when this exists |
| Source of truth in repo | `datachain-rope/scripts/shadow-witness-promotion/` | All five files above + README |

#### Gate criteria (every row must pass)

1. `service.active == active`
2. `chain.first_observed_at_age_s >= 604800 s` (7 days, **data-derived** from `first_install_at_unix` — survives binary refresh)
3. `rounds.last_hour.failure_pct <= 5`
4. `logs.last_24h.error_count <= 50`
5. `chain.observed_strings >= 1`
6. `chain.observed_knots >= 1`
7. `rpc.local_status_ok == true`
8. `chain.last_observed_at_age_s <= 60 s`
9. `process.rss_kb < 524288`

#### How to abort the auto-promotion

```bash
ssh rope-vps 'echo "$(date -u): aborted by <name> because <reason>" | sudo tee /etc/rope-shadow-witness/promotion-disabled'
```

The systemd unit has `ConditionPathExists=!...` so even the service start path refuses to fire. Remove the file to re-enable.

#### How to inspect the gate today (T+0..T+7d)

```bash
ssh rope-vps 'sudo /usr/local/bin/canary-health-gate.sh --report-only'        # human-readable
ssh rope-vps 'sudo /usr/local/bin/canary-health-gate.sh --report-only --json' # machine-readable
```

#### What happens on each outcome

| Gate | BLUE deploy | GREEN deploy | Exit | Final state |
|---|---|---|---|---|
| FAIL | skipped | skipped | 0 | Canary still running; no production change |
| PASS | OK | OK | 0 | Three-witness mesh active (canary + BLUE + GREEN) |
| PASS | OK | FAIL | 2 | Two-of-three (canary + BLUE); GREEN left stopped |
| PASS | FAIL | skipped | 1 | One-of-three (canary only); BLUE left stopped |

#### Determinism cross-check after promotion

Once the three-witness mesh is up, run the same `rope_v2_walkChain(string_id, 0, 50)` against canary, BLUE, and GREEN. The `knot_hash`, `previous_hash`, and `event_metadata_hash` fields must be byte-identical for every event_id present on all three. Any divergence is an architecture-memo-violation and must be triaged before extending the mesh.

#### Why T+7d, not T+90d, not T+1d

- T+1d catches process-level crashes but not slow leaks or upstream-correlated faults.
- T+7d catches one full weekly cycle of upstream load, two full daily compaction cycles in RocksDB, multiple network blip events, and the longest sustained nginx-failover patterns observed in `reth-blue-green-ipfs-architecture.mdc`.
- T+90d would be the right target for a hard fork; for a non-forking, opt-in advisory channel (read-only, never affects consensus), it is overkill. The Foundation can re-arm the timer for T+90d at any time by editing `OnCalendar` if the operational appetite changes.

## 7. Build provenance

The binary deployed today was built on the canary itself (no cross-host glibc mismatch) from a minimal build root containing:

- `crates/rope-core/` (32 MB source, including the §6.1.1 `knot_hash` module added 2026-05-07)
- `crates/rope-shadow-witness/` (~50 KB source, written 2026-05-09)
- A trimmed workspace `Cargo.toml` with only the two crates as members and the minimal set of workspace dependencies

Build time on canary hardware (3.8 GB RAM + 4 GB swap, 78 GB disk): 21 m 28 s for cold release build. Subsequent rebuilds on the same host take seconds.

## 8. The non-deployed hardenings (for the next phase only)

These are not in scope for the canary and will be evaluated only after the 7-day soak succeeds:

- IPFS publication of the v2 chain Merkle root (per architecture memo §3.1.2)
- `v2_anchor` knots written into the canonical chain (per architecture memo §3.1.3)
- IPNS path namespace for cross-witness agreement (per architecture memo §7.3)
- Migration from RPC poll to in-process subscription via `LedgerLifecycleEvent::EntryAppended` (would require rope-node redeployment and is therefore out of scope for the non-forking canary)

Note: promotion to GREEN and BLUE is no longer in this list — it is now §6.5 (automated, gated, kill-switchable).

---

*This runbook is the operational complement to the architecture memo. It documents the actual state of the canary and the procedures to inspect, verify, and roll it back.*
