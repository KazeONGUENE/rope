# Writer Promote Runbook + Tier D Roadmap (2026-08-23)

**Status:** DRAFT. Runbook is ops-ready but has not been executed in anger. Tier D is the root-cause project; this document is a planning handover, not a deploy playbook.
**Author:** Datachain Rope agent (rope-vps operator context)
**Trigger:** operator asked "BLUE just wedged again. When BLUE is down, shouldn't we switch to GREEN, DO1 or DO2?" (2026-08-23, post-Tier-E deploy)

---

## 0. TL;DR

- **Reads and WebSocket already fail over** (Tier A + Tier B, deployed 2026-08-23). BLUE wedging does NOT take dcscan reads or Chainlist WS offline.
- **Writes cannot fail over automatically today**, because only BLUE has the sealer key unlocked + is the only node producing anchor knots. Attesters (GREEN, DO-1, DO-2) run as sync-only followers.
- **Two independent tracks solve this:**
 1. **Track 1 (immediate):** manual writer-promote runbook (§2 below) - ops action when BLUE is wedged for > 5 min.
 2. **Track 2 (root cause):** ORIGINAL claim was "Tier D - Quipu Canon v2.0 Phase 1 (sharded lattice + per-wallet head lock)". **CORRECTION 2026-08-23:** Phase 1 is already deployed in the current binary. The actual root cause is memory pressure / swap thrash on a 7.7 GB VPS. See `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` for evidence and mitigation menu (recommended fix: VPS upgrade to 16 GB RAM; stopgap: `MemorySwapMax=0` drop-in).
- **`digitalocean_rpc` upstream is dead code** in both `datachain.network.conf` (defined line 97, referenced by nothing) and `rope.network.conf` (referenced only in a stale comment). Cleaning it up is safe but requires an nginx reload - deferred to a low-risk maintenance window.

---

## 1. Current failover topology (post-Tier-A/B, verified 2026-08-23)

Nginx upstream layout on `rope-vps:/opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf`:

| Traffic class | Upstream | Members | Failover behaviour | Reason |
|---|---|---|---|---|
| Writes | `rpc_primary_only` | BLUE only (`host.docker.internal:8545`) | **None** - 502 to caller when BLUE is down | Attesters cannot seal; a dual-writer window would fork the chain |
| Reads (JSON-RPC) | `rpc_read_failover` | BLUE (primary) → GREEN → DO-1 → DO-2 (backups) | Automatic on `error/timeout/http_502/http_503` via `proxy_next_upstream` (Tier A) | Reads are idempotent, no risk of divergence |
| WebSocket | `rope_ws` | BLUE (primary) → GREEN → DO-1 → DO-2 (backups) | Automatic (Tier B) | Same as reads; subscriptions reconnect on backup |
| `/v1/read` public read | `rpc_attesters_only` | GREEN → DO-1 → DO-2 (no BLUE) | Round-robin over attesters | Deliberately excludes BLUE to protect writer isolation |
| `/v1/mesh/edge-probe` | direct proxy to `:9109` | rope-mesh service | N/A | Local service |
| `/v1/cerber/*` | direct proxy to `:9107` | rope-cerber service | N/A | Local service |
| **Dead code** | `digitalocean_rpc` | BLUE only | (unused since Tier A) | Left over from pre-Tier-A/B design |

**Consequences observed today** when BLUE wedges (**corrected root cause**: memory pressure / swap thrash on the 7.7 GB VPS, ~7-8 min MTBF as of 2026-08-23; see MTBF postmortem doc for evidence):

| Surface | Symptom during a wedge | Recovery time |
|---|---|---|
| `dcscan.io` (reads) | Slower first byte (~200-500 ms), then recovers via GREEN | Immediate |
| `ws.datachain.network` | Subscribers may drop and reconnect to GREEN | 1-3 s |
| `erpc.datachain.network` writes (dcswap, tanastok, users) | HTTP 502 or ETIMEDOUT until watchdog restarts BLUE | 30 s - 5 min |
| `/v1/fleet-status` | Continues serving (published by attester HA script, not BLUE) | Continuous |
| Sealing | Stalls; new anchor knots pause until BLUE recovers | Same as above |

The write-path 502s are the operator-visible pain. Tier E capped ghost-reclaim amplification but did not fix the underlying wedge cause.

---

## 2. Writer promote runbook (manual, ops-only)

**When to use this runbook:** BLUE has been wedged (systemd Restart looping, or accepts TCP but times out on `eth_blockNumber`) for **> 5 minutes** AND the operator has verified via `journalctl -u datachain-rope` that the wedge is not a transient recoverable state.

**Do NOT run this runbook** if:
- BLUE is recovering on its own (watch `curl -sS https://erpc.datachain.network/v1/fleet-status` for `writer.status: healthy`).
- The ghost-reclaim rate limiter is actively backing off (Tier E - `journalctl -t erpc-fleet-ha` shows `SKIP grace=…`).
- You do not have the sealer key material available on the target promotion node.

### 2.1 Preflight (5 min)

```bash
# 1. Confirm BLUE is actually wedged (not just slow)
ssh rope-vps 'systemctl status datachain-rope --no-pager | head -20'
ssh rope-vps 'curl -s --max-time 3 http://127.0.0.1:8545 -X POST -H "content-type: application/json" \
 -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" | jq -r .result'

# 2. Confirm GREEN is at head-1 (sync gap ≤ 5 blocks is acceptable)
ssh rope-vps 'curl -s --max-time 3 http://92.243.25.119:8545 -X POST -H "content-type: application/json" \
 -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" | jq -r .result'

# 3. Confirm sealer key material is available on GREEN
# (Should be pre-staged during blue-green setup. If not, ABORT - cannot promote.)
ssh anvil-vps 'test -f /opt/datachain-rope/data/sealer.key && echo READY || echo MISSING'
```

If preflight step 3 returns `MISSING`, stop here. The sealer key must be transported to GREEN out-of-band (physical carry, encrypted email + operator decrypt) before any promote can happen. This is a **deliberate air-gap** - key transport is not automated.

### 2.2 Fence BLUE (30 s)

```bash
# Stop BLUE's sealing loop cleanly. datachain-rope.service is systemd-managed.
ssh rope-vps 'sudo systemctl stop datachain-rope.service'

# Confirm BLUE is no longer producing blocks (nc test on 8545 should refuse or time out)
sleep 5
ssh rope-vps 'timeout 3 bash -c "echo > /dev/tcp/127.0.0.1/8545" && echo "still up" || echo "fenced"'
```

**Critical:** Do not skip the fence. If BLUE recovers on its own after GREEN starts sealing, the network forks. The watchdog timer that normally restarts BLUE MUST be disabled before this step in a production runbook - add:

```bash
ssh rope-vps 'sudo systemctl mask datachain-rope.service'
```

Unmask only after promote is fully verified and the old sealer key is destroyed.

### 2.3 Promote GREEN (2 min)

```bash
# 1. Enable sealer mode on GREEN
ssh anvil-vps 'sudo systemctl edit datachain-rope.service' # add --enable-mining + keystore path
# Or edit /etc/systemd/system/datachain-rope.service.d/50-sealer.conf directly:
# [Service]
# Environment="ROPE_ENABLE_MINING=1"
# Environment="ROPE_SEALER_KEYSTORE=/opt/datachain-rope/data/sealer.key"

ssh anvil-vps 'sudo systemctl daemon-reload && sudo systemctl restart datachain-rope.service'

# 2. Verify GREEN seals a new block
sleep 15
ssh rope-vps 'curl -s --max-time 3 http://92.243.25.119:8545 -X POST -H "content-type: application/json" \
 -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" | jq -r .result'
# Should advance every ~2s. If not, ABORT (revert to §2.5 rollback).
```

### 2.4 Repoint nginx (10 s)

```bash
# Edit /opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf on rope-vps
# Change lines 39-41:
# upstream rpc_primary_only {
# server host.docker.internal:8545; # BLUE
# }
# to:
# upstream rpc_primary_only {
# server 92.243.25.119:8545; # GREEN (promoted 2026-XX-XX)
# }
# Also change rpc_read_failover (line 65) primary from BLUE to GREEN.

ssh rope-vps 'sudo docker exec rope-nginx nginx -t' # verify
ssh rope-vps 'sudo docker exec rope-nginx nginx -s reload' # zero-downtime reload

# Verify writes now succeed
curl -s --max-time 5 https://erpc.datachain.network -X POST -H "content-type: application/json" \
 -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq -r .result
```

### 2.5 Announce + destroy old key (5 min)

```bash
# 1. Anchor a WriterPromoteEvent knot on the governance ledger (0x…d002)
# via the loopback bypass on GREEN (since GREEN is the new sealer):
ssh anvil-vps 'curl -s -X POST http://127.0.0.1:8545 -H "content-type: application/json" \
 -d "{\"jsonrpc\":\"2.0\",\"method\":\"rope_appendToLedger\",
 \"params\":[\"0x000000000000000000000000000000000000d002\",
 {\"interaction_type\":\"WriterPromoteEvent\",
 \"description\":\"Sealer promoted from BLUE (rope-vps) to GREEN (anvil-vps) after $DURATION wedge\",
 \"metadata\":{\"from\":\"rope-vps\",\"to\":\"anvil-vps\",\"reason\":\"...\",\"operator\":\"...\"}}],
 \"id\":1}"'

# 2. Update fleet-status to reflect new writer
ssh rope-vps 'sudo /opt/datachain-rope/scripts/erpc-fleet-ha.sh --publish-only'

# 3. Destroy the sealer key on BLUE (or move it out-of-band to cold storage)
ssh rope-vps 'sudo shred -u /opt/datachain-rope/data/sealer.key'

# 4. Notify partners (DCSwap, Tanastok, wallet operators) via handover
```

### 2.6 Rollback (if promote fails)

```bash
# Revert nginx to BLUE
ssh rope-vps 'sudo cp /opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf.pre-promote-$TS \
 /opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf'
ssh rope-vps 'sudo docker exec rope-nginx nginx -s reload'

# Un-fence BLUE
ssh rope-vps 'sudo systemctl unmask datachain-rope.service && sudo systemctl start datachain-rope.service'

# Confirm BLUE is sealing again
sleep 10
ssh rope-vps 'curl -s http://127.0.0.1:8545 -X POST -H "content-type: application/json" \
 -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}"'
```

### 2.7 Why this is a manual runbook, not an automation

Automating promote is dangerous for four reasons:

1. **Wedge detection is noisy.** The Tier E rate limiter exists precisely because ghost-reclaim mis-detected wedges 8,328 times in one day. An auto-promote wired to the same signal would flap the sealer.
2. **Fence-then-promote sequencing must be sequential + verified.** A parallel automation could fail after fence but before promote, leaving the chain with no writer.
3. **Sealer key transport is deliberately air-gapped.** Automating key movement across VPSes weakens the security model.
4. **Every promote leaves an audit-worthy on-chain event.** A human operator writes a better `WriterPromoteEvent.description` than any automation.

We revisit automation only if promote frequency exceeds 1/week for 3 consecutive weeks - a threshold Tier D should preclude.

---

## 3. Tier D - the root-cause fix (SUPERSEDED, see correction footnote below)

> **CORRECTION 2026-08-23 (after publish):** live forensic evidence gathered ~2h after this runbook was drafted shows the actual root cause of BLUE's MTBF regression is **memory pressure / swap thrash**, not `LamportClock` contention. Phase 1 of Quipu Canon v2.0 (sharded lattice + per-wallet head lock + per-shard HLC) is **already deployed in the current binary** (build 2026-08-12) and is not the missing piece. rope-node steady-state RSS is 4 GB on a 7.7 GB VPS with 5.5 GB of active swap and 40% `iowait`. See `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` for the corrected diagnosis, evidence table, and mitigation menu (VPS upgrade to 16 GB RAM is the recommended fix; `MemorySwapMax=0` drop-in is the operator-approved stopgap). §§1-2 of this runbook (writer promote procedure, why writes can't auto-failover) remain 100% valid; §§3.1-3.5 below describe a hypothesis that the evidence now contradicts, and are kept for historical fidelity only.

### 3.1 Why BLUE wedges

`crates/rope-core/src/clock.rs` uses a global `parking_lot::Mutex<LamportClock>`. Every knot append acquires this mutex. Under sustained load (currently ~1.5-4K knot/sec sustained per the v2 architecture spec), the mutex becomes contended, and a lock-ordering bug (documented in `QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §"Root cause") deadlocks the sealer thread against the mempool ingestion thread.

Ghost-reclaim (Tier E context) added ~2 additional pokes per HA cycle, amplifying contention. Tier E capped that amplification but did not remove the mutex.

### 3.2 Phase 1 fix (sharded lattice + per-wallet head lock)

Per `QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §"Phase 1":

- Global `LamportClock` mutex is replaced with a **sharded clock** (default 256 shards, keyed by wallet-address prefix).
- `StringLattice::add_string` cascading `RwLock`s are collapsed into a **per-wallet head lock** - one `RwLock` per shard, one write-critical-section per knot.
- `OES::derive_key` is **cached** (100-199 BLAKE3 rounds per call → one hit per wallet per epoch).
- Storage is moved from in-memory to **RocksDB-backed persistence** (already done in Quipu Canon v2.0 Phase 1.6, per `handover-security-audit-2026-06-11.mdc` §doc-drift-correction).

Expected impact per the architecture spec:

| Metric | Today | Post-Phase-1 |
|---|---|---|
| Per-node sustained TPS | 1.5-4 K knot/sec | 50-100 K knot/sec |
| BLUE MTBF | ~7-8 min | Not measured, expected > 30 min (July 2026 baseline) |
| Writer promote frequency | ~1/hour worst case | Rare (unplanned event) |

### 3.3 Why Tier D is a project, not a task

Phase 1 requires:

1. **Validator coordination.** Phase 1 changes the knot schema versioning byte (v2 → v3 via `knot_hash_version` field per `quipu-canon-knot-hash-construction.mdc`). All 4 validators must run the Phase 1 binary simultaneously; a mixed-version fleet would fork.
2. **Soak testing.** `tools/rope-loadgen/` methodology from the v2 architecture spec (30-min sustained, p99 ≤ 1s, zero knot loss, untie path exercised at 1%, durability survives node kill). This must pass on a dedicated testnet before mainnet.
3. **Blue-green rollout.** BLUE and GREEN must run identical Phase 1 binaries with a coordinated cutover. This is the same operational shape as the Reth blue-green migration (2026-03-12), which took ~2 weeks including staging.
4. **Ecosystem notification.** DCSwap, Tanastok, Datawallet+, agents must all be running SDK versions that understand the new versioning byte (v1.2 emitters continue to work through Phase 3 per the spec, so this is compatibility-preserving, but partners should be told).
5. **Rollback plan.** If Phase 1 destabilizes, revert to v1.2 binary + drain new-shape knots via chain read. This is straightforward but requires validated tooling.

### 3.4 Estimated effort

Per the v2 architecture spec: **3 engineering-months** for Phase 1 alone. Codebase has Phases 1-5 CODE-COMPLETE (per `quipu-canon-v2-roadmap-5m-tps.mdc` and `QUIPU_CANON_V2_PHASE_STATUS_2026-07-06.md`); what remains is fleet deploy + validator expansion 6 → 21 + soak.

### 3.5 Decision gates

Before scheduling Tier D:

- [ ] Confirm the code in `datachain-rope/crates/rope-core/src/{clock,lattice}.rs` matches the Phase 1 design (last verified 2026-07-06).
- [ ] Confirm the RocksDB-backed `LedgerStore` is production-tested (last verified 2026-06-12).
- [ ] Confirm `master-nodes.toml` roster is coordinated with all 4 validators.
- [ ] Confirm ecosystem partners have received a deploy-window handover ≥ 14 days ahead.
- [ ] Confirm rollback binary is staged on rope-vps + anvil-vps + DO-1 + DO-2.
- [ ] Book a 4-hour maintenance window with all validator operators.

A dedicated `handover-quipu-canon-v2-phase1-deploy-YYYY-MM-DD.mdc` is drafted only after all gates above are green.

---

## 4. What the operator does NOT need to do today

- **Not run §2.** Reads and WebSockets already fail over automatically; write-path 502s during a hang cycle recover within ~34-45s via the external HA restart. See MTBF postmortem for the corrected cadence expectation (once a memory fix is in place, MTBF should return to July 2026 baseline). Watch fleet-status for a week after any memory mitigation lands.
- **Not touch `digitalocean_rpc`.** It's dead code but harmless. Cleanup can wait for the next nginx maintenance window.
- **Not accelerate Tier D as a MTBF fix.** Phase 1 is already deployed and is not the missing piece. Prioritize the memory mitigation menu in `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` first, then re-measure whether the rest of the v2.0 roadmap (Phase 2/3/4) is needed.

## 5. Cross-references

- `docs/BLUE_NEVER_HANGDOWN_ALTERNATIVES_2026-08-23.md` - **alternatives menu** for the "BLUE should never hangdown" directive. Includes auto-writer-promote with fencing (B1), which is the automated version of this manual runbook. Once B1 lands, §§1-2 of this doc become a rollback path rather than a first-response procedure.
- `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` - **CORRECTED root-cause diagnosis + mitigation menu** (published 2026-08-23, ~2h after this runbook)
- `datachain-rope/docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` - v2.0 architecture (Phase 1-5 code-complete; Phase 1 confirmed deployed on BLUE)
- `datachain-rope/docs/QUIPU_CANON_V2_PHASE_STATUS_2026-07-06.md` - code-complete status per phase
- `datachain-rope/docs/QUIPU_CANON_V2_PHASE1_BENCHMARK_RESULTS.md` - Phase 1 soak results
- `.cursor/rules/quipu-canon-v2-roadmap-5m-tps.mdc` - always-applied roadmap summary
- `handover-security-audit-2026-06-11.mdc` §doc-drift-correction - RocksDB persistence
- Tier A + Tier B: read/WS failover deploys (2026-08-23)
- Tier E: `erpc-fleet-ha.sh` ghost-reclaim rate limit (2026-08-23)

---

*This runbook exists to bound the operator-visible impact of BLUE wedges. It is not a substitute for the root-cause fix. Per the 2026-08-23 postmortem, the current root cause is memory pressure on the 7.7 GB VPS, not the LamportClock lock-ordering originally hypothesized. If BLUE wedges become weekly-or-more-frequent even after the memory mitigation, revisit Tier D Phase 2/3/4; do not automate this runbook.*
