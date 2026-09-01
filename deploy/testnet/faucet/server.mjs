#!/usr/bin/env node
// Datachain Rope Testnet Faucet backend (v2).
//
// Contract:
//   POST /api/drip           body: {"address":"0x..."}
//                            headers: X-Real-IP (set by nginx)
//   GET  /api/status         no body
//   POST /rpc                pass-through JSON-RPC 2.0 to reth-testnet
//                            allowlisted read + tx methods only
//   GET  /healthz            liveness
//
// Signs eth_sendRawTransaction against reth-testnet on FAUCET_RPC_URL,
// sending FAUCET_DRIP_AMOUNT_WEI to the requested address, gated by a
// per-address and per-IP rate limit persisted to FAUCET_STATE_PATH.
//
// Env is loaded from /etc/testnet-faucet.env by systemd EnvironmentFile.

import { JsonRpcProvider, Wallet, isAddress, getAddress } from "ethers";
import { createServer, request as httpRequest } from "node:http";
import { readFileSync, writeFileSync, mkdirSync, existsSync, renameSync } from "node:fs";
import { dirname } from "node:path";

const env = (k, def) => process.env[k] ?? def;
const need = (k) => {
  const v = process.env[k];
  if (!v) {
    console.error(`FATAL: env ${k} is required`);
    process.exit(2);
  }
  return v;
};

const RPC_URL = need("FAUCET_RPC_URL");
const CHAIN_ID = BigInt(need("FAUCET_CHAIN_ID"));
const PRIVATE_KEY = need("FAUCET_PRIVATE_KEY");
const FAUCET_ADDR = getAddress(need("FAUCET_ADDRESS"));
const DRIP_WEI = BigInt(need("FAUCET_DRIP_AMOUNT_WEI"));
const ADDR_COOLDOWN = Number(env("FAUCET_ADDR_COOLDOWN_SECS", "86400"));
const IP_MAX = Number(env("FAUCET_IP_MAX_PER_DAY", "3"));
const IP_WINDOW = Number(env("FAUCET_IP_WINDOW_SECS", "86400"));
const STATE_PATH = env("FAUCET_STATE_PATH", "/opt/datachain-rope/testnet/faucet/state/drip-log.json");
const LISTEN = env("FAUCET_LISTEN", "127.0.0.1:3100");

const [LISTEN_HOST, LISTEN_PORT] = LISTEN.split(":");
if (!LISTEN_HOST || !LISTEN_PORT) {
  console.error(`FATAL: FAUCET_LISTEN must be host:port, got ${LISTEN}`);
  process.exit(2);
}

const provider = new JsonRpcProvider(RPC_URL, { chainId: Number(CHAIN_ID), name: "datachain-rope-testnet" });
const wallet = new Wallet(PRIVATE_KEY, provider);
if (getAddress(wallet.address) !== FAUCET_ADDR) {
  console.error(`FATAL: FAUCET_PRIVATE_KEY derived ${wallet.address} != FAUCET_ADDRESS ${FAUCET_ADDR}`);
  process.exit(2);
}

// Parse RPC_URL into upstream host/port for the /rpc pass-through.
const upstream = new URL(RPC_URL);
if (upstream.protocol !== "http:") {
  console.error(`FATAL: FAUCET_RPC_URL must be http:// (got ${upstream.protocol})`);
  process.exit(2);
}
const UPSTREAM_HOST = upstream.hostname;
const UPSTREAM_PORT = Number(upstream.port || 80);
const UPSTREAM_PATH = upstream.pathname || "/";

// JSON-RPC method allowlist for the public /rpc pass-through.
// Deliberately excludes admin_*, personal_*, engine_*, debug_*, txpool_*
// (except txpool_status), miner_*, and anything that could leak keys or
// crash reth --dev under adversarial load.
const RPC_ALLOWLIST = new Set([
  "eth_chainId",
  "eth_blockNumber",
  "eth_getBlockByNumber",
  "eth_getBlockByHash",
  "eth_getBlockTransactionCountByHash",
  "eth_getBlockTransactionCountByNumber",
  "eth_getTransactionByHash",
  "eth_getTransactionByBlockHashAndIndex",
  "eth_getTransactionByBlockNumberAndIndex",
  "eth_getTransactionReceipt",
  "eth_getBalance",
  "eth_getCode",
  "eth_getStorageAt",
  "eth_getTransactionCount",
  "eth_getLogs",
  "eth_call",
  "eth_estimateGas",
  "eth_gasPrice",
  "eth_maxPriorityFeePerGas",
  "eth_feeHistory",
  "eth_sendRawTransaction",
  "eth_syncing",
  "eth_accounts",
  "eth_protocolVersion",
  "eth_getUncleCountByBlockHash",
  "eth_getUncleCountByBlockNumber",
  "net_version",
  "net_listening",
  "net_peerCount",
  "web3_clientVersion",
  "web3_sha3",
  "txpool_status",
]);

// ---- state --------------------------------------------------------

function ensureStateDir() {
  const d = dirname(STATE_PATH);
  if (!existsSync(d)) mkdirSync(d, { recursive: true });
}

function loadState() {
  ensureStateDir();
  if (!existsSync(STATE_PATH)) return { addrs: {}, ips: {} };
  try {
    return JSON.parse(readFileSync(STATE_PATH, "utf8"));
  } catch (e) {
    console.error(`state file ${STATE_PATH} unreadable, starting fresh: ${e.message}`);
    return { addrs: {}, ips: {} };
  }
}

function saveState(state) {
  // Real atomic replace: write to tmp then rename onto STATE_PATH.
  const tmp = `${STATE_PATH}.tmp`;
  writeFileSync(tmp, JSON.stringify(state, null, 2));
  renameSync(tmp, STATE_PATH);
}

const state = loadState();

// ---- helpers ------------------------------------------------------

function now() { return Math.floor(Date.now() / 1000); }

function isAddrOnCooldown(addrLc) {
  const last = state.addrs[addrLc];
  if (!last) return null;
  const remaining = ADDR_COOLDOWN - (now() - last);
  return remaining > 0 ? remaining : null;
}

function isIpOverLimit(ip) {
  const bucket = state.ips[ip] || [];
  const cutoff = now() - IP_WINDOW;
  const recent = bucket.filter((ts) => ts >= cutoff);
  state.ips[ip] = recent;
  return recent.length >= IP_MAX ? IP_MAX : null;
}

function recordDrip(addrLc, ip) {
  state.addrs[addrLc] = now();
  state.ips[ip] = (state.ips[ip] || []).concat(now());
  saveState(state);
}

async function readBody(req, maxBytes) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let total = 0;
    req.on("data", (c) => {
      total += c.length;
      if (total > maxBytes) {
        req.destroy();
        reject(new Error(`body exceeds ${maxBytes} bytes`));
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

async function jsonBody(req, maxBytes = 4096) {
  const buf = await readBody(req, maxBytes);
  const s = buf.toString("utf8").trim();
  if (!s) return {};
  try { return JSON.parse(s); } catch { throw new Error("invalid JSON body"); }
}

function send(res, code, obj) {
  res.writeHead(code, { "content-type": "application/json", "cache-control": "no-store" });
  res.end(JSON.stringify(obj));
}

function clientIp(req) {
  return (req.headers["x-real-ip"] || req.socket.remoteAddress || "unknown").toString();
}

// ---- handlers -----------------------------------------------------

async function handleDrip(req, res) {
  let body;
  try { body = await jsonBody(req); } catch (e) { return send(res, 400, { ok: false, error: e.message }); }
  const raw = (body.address || "").trim();
  if (!isAddress(raw)) return send(res, 400, { ok: false, error: "address is not a valid EVM address" });
  const target = getAddress(raw);
  const targetLc = target.toLowerCase();
  if (targetLc === FAUCET_ADDR.toLowerCase()) {
    return send(res, 400, { ok: false, error: "cannot drip to the faucet EOA itself" });
  }

  const ip = clientIp(req);
  const addrRemaining = isAddrOnCooldown(targetLc);
  if (addrRemaining !== null) {
    return send(res, 429, {
      ok: false,
      error: "address is on cooldown",
      retryAfterSecs: addrRemaining,
    });
  }
  const ipHit = isIpOverLimit(ip);
  if (ipHit !== null) {
    return send(res, 429, {
      ok: false,
      error: "IP has reached daily drip limit",
      ipMaxPerDay: IP_MAX,
    });
  }

  let tx;
  try {
    tx = await wallet.sendTransaction({
      to: target,
      value: DRIP_WEI,
      chainId: CHAIN_ID,
    });
  } catch (e) {
    return send(res, 502, { ok: false, error: `RPC send failed: ${e.shortMessage || e.message}` });
  }

  recordDrip(targetLc, ip);

  return send(res, 200, {
    ok: true,
    tx: tx.hash,
    to: target,
    amountWei: DRIP_WEI.toString(),
    amountXfat: (Number(DRIP_WEI) / 1e18).toString(),
    chainId: Number(CHAIN_ID),
    rpc: "https://faucet.datachain.network/rpc",
    explorer: `https://testnet.dcscan.io/`,
    note: "Testnet xFAT has no monetary value.",
  });
}

async function handleStatus(_req, res) {
  let block = null, balance = null;
  try { block = await provider.getBlockNumber(); } catch {}
  try { balance = await provider.getBalance(FAUCET_ADDR); } catch {}
  return send(res, 200, {
    ok: true,
    chainId: Number(CHAIN_ID),
    chainIdHex: "0x" + Number(CHAIN_ID).toString(16),
    network: "Datachain Rope Testnet",
    token: "xFAT",
    faucetAddress: FAUCET_ADDR,
    faucetBalanceWei: balance ? balance.toString() : null,
    faucetBalanceXfat: balance ? (Number(balance) / 1e18).toFixed(6) : null,
    dripAmountWei: DRIP_WEI.toString(),
    dripAmountXfat: (Number(DRIP_WEI) / 1e18).toString(),
    addressCooldownSecs: ADDR_COOLDOWN,
    ipMaxPerDay: IP_MAX,
    ipWindowSecs: IP_WINDOW,
    latestBlock: block,
    rpcUrl: "https://faucet.datachain.network/rpc",
  });
}

function proxyRpcOne(payload) {
  return new Promise((resolve, reject) => {
    const req = httpRequest({
      host: UPSTREAM_HOST,
      port: UPSTREAM_PORT,
      path: UPSTREAM_PATH,
      method: "POST",
      headers: {
        "content-type": "application/json",
        "content-length": Buffer.byteLength(payload),
      },
      timeout: 10_000,
    }, (upstreamRes) => {
      const chunks = [];
      upstreamRes.on("data", (c) => chunks.push(c));
      upstreamRes.on("end", () => {
        const body = Buffer.concat(chunks).toString("utf8");
        try {
          resolve(JSON.parse(body));
        } catch (e) {
          reject(new Error(`upstream returned non-JSON: ${body.slice(0, 200)}`));
        }
      });
    });
    req.on("timeout", () => { req.destroy(new Error("upstream timeout")); });
    req.on("error", reject);
    req.write(payload);
    req.end();
  });
}

async function handleRpc(req, res) {
  let body;
  try {
    body = await jsonBody(req, 65536);
  } catch (e) {
    return send(res, 400, { jsonrpc: "2.0", error: { code: -32700, message: `Parse error: ${e.message}` }, id: null });
  }

  const isBatch = Array.isArray(body);
  const calls = isBatch ? body : [body];

  // Reject if any call in the batch has a disallowed method.
  for (const c of calls) {
    if (!c || typeof c !== "object" || c.jsonrpc !== "2.0" || typeof c.method !== "string") {
      return send(res, 400, {
        jsonrpc: "2.0",
        error: { code: -32600, message: "Invalid Request: each item must be JSON-RPC 2.0" },
        id: (c && c.id) ?? null,
      });
    }
    if (!RPC_ALLOWLIST.has(c.method)) {
      return send(res, 403, {
        jsonrpc: "2.0",
        error: { code: -32601, message: `Method ${c.method} not exposed on this endpoint (testnet public allowlist)` },
        id: c.id ?? null,
      });
    }
  }

  // Forward the (possibly batched) call verbatim.
  const outboundPayload = JSON.stringify(body);
  try {
    const upstreamResp = await proxyRpcOne(outboundPayload);
    return send(res, 200, upstreamResp);
  } catch (e) {
    return send(res, 502, {
      jsonrpc: "2.0",
      error: { code: -32603, message: `Upstream RPC error: ${e.message}` },
      id: isBatch ? null : (body.id ?? null),
    });
  }
}

// ---- server -------------------------------------------------------

const server = createServer(async (req, res) => {
  // CORS - allow browser wallets and dapps to reach the testnet RPC.
  res.setHeader("access-control-allow-origin", "*");
  res.setHeader("access-control-allow-methods", "GET, POST, OPTIONS");
  res.setHeader("access-control-allow-headers", "content-type");
  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  const url = new URL(req.url, `http://${req.headers.host || "localhost"}`);
  try {
    // Treat HEAD as GET (drops body) so monitoring/health probes work.
    const method = req.method === "HEAD" ? "GET" : req.method;
    if (req.method === "HEAD") {
      const origEnd = res.end.bind(res);
      res.end = () => origEnd();
    }
    if (method === "POST" && url.pathname === "/api/drip") return handleDrip(req, res);
    if (method === "GET"  && url.pathname === "/api/status") return handleStatus(req, res);
    if (method === "POST" && url.pathname === "/rpc") return handleRpc(req, res);
    if (method === "GET"  && url.pathname === "/healthz") return send(res, 200, { ok: true });
    return send(res, 404, { ok: false, error: "not found" });
  } catch (e) {
    console.error("handler error:", e.stack || e.message);
    return send(res, 500, { ok: false, error: "internal error" });
  }
});

server.listen(Number(LISTEN_PORT), LISTEN_HOST, () => {
  console.log(`testnet-faucet listening on http://${LISTEN_HOST}:${LISTEN_PORT}`);
  console.log(`  UPSTREAM  = ${UPSTREAM_HOST}:${UPSTREAM_PORT}${UPSTREAM_PATH} (chainId ${CHAIN_ID})`);
  console.log(`  FAUCET    = ${FAUCET_ADDR}`);
  console.log(`  DRIP      = ${DRIP_WEI} wei / drip`);
  console.log(`  ADDR COOL = ${ADDR_COOLDOWN}s`);
  console.log(`  IP MAX    = ${IP_MAX} / ${IP_WINDOW}s`);
  console.log(`  STATE     = ${STATE_PATH}`);
  console.log(`  RPC ALLOW = ${RPC_ALLOWLIST.size} methods`);
});

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => { server.close(() => process.exit(0)); });
}
