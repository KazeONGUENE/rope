## Datachain Rope Testnet (chainId 271829)

Adds `_data/chains/eip155-271829.json` for the public testnet of Datachain Rope. The mainnet is already registered at [eip155-271828](https://github.com/ethereum-lists/chains/blob/master/_data/chains/eip155-271828.json); this PR registers its sibling testnet.

### Details

| Field | Value |
|---|---|
| **Name** | Datachain Rope Testnet |
| **Chain family** | DATACHAIN (same as mainnet) |
| **Chain ID** | `271829` (`0x425D5`) |
| **Parent chain** | `eip155-271828` (Datachain Rope mainnet) |
| **Native currency** | Testnet xFAT (`xFAT`, 18 decimals — deliberately distinct from mainnet FAT so testnet balances cannot be confused with real value) |
| **RPC** | https://testnet.erpc.datachain.network |
| **Faucet** | https://faucet.datachain.network (100 xFAT drip, per-IP + per-address rate limited) |
| **Explorer** | https://testnet.dcscan.io (EIP-3091 compatible) |
| **Features** | EIP-155 replay protection, EIP-1559 fee market |
| **slip44** | `1` (BIP-44 testnet marker) |

### Verification

Local schema-validate is green:

```console
$ ./gradlew check
BUILD SUCCESSFUL
```

Live RPC verification against the public endpoint:

```console
$ curl -sS -X POST -H 'content-type: application/json' \
       -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}' \
       https://testnet.erpc.datachain.network | jq -r .result
0x425d5

$ curl -sS -X POST -H 'content-type: application/json' \
       -d '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}' \
       https://testnet.erpc.datachain.network | jq -r .result
Datachain-Rope/0.1.0-testnet

$ curl -sS -o /dev/null -w '%{http_code}\n' https://faucet.datachain.network/healthz
200

$ curl -sS -o /dev/null -w '%{http_code}\n' https://testnet.dcscan.io
200
```

### Notes for reviewers

- No bridge between mainnet FAT and testnet xFAT (`parent.bridges: []` reflects that intentionally). Testnet is a fully isolated ledger — it exists for contract-integration testing and wallet on-boarding walkthroughs, not for any economic activity.
- Explorer implements EIP-3091 (`/tx/:hash`, `/block/:number`, `/address/:addr`, `/token/:addr`) so wallets that deep-link on-chain artefacts work correctly.
- Faucet accepts one drip per address per 24h and one drip per IP per hour. If a maintainer wants to test the drip flow, the faucet UI at https://faucet.datachain.network takes any valid EVM address; balance shows up on the explorer within one block (~3s auto-mine).
