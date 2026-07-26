# MapstoreEscrow Production Deploy Runbook

**Owner:** Datachain Rope agent + foundation operator
**Target:** Datachain Rope mainnet (chainId 271828, `https://erpc.datachain.network`)
**Source:** `datachain-rope/contracts/src/mapstore/MapstoreEscrow.sol`
**Tests:** `datachain-rope/contracts/test/mapstore/MapstoreEscrow.t.sol` — 38/38 PASS as of 2026-06-15
**Deploy script:** `datachain-rope/contracts/scripts/DeployMapstoreEscrow.s.sol`
**Bytecode size:** ~19 KB (well under the 24 KB EIP-170 limit)
**Estimated deploy cost:** ~2.65 M gas at 1 gwei = ~0.003 FAT (verified by live `forge script` simulation against erpc.datachain.network on 2026-06-15)

---

## When to run this runbook

Trigger: the Mapstore agent has shipped a "READY" handover and confirmed **all five** of the following addresses, ideally as a JSON block dropped into `tanastok/dcswap/ROPE` workspace under `.cursor/rules/`:

| # | Variable | Type | Purpose |
|---|---|---|---|
| 1 | `PLATFORM_TREASURY` | EOA or multisig | Receives the platform fee on every release. Mapstore treasury wallet. |
| 2 | `ESCROW_ADMIN` | Multisig (preferred) | Holds `DEFAULT_ADMIN_ROLE`. Can grant/revoke roles, update treasury, fee, dispute window. |
| 3 | `ESCROW_PLATFORM` | EOA (rotatable) | Holds `PLATFORM_ROLE`. The Mapstore API relayer EOA — buyers don't pay gas, the relayer does. |
| 4 | `ESCROW_OPERATOR` | Multisig | Holds `OPERATOR_ROLE`. Resolves disputes between buyer and payee. |
| 5 | `ESCROW_GUARDIAN` | Multisig | Holds `GUARDIAN_ROLE`. Can pause/unpause the contract in emergency. |

**Anti-foot-gun checks the deploy script enforces (compile-time, before broadcast):**

- All 5 addresses must be non-zero
- `ESCROW_ADMIN != ESCROW_PLATFORM`
- `ESCROW_ADMIN != ESCROW_OPERATOR`
- `ESCROW_OPERATOR != ESCROW_GUARDIAN`

If Mapstore's confirmation collapses two roles (e.g. admin == operator for a small-team pilot), patch `DeployMapstoreEscrow.s.sol` to comment out the matching require, document the reason in the deploy log, and proceed. **Never** silently re-use an EOA across roles in production.

---

## Step 1 — Stage the env on the deploy host

The deploy can be run from any machine with `forge` installed and the deployer's private key. The local workstation works (verified). `rope-vps` also works (`/home/ubuntu/.foundry/bin/forge`).

```bash
# In a private shell, NEVER in a tracked .env file
export PLATFORM_TREASURY=0x...                 # from Mapstore confirmation
export ESCROW_ADMIN=0x...
export ESCROW_PLATFORM=0x...
export ESCROW_OPERATOR=0x...
export ESCROW_GUARDIAN=0x...
export DEPLOYER_PRIVATE_KEY=0x...              # foundation deployer key

cd /path/to/DATACHAIN\ ROPE/datachain-rope/contracts
```

The foundation deployer is the same `0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195` used for DCSwap and Tanastok seedings (see `handover-dcswap-redeployed-2026-02-26.mdc`). Operator-only access via Tanastok/DCSwap-style provenance — the key is never committed and never logged.

## Step 2 — Final dry run

```bash
forge script scripts/DeployMapstoreEscrow.s.sol \
  --rpc-url https://erpc.datachain.network
```

Expected output (last lines):

```
MapstoreEscrow deployed at: 0x<computed-address>
Default platform fee bps  : 800
Default dispute window (s): 604800
Estimated gas price: 1.000000014 gwei
Estimated total gas used for script: ~2.6M
SIMULATION COMPLETE. To broadcast these transactions, add --broadcast ...
```

If the estimated gas is materially higher than 3M, **stop** and investigate — likely a constructor revert silently masked by a different require message.

## Step 3 — Broadcast

```bash
forge script scripts/DeployMapstoreEscrow.s.sol \
  --rpc-url https://erpc.datachain.network \
  --private-key $DEPLOYER_PRIVATE_KEY \
  --broadcast --slow --legacy
```

The `--slow` flag waits for each tx to be mined before submitting the next. `--legacy` matches the Reth backend's preferred tx envelope (no EIP-1559 priority fee).

Record the deployed contract address. It will be saved to:

```
broadcast/DeployMapstoreEscrow.s.sol/271828/run-latest.json
  → .transactions[0].contractAddress
```

Pull it with:

```bash
DEPLOYED_ADDR=$(jq -r '.transactions[0].contractAddress' \
  broadcast/DeployMapstoreEscrow.s.sol/271828/run-latest.json)
echo "MapstoreEscrow deployed at: $DEPLOYED_ADDR"
```

## Step 4 — Update `deployed_addresses.json`

```bash
jq --arg addr "$DEPLOYED_ADDR" '. + {"MapstoreEscrow": $addr}' \
  deployed_addresses.json > deployed_addresses.json.tmp \
  && mv deployed_addresses.json.tmp deployed_addresses.json
```

Commit the change with a message like `chore(contracts): record MapstoreEscrow deploy at 0x…`.

## Step 5 — Anchor the establishment knot

The treasury wallet's first knot should be a verifiable establishment record so a `/v1/search?q=Mapstore+Escrow+Established` query returns this entry at the top. Matches the pattern used by Tanastok in `handover-tanastok-treasury-address-confirmed-2026-06-04.mdc`.

```bash
# Step 5a — Create the personal ledger (idempotent; 2001 = already exists)
curl -sS -X POST https://erpc.datachain.network \
  -H 'content-type: application/json' \
  -d "{
    \"jsonrpc\":\"2.0\",
    \"method\":\"rope_createPersonalLedger\",
    \"params\":[\"$PLATFORM_TREASURY\"],
    \"id\":1
  }"

# Step 5b — Anchor the establishment knot
curl -sS -X POST https://erpc.datachain.network \
  -H 'content-type: application/json' \
  -d "{
    \"jsonrpc\":\"2.0\",
    \"method\":\"rope_appendToLedger\",
    \"params\":[\"$PLATFORM_TREASURY\", {
      \"interaction_type\": \"MapstoreEscrowEstablished\",
      \"description\": \"Mapstore marketplace escrow established on Datachain Rope. Trustless DCR-20 stablecoin escrow for service jobs and orders. Time-bounded auto-release. GDPR-friendly metadata anchoring via hash. Per-job platform fee, default 8%, cap 20%.\",
      \"metadata\": {
        \"escrow_contract\": \"$DEPLOYED_ADDR\",
        \"platform_treasury\": \"$PLATFORM_TREASURY\",
        \"admin_multisig\": \"$ESCROW_ADMIN\",
        \"platform_relayer\": \"$ESCROW_PLATFORM\",
        \"operator_multisig\": \"$ESCROW_OPERATOR\",
        \"guardian_multisig\": \"$ESCROW_GUARDIAN\",
        \"default_settlement_token\": \"0xb93bd8db94f1baff474aa9cba0739daaad01641f\",
        \"default_settlement_token_symbol\": \"USDC\",
        \"default_settlement_token_standard\": \"DCR-20\",
        \"chain_id\": 271828,
        \"default_platform_fee_bps\": 800,
        \"default_dispute_window_seconds\": 604800,
        \"established_via_runbook\": \"datachain-rope/docs/MAPSTORE_DEPLOY_RUNBOOK.md\",
        \"established_via_handover\": \"prompts/MAPSTORE_DATACHAIN_ROPE_INTEGRATION.md\",
        \"contract_bytecode_hash\": \"<fill with: cast code $DEPLOYED_ADDR --rpc-url https://erpc.datachain.network | sha256sum>\"
      }
    }],
    \"id\":2
  }"
```

Note: Phase-2 signed-RPC (`ROPE_PHASE2_SIGNED_DESTRUCTIVE=1`) is currently OFF in production per `handover-security-audit-2026-06-11.mdc` §"Phase-2 status". The `rope_*` destructive methods bypass the deny-gate when called from the rope-vps loopback (loopback + no `X-Forwarded-For`). So this curl needs to run from inside rope-vps:

```bash
ssh rope-vps "curl -sS -X POST http://127.0.0.1:8545 -H 'content-type: application/json' -d '{...above payload...}'"
```

Once Phase-2 ships, this step gets a `signed_at`/`nonce`/`signature` envelope. See `datachain-rope/docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md`.

## Step 6 — Label on dcscan.io

This is the part that touches all 4 production VPS nodes. The labels live in `crates/rope-explorer/src/main.rs` (function `address_registry()`).

### 6a. Patch the labels

Add these six entries to the `m.insert(...)` chain in `address_registry()`. Place them right after the existing "Tanastok deployer wallets" block (around line 3260 in the current file):

```rust
// Mapstore marketplace contracts and governance
m.insert(
    "<LOWERCASED_DEPLOYED_ADDR>",
    AddressTag {
        label: "Mapstore Escrow",
        category: "mapstore",
        icon: "fa-handshake",
        hidden: false,
    },
);
m.insert(
    "<LOWERCASED_PLATFORM_TREASURY>",
    AddressTag {
        label: "Mapstore Platform Treasury",
        category: "mapstore",
        icon: "fa-vault",
        hidden: false,
    },
);
m.insert(
    "<LOWERCASED_ESCROW_ADMIN>",
    AddressTag {
        label: "Mapstore Governance (Admin)",
        category: "mapstore",
        icon: "fa-shield-halved",
        hidden: false,
    },
);
m.insert(
    "<LOWERCASED_ESCROW_PLATFORM>",
    AddressTag {
        label: "Mapstore API Relayer",
        category: "mapstore",
        icon: "fa-server",
        hidden: false,
    },
);
m.insert(
    "<LOWERCASED_ESCROW_OPERATOR>",
    AddressTag {
        label: "Mapstore Operator (Disputes)",
        category: "mapstore",
        icon: "fa-gavel",
        hidden: false,
    },
);
m.insert(
    "<LOWERCASED_ESCROW_GUARDIAN>",
    AddressTag {
        label: "Mapstore Guardian (Pauser)",
        category: "mapstore",
        icon: "fa-circle-pause",
        hidden: false,
    },
);
```

Make sure all six address strings are **lowercased** (the registry compares case-sensitively).

### 6b. Roll out to all 4 production VPS

Per `handover-security-audit-2026-06-11.mdc` §"Operational gap surfaced + closed", use `deploy-fleet.sh` (not the legacy blue-green-only script):

```bash
ssh rope-vps "/opt/datachain-rope/scripts/deploy-fleet.sh full"
```

This:
1. Syncs the source change to GREEN (the canonical jammy build host)
2. Runs `cargo build --release -p dc-explorer` on GREEN
3. Distributes the freshly-built `dc-explorer` binary to GREEN → DO-1 → DO-2 → BLUE
4. Restarts `dc-explorer.service` on each node and audits the V11 gate is still active

### 6c. Verify

```bash
curl -sS "https://dcscan.io/api/v1/labels" | jq ".[\"$DEPLOYED_ADDR\"]"
# Expect: { "label": "Mapstore Escrow", "category": "mapstore", ... }

# Spot-check on every node:
for ip in 92.243.26.189 92.243.25.119 157.230.18.45 167.172.106.174; do
  echo "--- $ip ---"
  curl -sS --max-time 4 -X POST "http://$ip:8545" \
    -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_getCode\",\"params\":[\"$DEPLOYED_ADDR\",\"latest\"],\"id\":1}" \
    | jq '.result | length'  # expect non-zero (bytecode size hex string)
done
```

The page `https://dcscan.io/address/$DEPLOYED_ADDR` should now render with a "Mapstore Escrow" label and the `mapstore` category pill.

## Step 7 — Verify the contract source on dcscan.io

```bash
forge verify-contract $DEPLOYED_ADDR src/mapstore/MapstoreEscrow.sol:MapstoreEscrow \
  --chain 271828 \
  --verifier-url https://api.dcscan.io/api \
  --etherscan-api-key $ETHERSCAN_API_KEY \
  --constructor-args $(cast abi-encode \
    "constructor(address,address,address,address,address)" \
    $PLATFORM_TREASURY $ESCROW_ADMIN $ESCROW_PLATFORM $ESCROW_OPERATOR $ESCROW_GUARDIAN)
```

If `dcscan.io` verification API isn't open for third-party contracts yet, attach a verification request to a follow-up PR.

## Step 8 — Hand back to Mapstore

Drop a return handover at `/Users/kazealphonseonguene/Downloads/mapstore/.cursor/rules/handover-from-rope-mapstore-escrow-live-<date>.mdc` containing:

| Field | Value |
|---|---|
| `escrow_contract_address` | `$DEPLOYED_ADDR` |
| `deploy_tx_hash` | from `broadcast/.../run-latest.json` |
| `deploy_block` | from same file |
| `chain_id` | `271828` |
| `dcscan_url` | `https://dcscan.io/address/$DEPLOYED_ADDR` |
| `establishment_knot_event_id` | the knot id returned by `rope_appendToLedger` |
| `rpc_url` | `https://erpc.datachain.network` |
| `default_settlement_token` | `0xb93bd8db94f1baff474aa9cba0739daaad01641f` (USDC) |
| `default_platform_fee_bps` | `800` (= 8%) |
| `default_dispute_window_seconds` | `604800` (= 7 days) |

Update the Mapstore-side `.env.example` to reflect `CRYPTO_ESCROW_ADDRESS=$DEPLOYED_ADDR`. Mark the Mapstore prompt's "Phase 1 ready" status to LIVE.

## Rollback

The escrow contract is **not** upgradeable (this is intentional, per the trustlessness goal). If a critical bug is discovered post-deploy:

1. `GUARDIAN_ROLE` calls `pause()` immediately — blocks all writes.
2. Buyers whose jobs are in `Pending` can `cancelJob` for a refund (cancel is `whenNotPaused` so this requires unpausing first; in practice the operator unpauses, broadcasts the cancellations, then re-pauses).
3. For `InProgress` jobs, wait for the natural dispute window or have one party open a dispute; `OPERATOR_ROLE` resolves with the appropriate split (resolveDispute is NOT `whenNotPaused`, so it works even while paused).
4. Deploy a v2 with the fix. Treasury and governance addresses can be the same; only the bytecode changes.
5. Mapstore-side: rotate `CRYPTO_ESCROW_ADDRESS` in the API env, redeploy.

## Useful reference data

| Asset | Address |
|---|---|
| USDC (DCR-20) | `0xb93bd8db94f1baff474aa9cba0739daaad01641f` |
| USDT (DCR-20) | `0x79a26132f48394421382c13b54ae77fa3af73289` |
| EUROD (DCR-20) | `0x24d6137807fa8a592888726d87ac748d018c6d4a` |
| WFAT | `0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4` |
| Foundation deployer | `0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195` |
| Tanastok treasury (reference) | `0x63423bbc1275F973Eb00D6198B757797A8Db320B` |
| Public RPC | `https://erpc.datachain.network` |
| Public WSS | `wss://ws.datachain.network` |
| Explorer | `https://dcscan.io` |
| Chain ID | `271828` |
| FAT price feed | `https://dcswap.net/v1/prices` |

## Cross-references

- Contract: `datachain-rope/contracts/src/mapstore/MapstoreEscrow.sol`
- Tests: `datachain-rope/contracts/test/mapstore/MapstoreEscrow.t.sol`
- Deploy script: `datachain-rope/contracts/scripts/DeployMapstoreEscrow.s.sol`
- Mapstore agent prompt: `prompts/MAPSTORE_DATACHAIN_ROPE_INTEGRATION.md`
- Handover rule: `.cursor/rules/handover-mapstore-integration-2026-06-15.mdc`
- Tanastok treasury pattern (this runbook follows it): `.cursor/rules/handover-tanastok-treasury-address-confirmed-2026-06-04.mdc`
- Production fleet topology: `.cursor/rules/digitalocean-third-blue-green-slot.mdc`
- Fleet deploy script: `deploy/scripts/deploy-fleet.sh` (preferred over legacy `deploy-blue-green.sh`)
- Phase-2 signed-RPC plan: `datachain-rope/docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md`
