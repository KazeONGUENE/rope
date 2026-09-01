# Testnet Parity Roadmap - `rope-node` facade + Chainlist listing (2026-08-30)

**Author:** Datachain Rope agent
**Status:** Phase 0 LANDED (code-complete, `cargo test -p rope-node --lib` = 199/199 green, zero mainnet wire drift, staged in laptop tree; not yet deployed to `rope-vps`). Phase 1 (facade rollout) and Phase 2 (Chainlist PR) still queued behind the operator-gated deploy.
**Audience:** the future agent (or human operator) who will implement these two items
**Scope:** two coupled near-term roadmap items for the Datachain Rope testnet (chain id `271829`)

---

## 0. TL;DR

Two design docs already exist and are internally consistent:

- `docs/design/rope-testnet-writer-facade.md` - Phase-2 signed-write facade in front of the testnet EVM engine.
- `docs/design/chainlist-271829-submission.md` (+ `eip155-271829.json` + `chainlist-271829-pr-body.md`) - PR payload for `ethereum-lists/chains`.

They are coupled by a hard ordering constraint:

> **The facade must land in production and observe one clean week of traffic before the Chainlist PR opens.**

Reason: the Chainlist listing will attract wallet-onboarding traffic AND automated abuse probes. If those probes hit a permissive testnet directly, the first data point external developers get about our testnet is "destructive RPCs are open on the testnet, please don't rely on this network being canonical." That is a worse first impression than a one-week delay.

This roadmap document exists to (a) tie the two design docs to a single execution plan, (b) enumerate the three prerequisite fixes I found during verification, and (c) give a checklist that survives the handover from planning to implementation.

---

## 1. Live-state verification (2026-08-30)

I re-checked every claim in the two design docs against production before writing this. Findings:

| Claim | Verified | Notes |
|---|---|---|
| Chainlist mainnet 271828 is listed on `ethereum-lists/chains` | Yes | `raw.githubusercontent.com/ethereum-lists/chains/master/_data/chains/eip155-271828.json` returns 200. Also present in the `chainid.network/chains.json` CDN dump. |
| Chainlist testnet 271829 is NOT listed | Yes | Same URL for 271829 returns 404. Same absence in the CDN dump. Target repo is clear. |
| `testnet.erpc.datachain.network` serves chain 271829 | Yes | Confirmed in `.cursor/rules/handover-testnet-erpc-endpoint-and-rope-naming-2026-08-30.mdc`. `web3_clientVersion` currently masks Reth via the faucet-side `sanitizeUpstreamResponse` hack. |
| `rope-testnet-engine.service` and `rope-testnet-faucet.service` renames landed | Yes | Same handover confirms the systemd rename. `rope-testnet-node.service` (the facade) is the missing third unit. |
| `rope node --network testnet` CLI path is wired | Yes | `crates/rope-cli/src/main.rs` accepts `--network testnet` and calls into `NodeConfig::for_network`. |
| `NodeConfig::testnet()` produces the right defaults for the facade | **No** - see §2 below | Ports and EVM backend URL are inherited from `mainnet()` and collide with the running mainnet node. |
| Phase-2 signed destructive RPC verifier enforces chain-scoped replay protection | **No** - see §2 below | `DOMAIN_TAG` in `rpc_signature.rs` is a fixed constant with no chain id. |
| `chainlist-submission/chainid-271829.js` at workspace root is stale | Yes | January 2026 DefiLlama-style artefact. Wrong symbol (`FAT`), wrong faucet host (`faucet.testnet.datachain.network` - no DNS), phantom RPC (`testnet.erpc.rope.network`). Different registry (DefiLlama, not `ethereum-lists/chains`). |

Green rows are ready-to-ship as designed. Red rows are prerequisites that gate the facade rollout.

---

## 2. Prerequisite fixes (block the facade)

These three items must land in a preparatory PR **before** the facade design doc's step-by-step plan begins. None of them are large. All are contained changes.

### 2.1 `NodeConfig::testnet()` ports and EVM backend

**File:** `crates/rope-node/src/config.rs`

**Current behaviour:** `NodeConfig::testnet()` starts from `Self::mainnet()` and only overrides `node.name`, `node.chain_id`, `network.testnet`, `network.bootstrap_peers`, `consensus.min_testimonies`, and `genesis.chain_id / genesis.name`. It inherits everything else, which means:

| Setting | Inherited (mainnet default) | Required for testnet facade |
|---|---|---|
| `rpc.http_addr` | `127.0.0.1:8545` | `127.0.0.1:8545` (dedicated host, natural port) |
| `rpc.ws_addr` | `127.0.0.1:8546` | `127.0.0.1:8546` (dedicated host, natural port) |
| `rpc.grpc_addr` | `127.0.0.1:9001` | `127.0.0.1:9001` (dedicated host, natural port) |
| `metrics.prometheus_addr` | `127.0.0.1:9090` | `127.0.0.1:9090` (dedicated host, natural port) |
| `network.listen_addr` | `0.0.0.0:9000` | `0.0.0.0:9000` (dedicated host, natural port) |
| `evm_backend.url` | `http://127.0.0.1:8595` | `http://127.0.0.1:8595` (dedicated host, natural port) |
| `network.bootstrap_peers` | 3 mainnet peer IDs | `Vec::new()` (no testnet committee yet) |
| `consensus.enabled` | `true` (Testimony) | `false` for the dev-mode engine facade |
| `storage.db_path` | mainnet-flavoured path | must contain the substring `testnet` (namespace guard) |

**Deployment topology (2026-08-31 decision):** the testnet runs on a **dedicated DigitalOcean droplet** (`rope-testnet-1`, lon1, s-2vcpu-4gb, Ubuntu 24.04), NOT alongside mainnet on `new-blue`. This decision was taken after auditing `new-blue`, where the original proposed testnet ports (`8547/8548/9002/9091/9010`) all collided (mainnet reth's WS on `8547`, testnet reth's WS on `8548`, mainnet reth's metrics on `9002`, compliance-agent on `9091`, and `9010` reserved for a future cluster p2p listener), and where an amended collision-shifted scheme (`8549/8550/9002→9012/9091→9093/9011`) would have added permanent operational surface area purely to work around co-hosting. Rather than paying that co-hosting tax forever, the testnet moves to its own box, which:

1. Isolates blast radius from Chainlist-driven abuse probes.
2. Eliminates memory pressure on `new-blue` (mainnet writer + reth + testnet reth was already close to the 8 GB ceiling per `handover-mtbf-postmortem-swap-thrash-2026-08-23.mdc`).
3. Lets every ops script assume "rope-node listens on `:8545 / :8546 / :9001 / :9090 / :9000` and reads from Reth on `:8595`" regardless of which network the box serves - the *only* difference between a mainnet box and a testnet box is `--network testnet` on the systemd `ExecStart`.

**Consequence for `NodeConfig::testnet()`:** the config uses natural mainnet-style ports. A single unit test (`testnet_config_uses_natural_ports_and_disabled_consensus`) pins the ports, asserts `chain_id != 271828` (so a future edit that copies the mainnet chain_id through cannot ship silently), and asserts `db_path` contains the substring `testnet` (so a future edit that shares a `db_path` with mainnet cannot ship silently either).

The bootstrap peers and consensus flag are the most surprising ones - inheriting them would make the testnet node try to gossip mainnet peer IDs and require a Testimony quorum against a single-node reth `--dev` engine, both of which fail closed at start-up.

**Future co-hosting escape hatch:** if a later operator ever needs to co-host mainnet and testnet on the same box (e.g. for a laptop developer scenario), they should override the ports via a `deploy/config/rope-testnet.toml` file rather than baking collision-shifted ports into the default. The `NodeConfig::testnet()` defaults stay natural.

**Change:** rewrite `NodeConfig::testnet()` to construct a full config explicitly rather than deriving from `mainnet()`. Ship an accompanying unit test that asserts every port and URL to prevent silent regressions if `mainnet()` shifts a default.

**Effort:** ~2 hours including the test.

### 2.2 Chain-scoped signing tag in `rpc_signature.rs`

**File:** `crates/rope-node/src/rpc_signature.rs`

**Current state (line 44-46):**

```rust
/// Domain-separator tag: keeps signatures for destructive-rpc calls disjoint
/// from every other signing surface (rope-idp, EDC console, mesh, etc.) and
/// prevents cross-chain replay against any other Datachain Rope-flavoured
/// deployment (mainnet, testnet, sandbox).
pub const DOMAIN_TAG: &[u8] = b"DCROPE/destructive-rpc/v1\0";
```

The comment is wrong today. The tag has no chain-id binding, so a captured mainnet envelope can be replayed against testnet if the wallet has FAT on both chains, the nonce isn't recorded on testnet (independent nonce store), and `signed_at` is inside the freshness window. The `chain_id` field in JWTs (used by `rope-idp`) IS chain-scoped, but the destructive-RPC scheme isn't.

**Change:** thread `chain_id: u64` into `canonical_message` and `verify_destructive_call`, and derive the tag as follows:

```rust
const DOMAIN_TAG_PREFIX: &[u8] = b"DCROPE/destructive-rpc/v1";

fn domain_tag_for(chain_id: u64) -> Vec<u8> {
    // Mainnet keeps the historical wire (no suffix, no chain id) so that
    // signed envelopes minted by clients on mainnet continue to verify.
    if chain_id == 271828 {
        let mut v = DOMAIN_TAG_PREFIX.to_vec();
        v.push(0);
        v
    } else {
        // Every other Rope-flavoured chain (testnet 271829 today, sandboxes
        // tomorrow) gets an explicit /<chain_id> suffix so an envelope
        // cannot cross the chain boundary.
        let mut v = DOMAIN_TAG_PREFIX.to_vec();
        v.push(b'/');
        v.extend_from_slice(chain_id.to_string().as_bytes());
        v.push(0);
        v
    }
}
```

Rationale for the mainnet carve-out: every existing mainnet client (SDK, Datawallet+, DCSwap, EDC console) has the current tag baked in as a byte string. If we change mainnet's tag, every one of them stops signing correctly on the day of the release, without a way to detect that in the client. Keeping the mainnet tag byte-identical means the release is a pure additive rollout: new chains pick up the chain-scoped tag automatically, mainnet's wire is frozen.

**SDK side:** the TypeScript sample (`docs/handovers/DESTRUCTIVE_RPC_SIGNING.md` and the reference examples under `examples/phase2-signed-rpc/`) must pick the tag from the connected wallet's `eth_chainId` at sign time rather than hardcoding it. This is a one-liner in each example.

**Change:** ~4 hours including:
- The Rust change and its unit tests (add a `signature_verifies_only_on_matching_chain_tag` test).
- The Rust example `examples/phase2-signed-rpc/sign_phase2_rpc.rs` updated to derive the tag from `--chain-id`.
- The TypeScript example `examples/phase2-signed-rpc/sign-phase2-rpc.ts` updated to derive the tag from `chainId`.
- A note in `docs/handovers/DESTRUCTIVE_RPC_SIGNING.md` explaining the mainnet carve-out and the testnet tag shape.

Optional (post-facade): once the SDK caches are refreshed across the ecosystem, we can consider a v2 tag that always includes chain id and eventually retire the mainnet carve-out on a coordinated release. Not for this pass.

### 2.3 Stale `chainlist-submission/` artefact

**File:** `chainlist-submission/chainid-271829.js` (workspace root)

DefiLlama-style entry from January 2026. Wrong symbol, wrong faucet host, phantom RPC. Different registry from the current target (`ethereum-lists/chains`).

**Choice:**

1. **Delete** the file. This is the recommended path - the design doc's `docs/design/eip155-271829.json` is the single source of truth for the current submission.
2. **Repurpose** the directory for a follow-up DefiLlama Chainlist submission, but rewrite `chainid-271829.js` from `docs/design/eip155-271829.json` first. Only worth doing if we actually plan to submit to DefiLlama - most wallets aggregate against `ethereum-lists/chains`, so DefiLlama is a nice-to-have.

Whichever we pick, the current file must not survive the roadmap - it's the kind of leftover that a future agent will find and mistake for the canonical payload.

**Change:** ~10 minutes.

---

## 3. Ordered execution plan

### Phase 0 - prerequisites (LANDED 2026-08-31 in laptop tree)

1. ✅ **§2.1 - `NodeConfig::testnet()` rewritten explicitly (natural ports + dedicated-host topology).** `crates/rope-node/src/config.rs` no longer inherits mainnet ports transitively. Testnet is now constructed field-by-field. Because the testnet runs on a **dedicated box** (`rope-testnet-1` on DigitalOcean, see §2.1 topology note), the config uses natural mainnet-style ports: `rpc.http_addr=127.0.0.1:8545`, `ws=8546`, `grpc=9001`, `metrics.prometheus_addr=127.0.0.1:9090`, `network.listen_addr=0.0.0.0:9000`, `evm_backend.url=http://127.0.0.1:8595`, `consensus.enabled=false`, empty `network.bootstrap_nodes`, and `db_path` namespaced under `testnet`. New unit tests `testnet_config_uses_natural_ports_and_disabled_consensus` + `mainnet_config_defaults_unchanged` + `for_network_dispatches_correctly` pin both branches; the mainnet test asserts every mainnet field is bit-identical to the pre-refactor defaults, and the testnet test asserts `chain_id != 271828` and `db_path.contains("testnet")` so a future edit that lets one leak into the other cannot ship silently.
2. ✅ **§2.2 - Chain-scoped `DOMAIN_TAG` in `rpc_signature.rs` with mainnet carve-out.** Introduced `MAINNET_CHAIN_ID = 271828`, `chain_domain_tag(chain_id)`, `canonical_message_with_chain(chain_id, …)`, and `verify_destructive_call_for_chain(verifier, chain_id, …)`. Mainnet keeps the frozen legacy `DCROPE/destructive-rpc/v1\0` byte string (asserted by `chain_domain_tag_mainnet_is_frozen_legacy_bytes` and `canonical_message_mainnet_default_matches_frozen_wire_format`). Testnet and every other chain emit `DCROPE/destructive-rpc/v1/{chain_id}\0` (asserted by `chain_domain_tag_testnet_encodes_chain_id`, `chain_domain_tag_distinct_per_chain`, `canonical_message_testnet_differs_from_mainnet_from_first_byte`). Cross-chain replay is rejected in both directions (`signature_from_mainnet_is_rejected_on_testnet`, `signature_from_testnet_is_rejected_on_mainnet`), and the mainnet call site in `rpc_server.rs::handle_json_rpc_with_auth` now threads `self.chain_id` through `verify_destructive_call_for_chain`.
3. ✅ **§2.2 - SDK examples chain-scoped.** `examples/phase2-signed-rpc/sign_phase2_rpc.rs`, `sign_phase2_create_ledger.rs`, and `sign-phase2-rpc.ts` all accept `chain_id` as a positional CLI argument (defaults to mainnet for backward compatibility) and derive the domain tag via a byte-for-byte-identical `chain_domain_tag` helper. The Rust and TypeScript helpers print the domain tag on startup so partners can eyeball wire correctness before submitting.
4. ✅ **§2.2 - `PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md` updated.** `Last updated` bumped to 2026-08-30, mainnet vs non-mainnet tag shapes are explicitly documented, and the embedded Rust and TypeScript snippets show the `chain_domain_tag` derivation and comment on the frozen mainnet byte string.
5. ✅ **§2.3 - Stale `chainlist-submission/chainid-271829.js` deleted 2026-08-30.** `chainlist-submission/README.md` now records the deletion, points the testnet path at `ethereum-lists/chains`, and cross-references `docs/design/eip155-271829.json` as the single source of truth.
6. ✅ **Regression evidence.** `cargo test -p rope-node --lib` = **199 passed / 0 failed / 0 ignored**. Ten new tests landed in Phase 0: seven in `rpc_signature::tests` (`chain_domain_tag_mainnet_is_frozen_legacy_bytes`, `chain_domain_tag_testnet_encodes_chain_id`, `chain_domain_tag_distinct_per_chain`, `canonical_message_mainnet_default_matches_frozen_wire_format`, `canonical_message_testnet_differs_from_mainnet_from_first_byte`, `signature_from_testnet_is_rejected_on_mainnet`, `signature_from_mainnet_is_rejected_on_testnet`) and three in `config::tests` (`testnet_config_has_shifted_ports_and_disabled_consensus`, `mainnet_config_defaults_unchanged`, `for_network_dispatches_correctly`).

**Still gated on the operator:**

7. ⏸ Deploy the rebuilt `rope-node` binary to `rope-vps` (mainnet writer). Mainnet is unaffected by design: the mainnet `DOMAIN_TAG`, canonical-message pre-image, verifier pre-image, and dispatch entry-point are byte-identical to the pre-Phase-0 build. Confirm live with one signed `rope_appendToLedger` against `erpc.datachain.network` using the existing `sign_phase2_create_ledger` example (no `chain-id` positional arg = mainnet default).
8. ⏸ Deploy the rebuilt binary to the testnet host as part of Phase 1 (see below).

**Exit criterion (already met in laptop tree, awaits deploy):** mainnet destructive-RPC verification unchanged (bit-identical wire, asserted by regression tests), `NodeConfig::testnet()` emits the facade's ports and backend URL (asserted by unit test), `chainlist-submission/` directory scrubbed of stale testnet payload.

### Phase 1 - `rope-testnet-node` facade rollout on `rope-testnet-1` (weeks 1-2 after Phase 0)

Follow `docs/design/rope-testnet-writer-facade.md`. The design doc has been amended for the dedicated-host topology; the sequence below is the operational checklist.

**Prerequisite: provision the dedicated testnet box.**

0. **Provision `rope-testnet-1`** on DigitalOcean lon1 (s-2vcpu-4gb, Ubuntu 24.04, existing SSH key). Open firewall for `22/tcp` (SSH), `9000/tcp` (libp2p, unused today but reserved), and `443` (only for the eventual nginx-terminated public endpoint if we move TLS off `new-blue`; for the canary we keep TLS on `new-blue` and let nginx proxy over the private network). Register hostname `rope-testnet-1.dcrope.internal` in `~/.ssh/config`.
1. **Install the testnet stack on `rope-testnet-1`** (parallel to the running services on `new-blue`, so testnet users see zero downtime during migration):
   - `reth --dev --dev.block-time 3s --chain <testnet-genesis>` on `127.0.0.1:8595` (natural mainnet-style Reth port; safe here because this box only serves testnet).
   - `rope-testnet-faucet.service` on `127.0.0.1:3100` (identical to `new-blue`).
   - `rope-testnet-node.service` on `127.0.0.1:8545` from the Phase-0 binary, with `--network testnet` and `[phase2_signed_destructive] = true`.
2. **Internal end-to-end smoke test** on `rope-testnet-1` loopback (all natural ports, dedicated box → no collisions):
   - `curl http://127.0.0.1:8545 -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'` returns `0x42555` (271829).
   - `curl http://127.0.0.1:8545 -d '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}'` returns `Datachain-Rope/…-testnet` (from the facade, not from the sanitizer hack).
   - `curl http://127.0.0.1:8545 -d '{"jsonrpc":"2.0","id":1,"method":"rope_untieKnot","params":[…]}'` (unsigned, with `X-Forwarded-For` set to simulate nginx) returns the Phase-1 `-32401 Method denied on public listener`.
   - A correctly-signed `rope_untieKnot` (using the chain-scoped tag `DCROPE/destructive-rpc/v1/271829\0`) passes and produces a tombstone.
3. **Repoint DNS + nginx** at `testnet.erpc.datachain.network` and `faucet.datachain.network` from the `new-blue` upstreams to `rope-testnet-1`. Options:
   - Simplest: keep TLS termination on `new-blue` and change its upstream from `http://172.18.0.1:3100/rpc` to `http://<rope-testnet-1 private IP>:8545` for the facade and `http://<rope-testnet-1 private IP>:3100` for the faucet paths. No DNS change, no cert change.
   - Cleaner (later): move DNS to point at `rope-testnet-1` directly, terminate TLS on `rope-testnet-1`. Adds a certbot workflow but removes a hop.
   Do the simplest option first; the cleaner one is a follow-up once the box has soaked. Keep the previous `new-blue` nginx config as a `.disabled` file next to the new one.
4. **Remove the sanitizer hack** from the faucet's `server.mjs` and redeploy the faucet on `rope-testnet-1`. The facade returns the right `web3_clientVersion` natively.
5. **Watch for one clean week.** No 5xx spike, no new lints, no unauthenticated destructive-RPC leaks (grep the systemd journal for `Method denied on public listener` and confirm the numbers look like abuse-scan noise, not client error). During this window the old `new-blue` testnet services stay running but no longer receive external traffic - they are the immediate rollback target.
6. **Decommission the testnet services on `new-blue`** after the clean week: backup the two binaries, `systemctl disable --now rope-testnet-engine.service rope-testnet-faucet.service`, remove the associated `data_dir` after a further 30-day cool-off (in case of an emergency rollback beyond the week).

**Exit criterion:** `testnet.erpc.datachain.network` serves through the facade running on `rope-testnet-1`, unsigned destructive RPCs are rejected on the public listener, the faucet no longer touches Reth wire format, one clean week of production traffic is on the record, and the testnet services on `new-blue` are disabled (data still on disk for the 30-day cool-off).

### Phase 2 - Chainlist PR (week 3 after Phase 0)

Follow `docs/design/chainlist-271829-submission.md` verbatim. Key checkpoints:

1. **Re-verify the payload** in `docs/design/eip155-271829.json` against production one more time immediately before opening the PR:
   - `eth_chainId` on `testnet.erpc.datachain.network` = `0x42555`.
   - `net_version` = `271829`.
   - `web3_clientVersion` = `Datachain-Rope/…-testnet` (via the facade, not the sanitizer).
   - `testnet.dcscan.io/tx/0x0` returns a real 404 (proving the explorer routes are live).
   - `faucet.datachain.network/rpc` still returns the faucet HTML/JSON (proving the faucet URL in the payload works).
2. **Fork** `ethereum-lists/chains` under a personal GitHub account (not the org account - the ethereum-lists reviewers prefer PRs from individual forks).
3. **Copy** `docs/design/eip155-271829.json` to `_data/chains/eip155-271829.json` on the fork.
4. **Open the PR** using `docs/design/chainlist-271829-pr-body.md` as the description. Cross-link the 271828 PR from January if we can find it, so the reviewer sees the parent-testnet relationship immediately.
5. **Merge lands** typically within a few days for a well-formed testnet entry. The `chainid.network` CDN dump refreshes within 24h of merge. `chainlist.org` picks up from the CDN within another 24h.

**Exit criterion:** `chainlist.org` lists Datachain Rope Testnet under chain 271829 with a working "Connect Wallet" button that programmatically adds the network to MetaMask via `wallet_addEthereumChain`.

---

## 4. Success metrics

Both items are done when all of these hold simultaneously:

| Metric | How to measure |
|---|---|
| `testnet.erpc.datachain.network` returns `web3_clientVersion = Datachain-Rope/*-testnet` | `curl` |
| Unsigned `rope_untieKnot` on the testnet public listener returns `-32401` | `curl` |
| Correctly-signed `rope_untieKnot` on the testnet passes and produces a tombstone visible on `testnet.dcscan.io` | Live signed call |
| Testnet Phase-2 signature verifies ONLY with the chain-scoped tag `DCROPE/destructive-rpc/v1/271829\0` and rejects a mainnet-tag signature | Unit test + live smoke |
| Mainnet Phase-2 signature continues to pass with the historical tag `DCROPE/destructive-rpc/v1\0` | Existing regression tests + live smoke |
| `chainlist.org` shows Datachain Rope Testnet 271829, one-click add-to-MetaMask works | Manual test with a clean browser profile |
| Faucet no longer runs `sanitizeUpstreamResponse` | Grep `faucet/server.mjs` |
| `chainlist-submission/chainid-271829.js` no longer exists (or accurately mirrors `docs/design/eip155-271829.json`) | `ls` |

---

## 5. Cost estimate

### Engineering

| Phase | Effort | Wall time |
|---|---|---|
| Phase 0 (prerequisites) | ~1 engineer-day (**LANDED**) | done |
| Phase 1 (facade + dedicated-host migration) | ~2.75 engineer-days (per `rope-testnet-writer-facade.md` §7) | 1 week of provisioning + rollout + 1 week of soak |
| Phase 2 (Chainlist PR) | ~2 engineer-hours to open + follow up | 1 week for reviewer to merge, then 1-2 days for the CDN |

Total wall time from now to Chainlist-live: **~3 weeks**, of which ~2 weeks is passive observation. Actual coding is ~3 engineer-days combined on top of the already-landed Phase 0.

### Infrastructure

- **New:** `rope-testnet-1` DigitalOcean droplet (s-2vcpu-4gb Ubuntu 24.04 in lon1), ~$24/month.
- **Freed:** ~2 GB RAM + ~50 GB disk on `new-blue` after Phase-1 decommissioning of the co-located testnet stack. Directly reduces mainnet-writer memory pressure per `handover-mtbf-postmortem-swap-thrash-2026-08-23.mdc`.
- **Net:** +$24/month, with a strictly positive trade against `new-blue` operational risk.

### Ecosystem coordination

None. Testnet users see a DNS-transparent flip. No downstream project has to change anything.

---

## 6. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Facade rollout breaks the existing testnet public RPC (customers of the current permissive endpoint start seeing rejections) | The permissive endpoint had `rope_untieKnot` succeeding without a signature - by design, nobody should have shipped code depending on that. The rejection message includes a pointer to the signing spec. If we do see a real customer regression, revert nginx to the previous `new-blue` upstreams (`.disabled` config file kept in place) - the old services stay running on `new-blue` for the whole soak week specifically to allow this rollback. |
| Mainnet `DOMAIN_TAG` carve-out gets forgotten and someone flips mainnet to the chain-scoped tag on a v2 release | Regression tests `chain_domain_tag_mainnet_is_frozen_legacy_bytes` and `canonical_message_mainnet_default_matches_frozen_wire_format` in `rpc_signature.rs` assert the frozen mainnet byte string. Comment in `rpc_signature.rs` immediately above `chain_domain_tag` says "do not change mainnet's tag without a coordinated ecosystem release." |
| Chainlist PR gets rejected for schema issues | `docs/design/eip155-271829.json` was authored against the schema in `ethereum-lists/chains/_data/chainSchema.json`. Pre-flight step 1 in Phase 2 catches schema drift by re-running the repo's `npm run validate` against the fork before opening the PR. |
| Chainlist listing attracts wallet-onboarding traffic before the facade is fully seasoned | The ordering constraint (facade must land + observe 1 week before PR) is the mitigation. Do not shortcut it. |
| `xFAT` symbol confuses wallets that expect mainnet's `FAT` symbol | Documented in `chainlist-271829-pr-body.md`. Wallets show the symbol as it appears on chain; testnet users copying mainnet balances will see the mismatch immediately, which is exactly the right UX for a testnet. |
| `rope-testnet-1` is a single-node deployment; if the droplet dies, the testnet is offline until it comes back | Acceptable for a testnet by design (see §7). Snapshots on DO (weekly), backups of `rope-testnet.toml` + faucet secrets in the ops secret store, and a documented "rebuild `rope-testnet-1` from scratch in ~30 minutes" runbook are sufficient. If a real HA requirement emerges later, Phase 3 can add a second box (`rope-testnet-2`) behind the same nginx upstream. Not this roadmap. |
| Provisioning `rope-testnet-1` late and forgetting to firewall it | DigitalOcean cloud firewall is applied to the droplet at creation time via the ops runbook. Only `22/tcp` (SSH from admin IPs), `9000/tcp` (libp2p, reserved), and `443/tcp` (only if we ever move TLS off `new-blue`) are open. RPC ports (`8545/8546`) stay on `127.0.0.1` and are reached from `new-blue`'s nginx over the DO private network. |
| DNS repointing drift: `testnet.erpc.datachain.network` still points at `new-blue` after decommissioning the co-located services | Phase 1 step 3 is DNS/nginx repointing **before** the soak week starts, not after. The soak validates the new upstream. Decommissioning happens only after the soak passes AND the DNS repoint is confirmed. |

---

## 7. What is explicitly out of scope

- **Testnet Testimony consensus.** The facade sits in front of a single-node reth `--dev` engine that auto-mines. `consensus.enabled = false` in `NodeConfig::testnet()`. We do not stand up a testnet committee in this roadmap.
- **Testnet HA / multi-box redundancy.** `rope-testnet-1` is a single droplet by design - a testnet does not need writer-promote, mempool-sharing, or the mainnet-grade fleet-status HA that BLUE/GREEN gives mainnet. If a real HA requirement emerges (e.g. persistent developer-facing SLA commitments), Phase 3 can add a second box. Not this roadmap.
- **Adding new RPC methods to the facade's allowlist.** The allowlist is inherited from the mainnet Phase-2 verifier. Adding a method is a separate design.
- **Cross-chain nonce sharing.** Testnet's Phase-2 nonce store is independent from mainnet's. The chain-scoped tag makes this safe.
- **Migrating existing testnet balances or state to `rope-testnet-1`.** Phase 1 does not migrate reth's data directory from `new-blue` - the testnet is a scratchpad, and starting from a fresh reth `--dev` state on the new box is the intended behaviour. Any developer with pinned state on `new-blue`'s testnet reth is on notice via the developer quickstart update.
- **Moving TLS termination to `rope-testnet-1`.** Phase 1 keeps TLS on `new-blue` and proxies over the DO private network. Moving TLS is a Phase-3 nice-to-have that can happen once `rope-testnet-1` has soaked.
- **Retiring the mainnet carve-out for `DOMAIN_TAG`.** Post-Chainlist, we can consider a v2 tag that always includes chain id. Not this roadmap.

---

## 8. Cross-references

- `docs/design/rope-testnet-writer-facade.md` - Phase-1 design doc, canonical sequencing.
- `docs/design/chainlist-271829-submission.md` - Phase-2 runbook.
- `docs/design/eip155-271829.json` - PR payload.
- `docs/design/chainlist-271829-pr-body.md` - PR description.
- `.cursor/rules/handover-testnet-erpc-endpoint-and-rope-naming-2026-08-30.mdc` - current testnet endpoint + systemd rename state.
- `.cursor/rules/handover-security-audit-2026-06-11.mdc` §V11 - the Phase-2 signed-write design this facade inherits.
- `.cursor/rules/handover-milestone-2026-08-30.mdc` - the milestone build that this roadmap sits on top of.
- `crates/rope-node/src/rpc_auth.rs` - Phase-1 destructive-method gate.
- `crates/rope-node/src/rpc_signature.rs` - Phase-2 signature verifier (target of §2.2).
- `crates/rope-node/src/config.rs` - `NodeConfig::testnet()` (target of §2.1).

---

## 9. Handover note for the implementing agent

If you're the agent picking this up: Phase 0 is **already landed** in the laptop tree with 199/199 green tests (see the checklist at the top of §3). Your first action is to deploy the Phase-0 rebuild to `rope-vps` (mainnet writer) - mainnet is unaffected by design, so this is a pure "get the new binary into production" step - and then start Phase 1 on the dedicated-host path (§3, Phase 1, step 0 = provision `rope-testnet-1`).

Do NOT skip the mainnet carve-out reasoning in §2.2 or attempt to flip the mainnet `DOMAIN_TAG` on the same release - that would silently break every ecosystem SDK on the same day.

Do NOT co-locate the testnet stack on `new-blue` "just for now" - the dedicated-host decision is the point of this refresh. Co-location was audited and rejected on 2026-08-31; the port-collision matrix in §2.1 and the memory-pressure cross-reference to `handover-mtbf-postmortem-swap-thrash-2026-08-23.mdc` explain why.

If you find that any of the three design docs (`rope-testnet-writer-facade.md`, `chainlist-271829-submission.md`, this one) have drifted from production between now and the day you start, re-run §1 as your first task and update this roadmap accordingly before writing any code.
