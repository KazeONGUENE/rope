#!/bin/bash
# Full public E2E verification of Datachain Rope Testnet + faucet UI + testnet.dcscan.io.
set -eu

FAUCET_HOST=${FAUCET_HOST:-https://faucet.datachain.network}
DCSCAN_HOST=${DCSCAN_HOST:-https://testnet.dcscan.io}
MAINNET_HOST=${MAINNET_HOST:-https://erpc.datachain.network}

cd /opt/datachain-rope/testnet

# --- 1. ephemeral address ---
echo "=== 1. generate ephemeral EVM address ==="
ADDR=$(node --input-type=module -e 'import { Wallet } from "ethers"; console.log(Wallet.createRandom().address);')
echo "recipient: $ADDR"

# --- 2. balance before ---
echo
echo "=== 2. balance before ==="
BAL_BEFORE=$(curl -sS -X POST -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBalance\",\"params\":[\"$ADDR\",\"latest\"]}" \
  "$FAUCET_HOST/rpc" | python3 -c 'import json,sys; print(int(json.load(sys.stdin)["result"],16))')
echo "balance before: $BAL_BEFORE wei"

# --- 3. drip ---
echo
echo "=== 3. drip via public HTTPS ==="
DRIP=$(curl -sS -X POST -H 'content-type: application/json' \
  -d "{\"address\":\"$ADDR\"}" "$FAUCET_HOST/api/drip")
echo "$DRIP" | python3 -m json.tool
TX=$(echo "$DRIP" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tx"])')
echo "tx: $TX"

# --- 4. wait receipt ---
echo
echo "=== 4. wait for receipt ==="
for i in 1 2 3 4 5 6; do
  R=$(curl -sS -X POST -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getTransactionReceipt\",\"params\":[\"$TX\"]}" \
    "$FAUCET_HOST/rpc")
  echo -n "attempt $i: "
  echo "$R" | python3 -c '
import json,sys
d = json.load(sys.stdin)
r = d.get("result")
if not r:
    print("pending")
else:
    bn = int(r["blockNumber"], 16)
    gu = int(r["gasUsed"], 16)
    st = r["status"]
    print("block=" + str(bn) + " status=" + st + " gasUsed=" + str(gu))
'
  echo "$R" | grep -q blockNumber && break
  sleep 2
done

# --- 5. balance after ---
echo
echo "=== 5. balance after ==="
BAL_AFTER=$(curl -sS -X POST -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBalance\",\"params\":[\"$ADDR\",\"latest\"]}" \
  "$FAUCET_HOST/rpc" | python3 -c 'import json,sys; print(int(json.load(sys.stdin)["result"],16))')
echo "balance after: $BAL_AFTER wei"
python3 -c "print(f'gained: {$BAL_AFTER/10**18} xFAT (expected 100.0)')"

# --- 6. per-address cooldown ---
echo
echo "=== 6. immediate re-drip (expect 429 / ok:false) ==="
curl -sS -o /tmp/redrip.json -w "HTTP %{http_code}\n" -X POST \
  -H 'content-type: application/json' \
  -d "{\"address\":\"$ADDR\"}" "$FAUCET_HOST/api/drip"
cat /tmp/redrip.json | python3 -m json.tool

# --- 7. testnet.dcscan.io ---
echo
echo "=== 7. testnet.dcscan.io / and /rpc + /healthz ==="
curl -sS -o /tmp/tn.html -w "HTTP %{http_code} bytes=%{size_download}\n" "$DCSCAN_HOST/"
grep -oE '<title>[^<]+</title>' /tmp/tn.html
curl -sS -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
  "$DCSCAN_HOST/rpc"
echo
curl -sS "$DCSCAN_HOST/healthz"
echo

# --- 8. mainnet-shaped paths redirect ---
echo
echo "=== 8. stale mainnet paths redirect ==="
for p in /address/0xabc /tx/0xabc /token/0xabc /tokens /blockchain/pending; do
  echo -n "$p -> "
  curl -sS -o /dev/null -w "HTTP %{http_code} loc=%{redirect_url}\n" "$DCSCAN_HOST$p"
done

# --- 9. mainnet parity ---
echo
echo "=== 9. mainnet still 271828 ==="
curl -sS -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}' \
  "$MAINNET_HOST"
echo

# --- 10. RPC allowlist ---
echo
echo "=== 10. disallowed RPC method blocked ==="
curl -sS -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"admin_peers","params":[]}' \
  "$FAUCET_HOST/rpc"
echo

# --- 11. Method mismatch surfaces ---
echo
echo "=== 11. GET /api/drip expected 405 ==="
curl -sS -o /dev/null -w "HTTP %{http_code}\n" "$FAUCET_HOST/api/drip"

# --- 12. Faucet UI ---
echo
echo "=== 12. faucet static UI ==="
curl -sS -o /tmp/faucet.html -w "HTTP %{http_code} bytes=%{size_download}\n" "$FAUCET_HOST/"
grep -oE '<title>[^<]+</title>' /tmp/faucet.html
grep -oE 'Chain ID[^<]*' /tmp/faucet.html | head -3 || true

# --- 13. Rate-limiting hint header ---
echo
echo "=== 13. nginx rate-limit headers present on /api/status ==="
curl -sSI "$FAUCET_HOST/api/status" | head -12
