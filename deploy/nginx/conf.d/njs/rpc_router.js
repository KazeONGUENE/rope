// rpc_router.js - method-aware failover for erpc.datachain.network
//
// Context (2026-07-25 incident + 2026-07-26 follow-up + 2026-07-27 P1.3):
// nginx used to fail over BOTH reads and writes across the 4-node committee.
// Because the 3 non-BLUE nodes are attester/witness-only (they never build
// blocks from their own local mempool and there is no devp2p tx-gossip
// between committee members), any eth_sendRawTransaction that failed over
// to one of them got accepted into THAT node's local tx pool and then sat
// there forever - silently unmined, with no error to the client.
//
// Additionally, Quipu personal-ledger state (`rope_*` registry / append /
// walk / globalStats) lives in a per-node RocksDB (`~/.rope/ledger_db`)
// that is NOT replicated by reth-*-sync.sh. Writes are already pinned to
// BLUE; failing over rope_* READS to GREEN/DO returned empty/stale ledgers
// (live 2026-07-27: public rope_globalStats = 2 strings while BLUE had 79).
//
// Routing rules:
//   - PRIMARY-ONLY (rpc_primary_only = BLUE):
//       * all EVM write-shaped methods
//       * all rope-node DESTRUCTIVE_METHODS (ledger mutators)
//       * EVERY method whose name starts with `rope_` (Quipu ledger is
//         BLUE-local until an explicit ledger-replication protocol exists)
//       * unparseable / empty POST bodies (fail-safe)
//   - FAILOVER (rpc_read_failover = BLUE then GREEN/DO backups):
//       * eth_* reads (eth_call, eth_getBalance, eth_blockNumber, …)
//       * net_*, web3_*, and anything else that is not rope_/write
//
// Batched JSON-RPC requests are treated as primary-only as soon as ANY
// member of the batch matches the rules above.
//
// NOTE: this nginx build's njs engine does not implement the ES6 `Set`
// object (confirmed 2026-07-26: `ReferenceError: "Set" is not defined`
// at runtime). Use a plain object as a string->bool lookup instead -
// https://nginx.org/en/docs/njs/compatibility.html

const WRITE_METHODS = {
    'eth_sendRawTransaction': true,
    'eth_sendTransaction': true,
    'eth_sign': true,
    'eth_signTransaction': true,
    'eth_signTypedData': true,
    'eth_signTypedData_v3': true,
    'eth_signTypedData_v4': true,
    'personal_sign': true,
    // Mirrors rope-node's crates/rope-node/src/rpc_auth.rs::DESTRUCTIVE_METHODS
    // (kept for documentation; the rope_ prefix rule below also covers them).
    'rope_untieKnot': true,
    'rope_erasePersonalLedger': true,
    'rope_appendToLedger': true,
    'rope_createPersonalLedger': true,
    'rope_anchorDeployerAttestation': true,
    'rope_submitTestimony': true,
    'rope_registerValidator': true,
    'rope_v2_appendKnot': true,
    'rope_v2_compact': true,
    'rope_registerDevice': true,
    'rope_ingestTelemetry': true,
    'rope_subscribeAgentToWallet': true,
    // 2026-07-29: attester mempools are not the sealer. Public txpool_*
    // failover painted "pending" for ghost writes that only lived on DO1
    // (500M FAT escrow incident). Pin pool views to BLUE so wallets see
    // the writer truth; erpc-fleet-ha reclaim injects ghosts into BLUE.
    'txpool_content': true,
    'txpool_status': true,
    'txpool_inspect': true,
};

function needsPrimaryOnly(bodyText) {
    if (!bodyText) {
        // Empty body on a POST to a JSON-RPC endpoint is malformed; fail
        // safe to primary-only rather than guessing.
        return true;
    }
    try {
        const parsed = JSON.parse(bodyText);
        const items = Array.isArray(parsed) ? parsed : [parsed];
        for (let i = 0; i < items.length; i++) {
            const m = items[i] && items[i].method;
            if (typeof m !== 'string') {
                continue;
            }
            if (WRITE_METHODS[m] === true) {
                return true;
            }
            // P1.3 (2026-07-27): Quipu personal-ledger is BLUE-local.
            // Never fail over any rope_* method (read or write).
            if (m.length >= 5 && m.substring(0, 5) === 'rope_') {
                return true;
            }
        }
        return false;
    } catch (e) {
        // Unparseable JSON: fail safe -> primary-only.
        return true;
    }
}

function route(r) {
    if (r.method !== 'POST') {
        // GET/HEAD (health checks, etc.) - never a JSON-RPC write.
        r.internalRedirect('@rpc_failover');
        return;
    }
    if (needsPrimaryOnly(r.requestText)) {
        r.internalRedirect('@rpc_primary');
    } else {
        r.internalRedirect('@rpc_failover');
    }
}

// Attester-only public read endpoint (2026-08-14).
// GET: human/machine descriptor. POST: eth_* reads via @rpc_attesters
// (GREEN/DO, never BLUE). Writes, rope_*, txpool_*, and unparseable
// bodies are rejected with JSON-RPC -32601 so this URL cannot mint a
// ghost tx the way a failover of eth_sendRawTransaction did on 2026-07-29.
function routeAttesterRead(r) {
    r.headersOut['Access-Control-Allow-Origin'] = '*';
    r.headersOut['Access-Control-Allow-Methods'] = 'GET, POST, OPTIONS';
    r.headersOut['Access-Control-Allow-Headers'] = 'Content-Type, Authorization';
    r.headersOut['Cache-Control'] = 'no-store';
    if (r.method === 'OPTIONS') {
        r.return(204);
        return;
    }
    if (r.method === 'GET' || r.method === 'HEAD') {
        r.headersOut['Content-Type'] = 'application/json';
        r.return(200, '{"ok":true,"role":"attester-read","writes":false,"url":"https://erpc.datachain.network/v1/read","note":"eth_* reads against GREEN/DO attesters only. Never send eth_sendRawTransaction or rope_* here."}\n');
        return;
    }
    if (r.method !== 'POST') {
        r.headersOut['Content-Type'] = 'application/json';
        r.return(405, '{"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"attester-read accepts POST JSON-RPC eth_* reads only"}}\n');
        return;
    }
    if (needsPrimaryOnly(r.requestText)) {
        r.headersOut['Content-Type'] = 'application/json';
        r.return(405, '{"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"Method denied on attester-read endpoint; writes and rope_* stay on https://erpc.datachain.network/"}}\n');
        return;
    }
    r.internalRedirect('@rpc_attesters');
}

export default { route, routeAttesterRead };
