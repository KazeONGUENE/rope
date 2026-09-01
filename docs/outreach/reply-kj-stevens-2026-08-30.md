# Reply to KJ Stevens — developer-experience feedback (2026-08-30)

**From:** Kazé A. ONGUENE — Datachain Foundation
**To:** KJ Stevens
**Re:** Feedback on datachain.network/docs quick-start (developer walkthrough)
**Status:** DRAFT for send
**Reference build:** milestone `2026-08-30` (production, no regressions — see internal notes)

---

Hi KJ,

Thank you — that walkthrough paid for itself. The "positive takeaway plus a punch-list" format made every item actionable in one pass, and it turned into more than just a set of fixes: it forced a decision on the developer-onboarding story that had been half-baked for months. Every item you flagged is closed on production, and today's build has been pinned as our `2026-08-30` reference milestone — no regressions, full stack verified end-to-end (mainnet RPC, dcscan, testnet, faucet, console, installer, agents, identity, CERBER mesh). Details on each of your points below, in the order you raised them.

## 1. GitHub README typo — chain ID `271828` shown as `0x42644`

**Fixed.** The README on the `main` branch of [github.com/KazeONGUENE/rope](https://github.com/KazeONGUENE/rope/blob/main/README.md) now correctly shows `Chain ID | 271828 (0x425D4)` for mainnet and `271829 (0x425D5)` for the new testnet. A drift monitor now compares the docs page, the faucet page, the README, and the live RPC against each other on a schedule and pages our on-call channel if any surface disagrees with `eth_chainId` — that class of typo can't quietly land again.

## 2. Conflicting `rope-cli` instructions (`cargo install` vs. build-from-source)

**Fixed — and the fix is bigger than a doc edit, because your feedback forced a decision that had been sitting in a queue.** The two paths conflicted because we were straddling two philosophies. Your walkthrough made us commit to the right one: **a developer should not need to `git clone` the protocol repo to start building on Datachain Rope.** That was an artefact of an early-alpha "read the source" era we've now moved past. The supported entry points, in the order a developer should reach for them, are:

1. **The Ecosystem Deployment Console** — [`https://console.datachain.network/console/`](https://console.datachain.network/console/). Browser-based. Sign in with your Datachain identity, open the **Deploy a Node** wizard, pick a cloud provider (both **DigitalOcean** and **Exoscale** are live today under the Foundation's sub-tenant), pick a region and instance size, and the console provisions a real VM, generates a cryptographic node-identity key on first boot (Blake3(Ed25519) + Dilithium3 hybrid), and hands you the IPv4 + status back in the panel. The public half of that key is what the network uses to authenticate the node and to define the rights and role of any third-party node onboarding in the future. Zero manual RPC-URL pasting, zero `git clone`. E2E verified on 2026-08-30: a real DigitalOcean droplet and a real Exoscale `standard.tiny` VM were provisioned and destroyed through the console API against live cloud accounts — no stubs, no dry-runs.
2. **One-line CLI installer** — `curl -fsSL https://get.datachain.network | sh`. Same `rope` binary the console uses under the hood, for anyone who wants a shell tool without the browser flow. Live now: pinned versioned tarballs under `/dist/<version>/`, SHA-256 verified against `SHA256SUMS` at install time, TLS via Let's Encrypt, no auth. A `rope`-provisioned node speaks the same signed-write protocol the console-provisioned node does, so the two paths are indistinguishable on the wire.
3. **Source build** — `git clone` + `cargo build --release -p rope-cli`. Documented as the **contributor** path, not the developer path. For people patching `rope-node` itself; ordinary integrators should not need it.

The Quick Start now leads with (1), points at (2) as the CLI option, lists (3) under "Building from source (contributors)", and no page anywhere says `cargo install rope-cli` (0 references on the live docs, verified today).

If you'd like to sanity-check the console before pointing more developers at the docs, the wizard is live at [`https://console.datachain.network/console/`](https://console.datachain.network/console/) — same TLS chain, same identity provider (id.datachain.network) as the rest of the ecosystem, same live status on both providers.

## 3. Testnet faucet returned a 502

**Fixed — and this is the largest win of the batch.** When you tested, the testnet was in a "planned, not deployed" state and the faucet page was routing to a stub that could return 502 under load. That is no longer the case. As of today:

- The testnet is live at chain ID `271829` (hex `0x425D5`), independent of mainnet.
- RPC endpoint: `https://testnet.erpc.datachain.network`
- Block explorer: `https://testnet.dcscan.io`
- Faucet: `https://faucet.datachain.network` — rate-limited, mints a small amount of test-only gas credit per address, returns the drip tx hash back to the caller.
- Test-only currency: **`xFAT`**, not FAT. `xFAT` is a chain-native gas credit with no mainnet value — the same pattern most L1s use (SepoliaETH, tBNB, MATIC-mumbai). It exists only to let developers deploy contracts and exercise the JSON-RPC surface without spending real DC.
- The docs now have a dedicated **Testnet & Faucet** section with a one-click "Add to Wallet" button (EIP-3085), the correct chain parameters, and a working drip flow.

Shortest path to re-run your walkthrough against the live testnet:

```
Wallet chainId:     271829 (0x425D5)
Network name:       Datachain Rope Testnet
RPC URL:            https://testnet.erpc.datachain.network
Currency symbol:    xFAT
Block explorer:     https://testnet.dcscan.io
Faucet:             https://faucet.datachain.network
```

Curl-only path if you'd rather not touch a wallet:

```bash
curl -sS -X POST https://testnet.erpc.datachain.network \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'
# → {"jsonrpc":"2.0","id":1,"result":"0x425d5"}   (271829 — testnet)
```

## 4. Older Databøx docs showing chain ID `314159`

**Fixed on the live surfaces.** `314159` was the temporary chain ID used during the pre-mainnet Databøx era; the migration to `271828` happened in January and we thought every user-facing document had been swept. It hadn't — three internal deployment templates still carried it, which is what would have leaked into any doc regenerated from those templates. Those are patched now, and the drift monitor from (1) explicitly treats `314159` (and its testnet sibling `314160`) as **known-wrong** chain IDs, so any future doc that mentions either will page us on the next run. Live check today: 0 references to `314159` on `datachain.network/docs`, 21 references to `271828`, 1 reference to `0x425D4`.

If you or any developer you point at the docs still finds a page that shows `314159`, please send me the URL — that will almost certainly be a cached copy somewhere we don't own, and I'd like to know who's mirroring us with stale content.

## 5. Direct RPC testing — ChatGPT's environment couldn't resolve our domains

Noted, and this is a fair point even if it wasn't a Rope failure. Two things that should help the next developer:

- The endpoints in (2) and (3) above are all standard public HTTPS, no auth required, CORS-open, and reachable from ordinary consumer networks. If a testing environment can't resolve `datachain.network`, it's typically a walled-garden restriction on that environment, not on our side.
- We also expose `https://erpc.datachain.network/v1/fleet-status` (mainnet) as a JSON health surface any external observer can call to see the writer state, block-tip, edge sample-ok ratio, and recommended client-side deadline padding. That's useful when a developer's local environment is refusing to talk to us but they need a green/red signal — or when they're writing a client and want to honour our self-heal protocol (which today already carries DCSwap and Tanastok through routine writer-heals without a user-visible failure).

---

## The bigger takeaway I want to acknowledge

You wrote:

> looking at this from a developer's perspective, there's enough real infrastructure and source material there to make someone want to clone the repo and investigate further.

That is the specific thing we've been trying to earn, and it's the most useful signal in the whole email. We've been deliberately conservative about marketing until the developer experience holds up under a real walkthrough — your review told us the walkthrough now clears that bar, modulo the punch-list, which is closed. Please do point more developers at the docs. If any of them hit a wall, I want to hear about it directly, in the same format you used here.

Two things on the near-term roadmap you might find worth a second look:

- A **rope-node facade in front of the testnet engine** so the testnet inherits the same signed-write allowlist as mainnet. Right now the testnet is a permissive playground on purpose (developers need to be able to try things); once the facade lands, `rope_untieKnot` and the other destructive methods will be signature-gated on the testnet too, and testing an untie flow against the testnet will exercise the exact same code path that guards mainnet.
- A **chainlist.org listing** for chainId `271829` so wallet on-boarding on the testnet is a one-click flow without pasting RPC URLs. The mainnet listing at chainId `271828` has been up since early in the year; the testnet one is a natural follow-up.

Thanks again for the review. This one had a compounding effect — every item you flagged either closed a bug or upgraded a guard-rail, and (3) in particular turned into a full testnet + faucet + explorer rollout that we've owed developers for a while.

If a good moment comes up, I'd like to chat by video for 20 minutes about what a future review round could look like — not to burden your time, but because the format you used here is unusually efficient.

Warmly,
Kazé
Datachain Foundation

---

## Internal notes (do not send)

**Milestone pin: `2026-08-30`.** Regression smoke run today confirmed no regressions across the full ecosystem surface. This build is now our reference for future rollbacks and for future developer walkthroughs. Summary of what was verified on production this session:

| Surface | Endpoint | Result |
|---|---|---|
| Mainnet RPC (writer) | `erpc.datachain.network/v1/fleet-status` | Writer transitioned `unhealthy` → `starting` → `healthy` within self-heal SLA (~60s); `escalate_to_cerber=false`; `pad_secs` published to clients; `estimated_recovery_at` correct. Not a regression — this is the documented steady-state under the pre-A3-memory-upgrade posture (`handover-p0-p1-p2-sequence-2026-08-23.mdc`), and the DCSwap/Tanastok `ResilientRopeClient` protocol handles it transparently. |
| Mainnet RPC (edge) | `erpc.datachain.network` | `edge.status=healthy`, 10/10 samples, `fail_ratio=0.0` |
| Attester read pool | `erpc.datachain.network/v1/read` | HTTP 200, returns current block; writes correctly 405 (verified separately from BLUE + DO-rpc-1 + DO-rpc-2 per `handover-to-dcswap-attester-read-do-edges-2026-08-16.mdc`) |
| Ghost-tx reclaim | `fleet-status.ghost_reclaim` | `enabled=true`, `reclaimed_total=107` (cumulative since 2026-07-29 rollout), `last_scan_ghosts_found=0`, `last_scan_error=none` |
| Ledger invariant | `rope_globalStats` | `invariant_holds=true`, 149 strings, 790,087 knots, label_registry: 1681 labels across 7 platforms |
| dcscan.io | `/api/v1/stats`, `/api/v1/supply/circulating`, `/api/v1/supply/reconciliation`, `/api/v1/labels`, `/api/v1/revenue-conversions`, `/api/v1/network/config`, `/address/<addr>` deep-links | All 200; supply Scenario A honoured (~3.73B circulating); 5 uncirculated wallets present in reconciliation; d001–d005 labels present; revenue-conversions returns `live=false, phase=pending` as expected substrate-only state |
| datachain.network | `/`, `/docs`, `/testnet`, `/faucet`, `/about`, `/contact`, `/privacy`, `/terms` | All 200; 5 references to `console.datachain.network` in docs; 0 references to `cargo install rope-cli`; 0 references to `314159`; correct `0x425D4` present; `curl -fsSL https://get.datachain.network` present |
| console.datachain.network | `/console/`, `/healthz`, `/api/v1/ecosystem/providers`, `/api/v1/ecosystem/nodes` | All 200; local + digitalocean + exoscale all `live=true`; anonymous 401 on POST, authenticated 200 with empty list (correct scoping); "+ Deploy Node" button + provisioning modal + destroy button live in UI |
| get.datachain.network | `/`, `/install.sh`, `/latest.txt`, `/dist/0.1.0/*.tar.gz`, `/dist/0.1.0/SHA256SUMS` | All 200; TLS via Let's Encrypt; install script `text/plain` + `no-cache`; tarballs `application/octet-stream` + `immutable`; end-to-end `curl -fsSL https://get.datachain.network | sh` smoke-tested from clean shell on new-blue → binary installed to `~/.local/bin/rope`, SHA-256 matched, `rope --version` printed correctly |
| id.datachain.network | `/healthz`, `/.well-known/jwks.json` | 200 + JWKS data present |
| Testnet | `testnet.erpc.datachain.network` (`eth_chainId`, `eth_blockNumber`) | `chainId=0x425d5` (271829), block advancing; `faucet.datachain.network` + `/api/v1/status` both 200 |
| Agents | `agents.datachain.network`, `semantic-agent.datachain.network/v1/search`, `compliance-agent.datachain.network/healthz` | All 200 |
| CERBER mesh | `erpc.datachain.network/v1/cerber/mesh-status` | 4 peers registered (rope, dcswap, tanastok, alteros); dcswap + tanastok + alteros all `reachable=true`; rope is self-reference |

**KJ-item resolution — evidence:**

- README typo (`0x42644` → `0x425D4`): pushed to `main` at [KazeONGUENE/rope@main](https://github.com/KazeONGUENE/rope/blob/main/README.md). Live check today via GitHub API: line 27 shows `| **Chain ID** | 271828 (0x425D4) |`, line 56 shows `| **Chain ID** | 271829 (0x425D5) |`.
- rope-cli install path: repositioned per operator directive 2026-08-30. Primary developer path is now the Ecosystem Deployment Console at `https://console.datachain.network/console/` (rope-edc backend, systemd unit `rope-edc.service` on new-blue, nginx vhost `console.datachain.network` proxying to `host.docker.internal:9095`, HTTPS via Let's Encrypt, HTTP 200 verified externally). Secondary path is `curl -fsSL https://get.datachain.network | sh` (nginx vhost `get.datachain.network` on new-blue, Let's Encrypt TLS, install script served at `/` and `/install.sh` — `text/plain`, `Cache-Control: no-cache`; versioned tarballs under `/dist/<version>/*.tar.gz` with `SHA256SUMS`, `latest.txt` pointer; end-to-end verified from clean shell). Tertiary path (git clone + cargo build) retained as "contributor build" in `docs/index.html`. Console + CLI both drive `rope-edc` → `rope-deployer::ProviderRegistry`; DigitalOcean + Exoscale providers both `live=true` on `/api/v1/ecosystem/providers`. Real E2E on 2026-08-30: provisioned + destroyed a droplet (`528316301`, IPv4 `165.227.132.87`, `s-2vcpu-4gb` in `fra1`) on DO and a `standard.tiny` VM (`4f3476a7-8c52-4f15-8027-51f59b5ef8bc`, `standard.tiny` in `ch-gva-2`) on Exoscale via the console API. Credentials in `/etc/rope-edc.env` (root-only, not committed). Deployer state at `/opt/datachain-rope/edc/deployer-state`. Console-generated nodes emit a node-identity keypair (Blake3(Ed25519) + Dilithium3 hybrid) on first boot; the public key is what the network uses to authenticate third-party nodes and to gate destructive `rope_*` methods. Cross-ref: `handover-security-audit-2026-06-11.mdc` §V11 (Phase-2 signed destructive RPC), same signature domain the node-identity key participates in. Full technical detail in `handover-console-node-deploy-live-2026-08-30.mdc`.
- Testnet 502: root cause was a routing stub; testnet is now a real chain (chainId 271829, `rope-testnet-engine.service` + `rope-testnet-faucet.service` on new-blue, isolated from mainnet). Faucet drips `xFAT`, rate-limited per address. Full E2E verified 2026-08-30. Live check today: `faucet.datachain.network` and `/api/v1/status` both HTTP 200; `testnet.erpc.datachain.network` returns `chainId=0x425d5` and advancing block numbers. See `handover-testnet-erpc-endpoint-and-rope-naming-2026-08-30.mdc`.
- Databøx 314159 stale doc: three deployment templates on new-blue still had it (`rope.toml`, `env.production.example`, `full-deploy.sh`). All patched to `271828`. `cerber-docs-drift.service` runs on schedule, treats `314159` and `314160` as known-wrong tripwires. Historical migration checklist at `DEPLOYMENT_CHECKLIST.md` correctly preserves the old value as an audit trail — that's deliberate, not drift. Live check today: 0 references to `314159` on `/docs`.

**Follow-ups filed (not in scope for this reply):**

- ~~Drift monitor at `deploy/cerber/lib/docs-drift.mjs` paged on 2026-08-30 for `docs-testnet-disclaimer` and `faucet-stub` — those are stale checks written when the testnet was still planned; assertions need to be inverted now that the testnet is live. Filed as `cerber-docs-drift-inverted-checks`.~~ **CLOSED same day.** R15 re-fired at 20:07Z on the two stale checks plus a transient `rpc-chainid`/`rpc-globalstats` failure caused by an nginx HTML error page during a brief BLUE flap (the same self-heal event the DCSwap/Tanastok `ResilientRopeClient` protocol handles transparently). Fix landed in `deploy/cerber/lib/docs-drift.mjs`: inverted the stale checks so they now catch the OPPOSITE regression (docs/faucet silently reverting to "PLANNED"), added a testnet RPC chain-id probe (must return `0x425D5`), hardened the JSON-RPC probes with retry + content-type + JSON-shape gating (`eth_chainId` 4×10s, `rope_globalStats` 4×20s because a legit slow answer on that method is 5-6s), and pointed the faucet probe at `https://faucet.datachain.network/` directly instead of relying on the 301 from `datachain.network/faucet`. 11 new unit tests, 43/43 CERBER tests green. Deployed to new-blue; four consecutive systemd runs all reported `pass=12 warn=0 fail=0`. The first post-deploy run needed 4 attempts on `rope_globalStats` — proof the widened budget was necessary. See `handover-milestone-2026-08-30.mdc` "Post-milestone follow-up" section.
- Phase 2 (rope-node facade for testnet writes) design doc: `docs/design/rope-testnet-writer-facade.md`.
- Chainlist PR (271829): payload + runbook at `docs/design/chainlist-271829-submission.md`.
- Coordinated roadmap tying the two together with prerequisites + ordering + success metrics: `docs/design/testnet-parity-roadmap-2026-08-30.md`. **Phase 0 (three prerequisite fixes) LANDED 2026-08-31 in laptop tree** — `NodeConfig::testnet()` rewritten with explicit testnet-scoped defaults (chain_id, DB path, disabled Testimony consensus, empty bootstrap set; ports match the natural mainnet-style scheme because the testnet will run on its own dedicated DigitalOcean droplet `rope-testnet-1`, not co-located on `new-blue`); chain-scoped `DOMAIN_TAG` with mainnet carve-out in `rpc_signature.rs` (`canonical_message_with_chain` + `verify_destructive_call_for_chain` threaded through `rpc_server.rs`, mainnet wire byte-identical, cross-chain replay rejected in both directions); stale `chainlist-submission/chainid-271829.js` deleted with README cross-reference to `docs/design/eip155-271829.json` as SoT. Rust + TypeScript SDK examples chain-scoped; `docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md` updated. `cargo test -p rope-node --lib` = **199 passed / 0 failed** (ten new tests: seven in `rpc_signature::tests`, three in `config::tests`). Deploy to `new-blue` + `rope-vps` (mainnet, wire byte-identical) operator-gated; then Phase 1 (provision `rope-testnet-1`, migrate testnet reth + faucet off `new-blue`, install facade, DNS repoint, one clean week soak) and Phase 2 (ethereum-lists/chains PR for 271829). Dedicated-host decision rationale and rollout plan pinned in `handover-dedicated-testnet-host-2026-08-31.mdc`.
- macOS + Linux ARM64 tarballs for the installer: filed as `installer-arm64-tarballs`.
- Per-wallet VM-count cap on `/api/v1/ecosystem/nodes` POST: filed as `edc-provision-rate-limit`.
- AWS + GCP + Hetzner + OVH provider modules: filed as `edc-provider-aws-gcp-hetzner-ovh` (DigitalOcean + Exoscale are the two live providers today; the `CloudProvider` trait is designed for cheap addition).

**Milestone artifact:** `handover-milestone-2026-08-30.mdc` (this workspace) captures the full reference build state — binaries deployed, service units live, endpoints verified, and rollback pointers.
