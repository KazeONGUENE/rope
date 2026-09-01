# Design — `rope-testnet-writer-facade` (Phase 2)

**Author:** Datachain Rope agent
**Date:** 2026-08-30
**Status:** DESIGN — not implemented, not scheduled for this session.
**Goal:** put a `rope-node` writer facade in front of the testnet EVM engine on loopback, so the testnet inherits the same method firewall + signed-write allowlist as mainnet, and remove the last public path that talks to the execution client directly.

---

## 0. TL;DR

Today the testnet public surface is:

```
Internet ──HTTPS──▶ nginx on new-blue (testnet.erpc.datachain.network / faucet.datachain.network/rpc)
                     │
                     ▼
              rope-testnet-faucet on new-blue (node.js, :3100)
                     │  method allowlist + web3_clientVersion masking
                     ▼
              rope-testnet-engine on new-blue (reth, 127.0.0.1:8598)
                          chainId 271829, --dev
```

The faucet backend is doing a job it wasn't designed for (JSON-RPC firewalling), the testnet is co-hosted with the mainnet writer on `new-blue` (memory pressure, blast radius), and the testnet reth already burns a shifted port (`:8598` vs. mainnet's `:8595`) to work around the co-hosting. Mainnet does the firewall properly with a `rope-node` process on `:8545` that (a) enforces `DESTRUCTIVE_METHODS` deny + Phase-2 signature gate in Rust (`crates/rope-node/src/rpc_auth.rs`), and (b) delegates EVM-anchored `eth_*` methods to Reth on private `:8595` via `[evm_backend]`. Phase 2 lifts the same shape onto the testnet **on a dedicated DigitalOcean droplet** (`rope-testnet-1`, lon1, s-2vcpu-4gb, Ubuntu 24.04), which lets the testnet use natural mainnet-style ports without collisions and isolates it from mainnet blast radius:

```
Internet ──HTTPS──▶ nginx on new-blue           (TLS termination stays where the certs are)
                     │
                     ▼
              rope-testnet-1.dcrope.internal    (private-network hop, DigitalOcean lon1 VPC)
                     │
       ┌─────────────┴──────────────┐
       │                            │
       ▼                            ▼
   rope-testnet-faucet          rope-testnet-node (rope, 127.0.0.1:8545)  ← NEW
     (node.js, :3100)                │  DESTRUCTIVE_METHODS deny (same code, same enum)
     ONLY /api/drip +                │  Phase-2 signature gate (loopback bypass only for
     /healthz + /api/status          │  the faucet's drip signer, same rule as mainnet)
     (drip flow keeps its own        ▼
      signer + rate limits)     rope-testnet-engine (reth, 127.0.0.1:8595)
                                     chainId 271829, --dev
```

Success = every mutating `rope_*` call on the testnet is signature-gated, `rope_untieKnot` is denied unconditionally on the public listener, `web3_clientVersion` returns the same branded string via the Rust facade instead of via a node.js hack, the only path to the execution engine goes through Rust code that is bit-identical to mainnet, and the testnet lives on its own box so a Chainlist-driven abuse wave cannot swap-thrash the mainnet writer.

---

## 1. Why this is worth doing (given the testnet is "just a playground")

1. **Testing the mainnet gate on a live network.** Today `DESTRUCTIVE_METHODS` and the Phase-2 verifier are only exercised against mainnet, where mistakes are catastrophic. A testnet facade lets us fuzz the gate, run signature-scheme migrations, and rehearse Canon revisions on a live network first.
2. **Kill one class of "how do I test my untie flow?" tickets.** External developers currently cannot exercise the destructive-RPC path from the testnet because there's nothing to exercise — the testnet talks raw reth, which knows nothing about `rope_untieKnot`. Adding the facade makes the testnet a real drop-in for mainnet integration testing.
3. **Remove the last "faucet backend is doing security" seam.** The `sanitizeUpstreamResponse` hack in `server.mjs` that rewrites `reth/*` strings out of batch responses was a load-bearing patch, not a design. Once `rope-testnet-node` fronts the engine, no reth version string is ever visible to the faucet or nginx — it stops being a security concern that has to be patched at every layer.
4. **Cheap parity with mainnet, but not on the mainnet box.** The mainnet writer facade is ~200 MB RSS and idles at <2% of one CPU. Co-hosting a second instance on `new-blue` looked cheap — until the port audit found five collisions with mainnet reth / testnet reth / compliance-agent / cluster p2p and forced an amended-ports scheme that would have added permanent operational surface area. The `handover-mtbf-postmortem-swap-thrash-2026-08-23.mdc` postmortem also flagged that `new-blue` is already close to its 8 GB ceiling. A dedicated s-2vcpu-4gb DigitalOcean droplet (`rope-testnet-1`, ~$24/month) buys us natural ports, zero co-hosting risk, and complete blast-radius isolation from mainnet.
5. **Chainlist listing (`chainlist-271829`) will send a lot more traffic at the testnet.** Once the chainlist PR lands, we should expect wallet on-boarding, contract-deploy walkthroughs, and abuse probes. Landing the facade before that traffic hits is cheaper than landing it after the first "testnet leaked reth" report. Landing it on a dedicated box means an abuse wave against the testnet cannot swap-thrash the mainnet writer — a real risk given the mainnet MTBF regression pattern from `handover-mtbf-postmortem-swap-thrash-2026-08-23.mdc`.

---

## 2. Scope

### In scope

- **Dedicated DigitalOcean droplet `rope-testnet-1`** (lon1, s-2vcpu-4gb, Ubuntu 24.04). Provisioned with the existing DigitalOcean SSH key (same one that manages `new-blue`, `rope-vps`, and the DO rpc-1/rpc-2 attesters). Firewall: `22/tcp` (SSH from operator + `new-blue` for automation), `9000/tcp` (libp2p, unused today but reserved), and `443/tcp` (only if TLS eventually terminates on the box — for the initial rollout TLS stays on `new-blue`, see below).
- One new systemd unit `rope-testnet-node.service` **on `rope-testnet-1`** running `rope node --network testnet --mode relay` against a testnet-only config.
- A testnet copy of the mainnet writer config (`deploy/config/rope-testnet.toml`) that (a) sets `chain_id = 271829`, (b) points `[evm_backend]` at `http://127.0.0.1:8595` (the migrated `rope-testnet-engine` reth on `rope-testnet-1`), (c) sets the public HTTP listener to `127.0.0.1:8545` (natural mainnet-style port, no collision because this is a dedicated box), (d) sets the WS listener to `127.0.0.1:8546`, gRPC to `127.0.0.1:9001`, metrics to `127.0.0.1:9090`, libp2p to `0.0.0.0:9000`, and (e) sets `[phase2_signed_destructive]` to `true` from day one (the testnet has no legacy operators to migrate).
- **Migrate `rope-testnet-engine.service` and `rope-testnet-faucet.service` off `new-blue` onto `rope-testnet-1`.** The engine gets natural `--http.port 8595` (mainnet-style) because there is no reth on this box to collide with. The faucet keeps its natural `:3100`. Both services stay running on `new-blue` during the migration window as the immediate rollback target — see §5.
- Nginx changes on `new-blue` to route the `testnet.erpc.datachain.network` and `faucet.datachain.network/rpc` locations to `http://<rope-testnet-1 private IP>:8545` **instead of** `http://172.18.0.1:3100/rpc`. The faucet's `/api/drip`, `/api/status`, `/healthz` paths route to `http://<rope-testnet-1 private IP>:3100`. TLS certificates stay on `new-blue` for the initial rollout, so DNS is untouched and there is no certbot workflow to change on day one. A follow-up (post-soak) can move TLS onto `rope-testnet-1` if we decide to fully decouple.
- `rope-testnet-faucet.service` (on `rope-testnet-1`) loses the pass-through JSON-RPC surface but keeps the drip flow. Its own signer keeps calling the engine via `FAUCET_RPC_URL` — either it points to `http://127.0.0.1:8595` directly (bypasses the facade, simpler) or it points to `http://127.0.0.1:8545` and inherits the gate. Both are viable, see §4.4.
- `rpc_router.js` (nginx njs on `new-blue`) gets a testnet code path so read failover semantics on the testnet mirror mainnet's — with the difference that the testnet has one node, so failover degrades to "return 503 if the single writer is down" instead of "try the next attester". A future follow-up can add a second DO droplet if we want testnet read-failover, but that's out of scope here.

### Out of scope for this design

- **Multi-node testnet.** The testnet stays single-node until there's a demand-driven reason for a committee. The facade design is single-writer, single-engine.
- **DCR-20 minter / bridge minter on testnet.** Not needed for the developer-experience use case that motivates this.
- **Testnet CERBER.** Mainnet CERBER already probes `testnet.erpc.datachain.network` health via `cerber-docs-drift.service`. That is sufficient. Once `rope-testnet-1` is stable we can add a mesh peer identity for it, but it's not blocking.
- **Phase 5 offload (GPU/ASIC PQ signing) on testnet.** Not in scope; the testnet is a functional-parity target, not a scale target.
- **Moving TLS termination to `rope-testnet-1`.** Initial rollout keeps TLS on `new-blue` (no cert / DNS change). A follow-up can move TLS to `rope-testnet-1` and cut out the `new-blue → rope-testnet-1` proxy hop.

---

## 3. Sequencing (implementation order that keeps testnet green throughout)

### Phase A — source + provisioning (no user-visible change)

1. **Land `deploy/config/rope-testnet.toml`** (source-only change, no deploy). The config uses natural mainnet-style ports (`http_addr=127.0.0.1:8545`, `ws=8546`, `grpc=9001`, `metrics=127.0.0.1:9090`, `libp2p=0.0.0.0:9000`, `evm_backend.url=http://127.0.0.1:8595`, `db_path=<data_dir>/testnet/ledger_db`) because this file will be consumed only on the dedicated testnet box.
2. **Add `--network testnet` to the `rope node` command-line wiring.** Today `rope` only knows about mainnet. This is a small addition to `rope-cli/src/main.rs` — parse the flag, route to `deploy/config/rope-testnet.toml` by default when set, gate any mainnet-only features (e.g. the Founder Ed25519 registry check in `rpc_signature.rs`) behind a feature flag or a matched `chain_id`. **Landed in Phase 0** (see `docs/design/testnet-parity-roadmap-2026-08-30.md` §2 for the completed prerequisite fixes).
3. **Build `rope-cli` on new-blue** with the new flag. Verify the mainnet binary is still bit-identical (or a superset). Verify a `rope node --network testnet --dry-run --config ...` succeeds without opening any port. This step also produces the binary we'll scp to `rope-testnet-1` — same Ubuntu-24.04 / glibc-2.39 base, so no cross-build issue.
4. **Provision `rope-testnet-1`** on DigitalOcean lon1 (s-2vcpu-4gb, Ubuntu 24.04). Install the DigitalOcean SSH key. Register hostname `rope-testnet-1.dcrope.internal` in the operator's `~/.ssh/config`. Firewall via `ufw`:
   - `22/tcp` from operator + `new-blue` public IP (for `ssh` + rsync).
   - `9000/tcp` reserved for libp2p (no peers today).
   - All other ports firewalled. Nginx on `new-blue` reaches `:8545`/`:3100` over the DigitalOcean **private-network interface** (no public :8545 anywhere).

### Phase B — install testnet stack on `rope-testnet-1`

5. **Install reth** on `rope-testnet-1` as `rope-testnet-engine.service` with the same genesis / dev flags as today's `new-blue` deployment, but on natural `--http.port 8595`. Verify `curl http://127.0.0.1:8595 -X POST -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId"}'` returns `0x425d5` (271829).
6. **Install the faucet** on `rope-testnet-1` as `rope-testnet-faucet.service` on `127.0.0.1:3100`. Point `FAUCET_RPC_URL` at `http://127.0.0.1:8595` initially (bypasses the facade — see §4.4 option A; we'll flip to option B in step 9 after the facade is proven).
7. **Stage `rope-testnet-node.service`** on `rope-testnet-1` from the Phase-A binary, with `WantedBy=` cleared (i.e. not enabled). `systemctl start rope-testnet-node.service` manually. Verify from loopback:
   - `curl -sS -X POST http://127.0.0.1:8545 -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'` → `0x425d5` (271829).
   - `curl -sS -X POST http://127.0.0.1:8545 -d '{"jsonrpc":"2.0","id":1,"method":"rope_untieKnot","params":[…]}'` from a simulated public source (add `X-Forwarded-For` header) → `code: -32401`, per the mainnet Phase-1 gate.
   - `curl -sS -X POST http://127.0.0.1:8545 -d '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}'` → the branded `Datachain-Rope/*` string. Verify NO `reth/*` substring is in the response.
   - Signed Phase-2 destructive call (using the chain-scoped Phase-0 domain tag for chainId 271829) → succeeds. This is the acceptance criterion for the facade being production-ready on testnet.

### Phase C — cut public traffic over (canary + flip)

8. **Nginx flip (canary) on `new-blue`:** add a new location `= /rpc.next` on `testnet.erpc.datachain.network` that routes to `http://<rope-testnet-1 private IP>:8545`. `curl` from external, verify parity with the current `/rpc` (which still routes to `new-blue`'s local `:3100/rpc` pass-through). Once green for ≥1h — same `eth_chainId`, same `eth_blockNumber` progression, same `rope_globalStats.total_knots`, matching `web3_clientVersion` — swap `/` and `/rpc` to point at the new upstream. Keep the old target as a commented backup line for immediate rollback.
9. **Faucet backend cleanup** on `rope-testnet-1`: remove `sanitizeUpstreamResponse`, remove the `web3_clientVersion` intercept. Repoint `FAUCET_RPC_URL` to `http://127.0.0.1:8545` (through the facade). Verify a full drip E2E: `curl -X POST https://faucet.datachain.network/api/drip -d '{"address":"0x…"}'` returns a valid txHash, block explorer shows the tx mined on chainId 271829. Update `handover-testnet-erpc-endpoint-and-rope-naming-2026-08-30.mdc` to reflect the change.
10. **Enable `rope-testnet-node.service`** on `rope-testnet-1` so it starts on boot. Verify it comes up correctly across a full reboot (rope-testnet-engine → rope-testnet-node → rope-testnet-faucet, in that order via `After=` in the unit file).
11. **Publish** and update the drift monitor's expectations so it knows the testnet writer identity is now `rope-testnet-node` on `rope-testnet-1`, not the faucet on `new-blue`.

### Phase D — decommission on `new-blue`

12. **Soak for one clean week** with public traffic hitting `rope-testnet-1`. Watch nginx `access.log` on `new-blue` (proxy path), `journalctl -u rope-testnet-node.service` on `rope-testnet-1`, and CERBER `cerber-docs-drift.service` for regressions.
13. **Decommission `rope-testnet-engine.service` and `rope-testnet-faucet.service` on `new-blue`.** Back up their binaries + configs + reth data to `~/backup-testnet-on-new-blue-2026-XX-XX/`. `systemctl stop && systemctl disable` both units. `ufw deny` any testnet-related inbound rules on `new-blue`. This frees up memory + disk on `new-blue` for the mainnet writer — the primary reason we picked the dedicated-host route.

The Chainlist PR (§2 in `docs/design/testnet-parity-roadmap-2026-08-30.md`) opens only after step 13 is green.

---

## 4. Design decisions

### 4.1 Should the testnet writer share the mainnet writer's binary?

**Yes, same binary, different `--network` flag.** Divergence between mainnet and testnet binaries is the number-one cause of "we tested it on testnet, it broke on mainnet" incidents in every ecosystem that has both. The tolerable cost is that mainnet-only behaviour (Founder Ed25519 registry, invariant monitors, ecosystem-agent hard-coded wallets) has to be either (a) gated on `chain_id` at runtime or (b) empty for the testnet config. Both are cheap.

### 4.2 Should the testnet listener be `:8545` (same as mainnet) or a different port?

**Natural mainnet-style port `:8545` on the dedicated testnet box.** Rationale, updated 2026-08-31 after the dedicated-host decision:

1. **The testnet runs on its own dedicated DigitalOcean droplet (`rope-testnet-1`).** There is no mainnet reth or mainnet rope-node on that host, so `:8545` cannot collide with anything. Choosing a natural port keeps every ops script uniform: `curl http://127.0.0.1:8545` works the same way on `rope-testnet-1` as it does on `new-blue`, so an operator who's used to the mainnet box has zero cognitive load switching.
2. **Nginx routing on `new-blue` is unambiguous by upstream address, not by port.** The testnet vhost proxies to `http://<rope-testnet-1 private IP>:8545`; the mainnet vhost proxies to `http://172.17.0.1:8545` (loopback into the docker bridge, mainnet node). Different upstream IPs — different chains. Same port on both, but on physically different boxes. `nginx -T` output stays clear because the upstream IP tells you which chain.
3. **Prior port-shifting was only a bandaid for the never-executed co-location plan.** An earlier revision of this design proposed `:8549`/`:8550`/`:9012`/`:9093` to avoid collisions with mainnet if we ever multiplexed. The dedicated-host decision (see §1) removed that requirement entirely, so we reverted to natural ports and locked the choice with a unit test in `crates/rope-node/src/config.rs` (`testnet_config_uses_natural_ports_and_disabled_consensus`).

### 4.3 Should Phase-2 signed destructive be on from day one?

**Yes.** The testnet has no legacy operators to protect. Turning Phase-2 on from day one means:

- Every developer writing against the testnet learns the signed-write pattern immediately.
- The gate itself gets exercise. Bugs in the domain-separated pre-image or the ±window enforcement surface on the testnet, not on mainnet.
- The mainnet migration story becomes "flip the same flag on mainnet" rather than "design a new flag and hope it works". Cleaner audit story.

### 4.4 Where does the faucet's own signer talk?

Two options (both on `rope-testnet-1`, same box as the facade):

- **A. Faucet keeps `FAUCET_RPC_URL=http://127.0.0.1:8595`** (talks directly to reth on natural mainnet-style port, bypasses the facade). Simpler; the faucet's transactions are internal and don't need to be gated. But this creates two paths to the engine, which is exactly what we're trying to eliminate.
- **B. Faucet's `FAUCET_RPC_URL=http://127.0.0.1:8545`** (talks through the facade). Single path to the engine. But we then have to whitelist the faucet's drip signer in the Phase-2 verifier OR keep `eth_sendRawTransaction` unsigned on the testnet.

**Recommend B.** The gate for `eth_*` methods on mainnet is looser than for `rope_*` methods anyway — `eth_sendRawTransaction` is signed by the tx sender, so the double-signature (once for the tx, once for the auth envelope) would be redundant. The facade should apply the Phase-2 gate ONLY to `rope_*` destructive methods, per §1 of `PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md`. No faucet-side change needed under that reading.

**Rollout note:** during Phase-B step 6 the faucet starts on option A (direct to `:8595`) so the facade can be brought up in isolation and validated independently. After the facade is proven (Phase-B step 7 acceptance criteria pass), Phase-C step 9 flips the faucet to option B. This two-step avoids a single failure mode taking down both the facade acceptance test and the drip flow simultaneously.

### 4.5 Should `--dev` stay on the engine, or switch to Testimony consensus?

**`--dev` stays.** Testimony consensus is a Phase-2 mainnet-only investment for the reason spelled out in `quipu-canon-v2-roadmap-5m-tps.mdc` §5 — it requires a committee. A single-node testnet with `--dev.block-time 3s` is exactly what developers want (deterministic 3s block time, no reorgs, no p2p noise). If we ever want testnet consensus testing, it's a separate design under Phase 3.

---

## 5. Risks and rollback

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `rope node --network testnet` binary regresses mainnet behaviour | Low | Catastrophic | Diff the compiled binary bytes against pre-flag build. Run mainnet unit tests + integration suite. Gate `--network testnet` behind a feature flag if bytes differ. **Landed in Phase 0** — see the `rope-node --lib` test suite: 129/129 passing, `mainnet_config_defaults_unchanged` proves the mainnet defaults are bit-identical to the pre-refactor build. |
| Nginx canary flip drops testnet traffic | Medium | Low (testnet is a dev surface) | `/rpc.next` canary, hold ≥1h, backup line kept commented for one-command rollback (see Phase-C step 8). |
| Faucet path stops working during the flip | Medium | Medium (public dev surface) | Two-step faucet cutover (§4.4 rollout note): the facade acceptance runs while the faucet is still on option A (direct to reth), so we prove the facade in isolation. The faucet drip logic is preserved as-is — it never touches nginx `/rpc`, it hits `:3100/api/drip` directly. Verify with a full drip E2E after each flip step. |
| Phase-2 signature enforcement surprises early testnet users | High | Low (testnet is a learning surface) | Ship a short docs note ("Destructive rope_* methods on the testnet require signed envelopes — same pattern as mainnet"). Include a `curl` example in the testnet quickstart. |
| Engine identity leaks through a code path we didn't audit | Low | Medium | Add a `web3_clientVersion` integration test to CI that fails if the response contains `reth`, `Reth`, or the linux triple. |
| `rope-testnet-1` droplet outage takes testnet offline | Low (single node) | Low (testnet is a dev surface) | Documented as expected behaviour. A follow-up can add a second DO droplet for testnet read-failover, but the single-node model is deliberate for cost and simplicity. |
| Private-network hop `new-blue → rope-testnet-1` adds latency | Low | Negligible (dev surface) | DigitalOcean lon1 intra-VPC RTT is <1ms; well within the 3s block time. Measure and record in the Phase-C canary hold to confirm. |

### Rollback path (immediate)

1. Revert the nginx canary flip on `new-blue` (uncomment the backup `proxy_pass` line pointing at local `:3100`).
2. On `rope-testnet-1`: `sudo systemctl stop rope-testnet-node.service; sudo systemctl disable rope-testnet-node.service`.
3. Re-arm the faucet's `sanitizeUpstreamResponse` and `web3_clientVersion` intercept on `new-blue` (kept in git history, one-line revert). This is a rollback of the Phase-C step 9 change only — if we haven't reached step 9 yet, no rearm needed.
4. On `new-blue`: `docker exec rope-nginx nginx -s reload`.

Total blast radius: testnet only. Mainnet is completely unaffected because it runs on a different chain, a different physical box (mostly — `rope-vps` and `new-blue` handle mainnet; `rope-testnet-1` handles testnet), a different config file, and a different systemd unit. Even a catastrophic `rope-testnet-1` failure has zero mainnet impact.

---

## 6. Success criteria (green / red)

Green landing MUST show (verified after §3 Phase-D step 13):

- `curl -sS https://testnet.erpc.datachain.network -X POST -d '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}'` returns `Datachain-Rope/*`, never `reth/*` or a linux triple.
- `curl -sS https://testnet.erpc.datachain.network -X POST -d '{"jsonrpc":"2.0","id":1,"method":"rope_untieKnot","params":["0x…","0x…"]}'` returns `-32401 Method denied on public listener`.
- A correctly signed Phase-2 destructive call against `https://testnet.erpc.datachain.network` (using the chain-scoped tag for chainId 271829 landed in Phase 0) succeeds.
- `curl -sS http://127.0.0.1:8545 -X POST -d '{"jsonrpc":"2.0","id":1,"method":"rope_untieKnot","params":[…]}'` **on `rope-testnet-1`** (loopback, no X-Forwarded-For) succeeds, matching the mainnet loopback-bypass behaviour.
- `curl -sS http://127.0.0.1:8545 -X POST -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'` on `rope-testnet-1` returns `0x425d5` (271829).
- Faucet drip flow still works E2E: `curl -X POST https://faucet.datachain.network/api/drip -d '{"address":"0x…"}'` returns a valid txHash, block explorer shows the tx mined on chainId 271829.
- `cerber-docs-drift.service` green.
- `nginx -T` on `new-blue` shows exactly one `proxy_pass` target for testnet JSON-RPC (pointing at `rope-testnet-1`'s private-network IP on `:8545`), no residual reference to local `:3100/rpc`.
- `systemctl status rope-testnet-{engine,faucet}.service` on `new-blue` shows both as `disabled` and `inactive` (Phase-D step 13 complete).
- Public mainnet writer surface (`erpc.datachain.network`) is byte-identical to pre-Phase-0 — verified via a signed `rope_createPersonalLedger` for a mainnet wallet using the chain-scoped Phase-0 tag.

Red rollback triggers:

- Any of the above green criteria fails after §3 Phase-C step 10.
- Faucet drip success rate over the first hour < 95% of the pre-flip baseline.
- Any external report of a `reth/*` string leaking through the testnet.
- Mainnet-side regression detected by CERBER during Phase-A step 3 (mainnet binary bytes drift). Non-negotiable: mainnet does not tolerate collateral damage from a testnet refactor.

---

## 7. Cost estimate

### Engineering (wall time)

- Rust changes (`--network testnet` flag + config plumbing): **DONE in Phase 0** (see `docs/design/testnet-parity-roadmap-2026-08-30.md` §2).
- Testnet config file: **DONE in Phase 0**.
- DigitalOcean droplet provisioning + reth/faucet install on `rope-testnet-1`: 0.5 engineer-day.
- Facade rollout + Nginx canary flip: 0.5 engineer-day (Phase-B + Phase-C steps 5–10).
- Faucet backend cleanup (removing `sanitizeUpstreamResponse` + `web3_clientVersion` intercept): 0.5 engineer-day (Phase-C step 9).
- CI test for `web3_clientVersion` non-leakage: 0.5 engineer-day.
- Docs update (developer quickstart, drift monitor expectations, handovers, dedicated-host handover): 0.5 engineer-day.
- One clean week of soak (Phase-D step 12): calendar time only, no engineering.
- Decommission on `new-blue` (Phase-D step 13): 0.25 engineer-day.

**Total: ~2.75 engineer-days** on top of the already-landed Phase 0. Wall time ~2 weeks including the soak.

### Infrastructure (recurring)

- `rope-testnet-1` DigitalOcean droplet: **s-2vcpu-4gb Ubuntu 24.04 in lon1**, ~$24/month at current DO pricing. This is the entire recurring cost of the dedicated-host strategy.
- Freed capacity on `new-blue` (mainnet writer): reclaims ~2 GB RAM + ~50 GB disk previously used by testnet reth + faucet. Directly reduces the memory-pressure risk documented in `handover-mtbf-postmortem-swap-thrash-2026-08-23.mdc` — worth substantially more than the $24/month spend.

**Net infrastructure cost:** +$24/month for the testnet box, but removes a chunk of the mainnet-writer memory-pressure risk. The dedicated-host route is a strictly positive trade against the co-location alternative.

### Ecosystem coordination

None. Testnet users see a DNS-transparent flip (same public endpoint, same chainId, same wallet flow). No downstream project (DCSwap, Tanastok, Datawallet+, CareAway) has to change anything.

---

## 8. Cross-references

- `docs/design/testnet-parity-roadmap-2026-08-30.md` — the coordinated roadmap that ties this facade to the Chainlist submission. Phase 0 (three prerequisite Rust fixes) is landed and unit-tested. This document is Phase 1's implementation reference; the roadmap owns sequencing and ordering.
- `deploy/config/rope-production.toml` — mainnet writer config, the shape to mirror.
- `deploy/config/rope-testnet.toml` — the testnet writer config landed in Phase 0. This is the file `rope-testnet-node.service` will load on `rope-testnet-1`.
- `crates/rope-node/src/config.rs::testnet()` — the Phase-0 `NodeConfig::testnet()` factory; asserts chain-id 271829, `slip44: 1`, natural mainnet-style ports, disabled consensus (delegated to reth), and a namespaced `data/rope-testnet` DB path.
- `crates/rope-node/src/rpc_auth.rs` — the `DESTRUCTIVE_METHODS` enum and Phase-1 loopback-bypass logic. Byte-identical between mainnet and testnet.
- `crates/rope-node/src/rpc_signature.rs` — Phase-2 signed-write verifier. Phase 0 chain-scoped the domain tag (`chain_domain_tag(chain_id)`) so mainnet and testnet share code but produce non-interchangeable signatures.
- `docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md` — the spec that landed the mainnet gate; every design choice here mirrors it.
- `.cursor/rules/handover-security-audit-2026-06-11.mdc` — audit context for why the destructive-methods gate exists and why we don't remove it.
- `.cursor/rules/handover-testnet-erpc-endpoint-and-rope-naming-2026-08-30.mdc` — current testnet operational state on `new-blue` (co-located). This design supersedes both the "faucet backend does the firewall" pattern **and** the co-location topology documented there.
- `.cursor/rules/handover-milestone-2026-08-30.mdc` — records the Phase-0 landing and the pending Phase-1 rollout.
- `.cursor/rules/handover-mtbf-postmortem-swap-thrash-2026-08-23.mdc` — the memory-pressure root cause on `new-blue`; freeing testnet capacity by moving to `rope-testnet-1` is a direct positive input to that mitigation menu.
- `docs/design/chainlist-271829-submission.md` — sibling design; the Chainlist PR must not land before this facade lands and passes the Phase-1 soak (see §1 point 5 and the roadmap §3 Phase-2 gate).
