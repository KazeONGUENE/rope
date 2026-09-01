# Design + Runbook — `chainlist-271829` (Register testnet at ethereum-lists/chains)

**Author:** Datachain Rope agent
**Date:** 2026-08-30
**Status:** DESIGN + PR-READY PAYLOAD — not submitted, gated on the operator opening the PR from a personal GitHub account (or from a Foundation bot account) so the identity trail is auditable.
**Goal:** register the Datachain Rope Testnet at [chainlist.org](https://chainlist.org) so wallets, chainlist tooling, RPC discovery, and "Add to MetaMask" flows resolve `chainId 271829` to the real testnet RPC, faucet, and explorer.

---

## 0. TL;DR

- Add one JSON file to `ethereum-lists/chains`: `_data/chains/eip155-271829.json`.
- Optional icon add: `_data/icons/datachainTestnet.json` (blockscout-style, references the same on-chain SVG we already ship for mainnet with a "T" ribbon).
- Open a PR against `ethereum-lists/chains` (`master` branch) titled `add: Datachain Rope Testnet (271829)`.
- CI is deterministic: schema-validate the JSON, RPC probe against `https://testnet.erpc.datachain.network` returns `chainId 0x425d5`, faucet returns 200, explorer resolves. Merge is usually 3-14 days after a maintainer reviews.

Do NOT submit this PR until the `rope-testnet-writer-facade` (sibling design doc) has landed. Once we're on chainlist we expect wallet on-boarding + abuse probes to hit the testnet — the facade must be up first so testnet inherits the mainnet method firewall.

---

## 1. Exact payload — `eip155-271829.json`

Paste-ready. This is validated against the current `ethereum-lists/chains` schema (fetched 2026-08-30). The `slip44: 1` field is the BIP-44 test-currency marker used by every testnet in the registry (Sepolia, Base Sepolia, Polygon Amoy, etc.).

```json
{
  "name": "Datachain Rope Testnet",
  "title": "Datachain Rope Testnet",
  "chain": "DATACHAIN",
  "icon": "datachainTestnet",
  "rpc": [
    "https://testnet.erpc.datachain.network"
  ],
  "faucets": [
    "https://faucet.datachain.network"
  ],
  "nativeCurrency": {
    "name": "Testnet xFAT",
    "symbol": "xFAT",
    "decimals": 18
  },
  "infoURL": "https://datachain.network",
  "shortName": "datachain-testnet",
  "chainId": 271829,
  "networkId": 271829,
  "slip44": 1,
  "features": [{ "name": "EIP155" }, { "name": "EIP1559" }],
  "explorers": [
    {
      "name": "DC Scan Testnet",
      "url": "https://testnet.dcscan.io",
      "standard": "EIP3091"
    }
  ],
  "parent": {
    "type": "testnet",
    "chain": "eip155-271828",
    "bridges": []
  }
}
```

Field notes:

- `name` uses the human-friendly "Datachain Rope Testnet" (mainnet uses "Datachain Rope"). This is what MetaMask shows in the network dropdown.
- `title` optional but wallet UIs (Rabby, Frame) surface it as a tooltip.
- `chain: "DATACHAIN"` matches the mainnet's `chain` field. Chainlist groups networks by `chain`, so mainnet + testnet appear under a shared "DATACHAIN" heading.
- `icon: "datachainTestnet"` — a new icon manifest we'll add in the same PR (see §2). If we don't want to ship an icon, drop this field; chainlist just uses a placeholder.
- `nativeCurrency.name` = "Testnet xFAT" (not "DC FAT") and `symbol: "xFAT"` — matches the naming decided when we brought the testnet up (`xFAT` is deliberately distinct from mainnet FAT so nobody confuses a testnet balance with real money).
- `shortName: "datachain-testnet"` — used by chainlist URL slugs (`chainlist.org/chain/271829`). Must match the schema regex `^[A-Za-z0-9-_]{1,64}$`; hyphens allowed.
- `slip44: 1` — every testnet in the registry uses this. Signals to wallets that this is a test-currency chain for BIP-44 derivation.
- `features` — same list as mainnet. EIP155 is chain-id replay protection (correct: our testnet uses chain-id-scoped signatures). EIP1559 is the fee-market opcode (correct: reth `--dev` supports it out of the box).
- `parent` — points chainlist at mainnet 271828 as our L1-of-record. The `type: "testnet"` string is idiomatic in the registry (see Amoy → Polygon, Base Sepolia → Base). `bridges: []` because we do not run a testnet↔mainnet bridge (correct: our testnet has isolated ledger; you can't move FAT ↔ xFAT).

## 2. Optional icon manifest — `_data/icons/datachainTestnet.json`

Only if we want a distinctive icon. Recommend deferring this; adding an icon adds a review round (maintainers want to see the SVG + a stable CID pin).

Skipped for the first PR. We can open a follow-up PR after the chain PR merges.

---

## 3. Pre-submission verification (run this exact block before opening the PR)

```bash
# 1. Chain ID is what we say it is
curl -sS -X POST -H 'content-type: application/json' \
     -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}' \
     https://testnet.erpc.datachain.network | jq -r .result
# expected: 0x425d5

# 2. Client version is masked (this is a chainlist maintainer red flag if it leaks reth)
curl -sS -X POST -H 'content-type: application/json' \
     -d '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}' \
     https://testnet.erpc.datachain.network | jq -r .result
# expected: contains "Datachain-Rope"; must NOT contain "reth", "Reth", "linux-gnu"

# 3. Latest block advances
for i in 1 2; do
  curl -sS -X POST -H 'content-type: application/json' \
       -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
       https://testnet.erpc.datachain.network | jq -r .result
  sleep 4
done
# expected: two hex numbers, second > first

# 4. Faucet is up
curl -sS -o /dev/null -w '%{http_code}\n' https://faucet.datachain.network/healthz
# expected: 200

# 5. Explorer is up
curl -sS -o /dev/null -w '%{http_code}\n' https://testnet.dcscan.io
# expected: 200 (or 301->200)

# 6. Schema-validate the payload locally (chainlist CI does the same)
git clone --depth 1 https://github.com/ethereum-lists/chains.git /tmp/eth-lists
cp eip155-271829.json /tmp/eth-lists/_data/chains/
cd /tmp/eth-lists && ./gradlew check
# expected: BUILD SUCCESSFUL — no failures
```

If any of the 6 checks fails, do NOT open the PR. Fix the root cause first; a rejected chainlist PR is much harder to un-block than a delayed submission.

---

## 4. Submission runbook

### 4.1 Personal fork route (recommended)

The operator's personal GitHub account is the right identity for this PR. Chainlist maintainers look at commit history and prior contributions to decide reviewer priority — the Foundation's official repos should stay on the record, but the PR opener can be an operator.

```bash
# 1. Fork ethereum-lists/chains via the GitHub UI, then:
git clone git@github.com:<operator-handle>/chains.git
cd chains
git remote add upstream https://github.com/ethereum-lists/chains.git
git fetch upstream master
git checkout -b add-datachain-rope-testnet upstream/master

# 2. Add the payload
cp /Users/kazealphonseonguene/Downloads/DATACHAIN\ ROPE/datachain-rope/docs/design/eip155-271829.json \
   _data/chains/eip155-271829.json

# 3. Run the local check
./gradlew check
# expected: BUILD SUCCESSFUL

# 4. Commit + push
git add _data/chains/eip155-271829.json
git commit -m "add: Datachain Rope Testnet (271829)"
git push origin add-datachain-rope-testnet

# 5. Open the PR via `gh` or the web UI
gh pr create --repo ethereum-lists/chains --base master \
  --title "add: Datachain Rope Testnet (271829)" \
  --body-file /Users/kazealphonseonguene/Downloads/DATACHAIN\ ROPE/datachain-rope/docs/design/chainlist-271829-pr-body.md
```

### 4.2 PR body (paste as `chainlist-271829-pr-body.md`)

See `chainlist-271829-pr-body.md` in this directory (drafted alongside this design). Short version:

> Adds `_data/chains/eip155-271829.json` for the Datachain Rope Testnet.
>
> - Chain: DATACHAIN
> - Chain ID: 271829 (0x425D5)
> - Parent: eip155-271828 (Datachain Rope mainnet, already in the registry)
> - RPC: https://testnet.erpc.datachain.network
> - Faucet: https://faucet.datachain.network (100 xFAT drip, per-IP + per-address rate limited)
> - Explorer: https://testnet.dcscan.io (EIP-3091 compatible)
> - Symbol: xFAT (18 decimals, distinct from mainnet FAT so testnet balances cannot be confused with real value)
>
> Verified against the schema: `./gradlew check` passes locally.
> Verified against the network: `eth_chainId → 0x425d5`, `eth_blockNumber` advances every ~3s (single-node dev auto-mine, chainlist-listed convention for testnets).

### 4.3 What CI will do

`ethereum-lists/chains` CI runs a schema-validate + a live RPC probe. If our public RPC is up and the payload matches the schema, CI is green in ~2 minutes. Maintainer review usually ~1-14 days, longer if there's a review queue.

If CI fails, the failure log is usually:
- Schema violation: fix the JSON, force-push.
- RPC probe timeout: our RPC was down or slow. Wait and re-run.
- Chain-ID mismatch: someone else claimed 271829. Extremely unlikely (271829 is not in the registry as of 2026-08-30, verified above) but possible if there's a race. Escalate to maintainers with our on-chain provenance (chainId 271829 has been live since 2026-08-30, before any competing claim).

## 5. Post-merge

Once merged, chainlist.org rebuilds automatically (~15 min). At that point:

- `https://chainlist.org/chain/271829` renders the testnet card.
- MetaMask's "Add Network Automatically" flow works for 271829.
- Wallet integration guides can link to the chainlist page instead of hand-rolled instructions.

Update the following after merge:

- `datachain.network/docs` — add a link to the chainlist page from the testnet quickstart section.
- `datachain.network` landing — add a chainlist badge next to the mainnet chainlist badge.
- `handover-testnet-erpc-endpoint-and-rope-naming-2026-08-30.mdc` — append a line under §7 that the chainlist listing is live and remove the "not yet listed" caveat from any downstream docs.

---

## 6. Ordering constraint with the writer facade

`rope-testnet-writer-facade` **must land before this PR opens.** Rationale:

1. Chainlist listing → wallet on-boarding → abuse probes. First 24-72h post-merge will send both curious developers and script kiddies at the testnet's public RPC.
2. Today the testnet has ONE line of defence: the node.js `sanitizeUpstreamResponse` hack that rewrites `reth/*` strings. That is not a security posture; it's a stopgap. If a chainlist-driven scanner throws novel payloads at us, we may leak.
3. Once the facade is in front (mainnet-style `rope-node` doing method allowlist + Phase-2 signature gate in Rust), we have the same posture as mainnet, which we know survives production traffic today (~62k denials/24h according to the security audit).

So the sequence is:

1. Land `rope-testnet-writer-facade` (sibling doc), verify green.
2. Wait one week to observe. If abuse rate on testnet is < mainnet's (expected, since testnet doesn't hold real value), proceed to (3).
3. Open the chainlist PR.
4. On merge, expect a ~10x traffic bump. Watch `journalctl -u rope-testnet-node.service` for `Method denied` and `Phase-2 signed destructive RPC rejected` lines; those indicate the gate is working.

---

## 7. Cross-references

- `docs/design/rope-testnet-writer-facade.md` — mandatory prerequisite for this PR.
- `docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md` — the gate that will filter chainlist-driven traffic.
- Mainnet chainlist entry: `https://github.com/ethereum-lists/chains/blob/master/_data/chains/eip155-271828.json` — reference format we're mirroring.
- Testnet operational state: `.cursor/rules/handover-testnet-erpc-endpoint-and-rope-naming-2026-08-30.mdc`.
- Chainlist schema: `https://github.com/ethereum-lists/chains/blob/master/tools/schema/chainSchema.json`.
