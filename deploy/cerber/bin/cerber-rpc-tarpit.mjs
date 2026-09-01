#!/usr/bin/env node
/**
 * CERBER R14 tarpit - JSON-RPC decoy for malicious clients.
 *
 * Policy (explicit, non-negotiable):
 *   1. This process NEVER emits retaliatory traffic. No DDoS-back, ever.
 *   2. It answers requests with a slow, plausible-but-wrong JSON-RPC reply
 *      so a scanner cannot cheaply distinguish this from a real writer.
 *   3. Every hit is logged as ndjson to /var/lib/datachain-rope/cerber/tarpit/hits-YYYY-MM-DD.ndjson
 *      for audit and later CERBER-mesh cross-checking.
 *   4. The block/quarantine decision is upstream (nginx `map` from the
 *      classifier); this process only serves the decoy.
 *
 * Wire (see deploy/nginx/conf.d/tarpit.map.conf + datachain.network.conf):
 *   map $remote_addr $rope_tarpit_flag {
 *       default        normal;
 *       include        /etc/nginx/conf.d/malicious-ips.include;  # written by classifier
 *   }
 *   if ($rope_tarpit_flag = malicious) { return 418; }           # rewrite phase
 *   error_page 418 = @rpc_tarpit;                                # -> upstream rpc_tarpit
 *   upstream rpc_tarpit { server 127.0.0.1:9099; keepalive 8; }
 *
 * Fake data is bounded (never leaks a real balance, tx, or block hash) and
 * uses a fixed low block-height, `0x0` balances, empty log arrays, and a
 * fake chain-id `0x0`.
 *
 * Env:
 *   CERBER_TARPIT_LISTEN            (default 127.0.0.1:9099)
 *   CERBER_TARPIT_DELAY_MIN_MS      (default 1500)
 *   CERBER_TARPIT_DELAY_MAX_MS      (default 6000)
 *   CERBER_TARPIT_LOG_DIR           (default /var/lib/datachain-rope/cerber/tarpit)
 */

import { createServer } from "node:http";
import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { hostname } from "node:os";

const LISTEN = (process.env.CERBER_TARPIT_LISTEN || "127.0.0.1:9099").split(":");
const HOST = LISTEN[0];
const PORT = Number(LISTEN[1] || 9099);
const DELAY_MIN = Number(process.env.CERBER_TARPIT_DELAY_MIN_MS || 1500);
const DELAY_MAX = Number(process.env.CERBER_TARPIT_DELAY_MAX_MS || 6000);
const LOG_DIR = process.env.CERBER_TARPIT_LOG_DIR || "/var/lib/datachain-rope/cerber/tarpit";

function ensureLogDir() {
  mkdirSync(LOG_DIR, { recursive: true, mode: 0o750 });
}

function logHit(rec) {
  ensureLogDir();
  const day = new Date().toISOString().slice(0, 10);
  const path = join(LOG_DIR, `hits-${day}.ndjson`);
  appendFileSync(path, JSON.stringify({ ts: new Date().toISOString(), host: hostname(), ...rec }) + "\n", { mode: 0o640 });
}

function delayMs() {
  return Math.floor(DELAY_MIN + Math.random() * Math.max(1, DELAY_MAX - DELAY_MIN));
}

const FAKE_BLOCK_NUMBER_HEX = "0x1"; // deliberately tiny; a scanner should NOT be able to distinguish easily
const FAKE_CHAIN_ID_HEX = "0x0";
const FAKE_HASH = "0x" + "0".repeat(64);
const FAKE_ADDR_ZERO = "0x" + "0".repeat(40);

function fakeReply(id, method, params) {
  const method_ = String(method || "");
  if (method_ === "eth_blockNumber") return { jsonrpc: "2.0", id, result: FAKE_BLOCK_NUMBER_HEX };
  if (method_ === "eth_chainId") return { jsonrpc: "2.0", id, result: FAKE_CHAIN_ID_HEX };
  if (method_ === "net_version") return { jsonrpc: "2.0", id, result: "0" };
  if (method_ === "web3_clientVersion")
    return { jsonrpc: "2.0", id, result: "geth/v0.0.0-fake" };
  if (method_ === "eth_getBalance") return { jsonrpc: "2.0", id, result: "0x0" };
  if (method_ === "eth_getTransactionCount") return { jsonrpc: "2.0", id, result: "0x0" };
  if (method_ === "eth_gasPrice") return { jsonrpc: "2.0", id, result: "0x0" };
  if (method_ === "eth_getCode") return { jsonrpc: "2.0", id, result: "0x" };
  if (method_ === "eth_getStorageAt") return { jsonrpc: "2.0", id, result: FAKE_HASH };
  if (method_ === "eth_call") return { jsonrpc: "2.0", id, result: "0x" };
  if (method_ === "eth_estimateGas") return { jsonrpc: "2.0", id, result: "0x5208" };
  if (method_ === "eth_getBlockByNumber" || method_ === "eth_getBlockByHash") {
    return {
      jsonrpc: "2.0",
      id,
      result: {
        number: FAKE_BLOCK_NUMBER_HEX,
        hash: FAKE_HASH,
        parentHash: FAKE_HASH,
        timestamp: "0x0",
        transactions: [],
        gasLimit: "0x0",
        gasUsed: "0x0",
        miner: FAKE_ADDR_ZERO,
      },
    };
  }
  if (method_ === "eth_getTransactionByHash" || method_ === "eth_getTransactionReceipt") {
    return { jsonrpc: "2.0", id, result: null };
  }
  if (method_ === "eth_getLogs") return { jsonrpc: "2.0", id, result: [] };
  if (method_ === "eth_sendRawTransaction") {
    // Return an internal error so the scanner cannot even build a synthetic hash.
    return { jsonrpc: "2.0", id, error: { code: -32000, message: "internal error" } };
  }
  // Rope-native reads: return "not found" shapes so a scanner learns nothing.
  if (method_ === "rope_knotIndex") return { jsonrpc: "2.0", id, result: 0 };
  if (method_ === "rope_globalStats") {
    return {
      jsonrpc: "2.0",
      id,
      result: { total_strings: 0, total_knots: 0, by_kind: {}, invariant_holds: true },
    };
  }
  if (method_ === "rope_getString" || method_ === "rope_getStringWithKnots" || method_ === "rope_listStrings") {
    return { jsonrpc: "2.0", id, result: null };
  }
  if (method_.startsWith("rope_")) {
    return { jsonrpc: "2.0", id, error: { code: -32601, message: "method not available" } };
  }
  return { jsonrpc: "2.0", id, error: { code: -32601, message: "method not found" } };
}

function readBody(req, limit = 1_000_000) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks = [];
    req.on("data", (c) => {
      size += c.length;
      if (size > limit) {
        reject(new Error("payload too large"));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    req.on("error", reject);
  });
}

function replyFor(bodyText) {
  let payload;
  try {
    payload = JSON.parse(bodyText);
  } catch {
    return { jsonrpc: "2.0", id: null, error: { code: -32700, message: "parse error" } };
  }
  if (Array.isArray(payload)) {
    return payload.map((p) => fakeReply(p?.id ?? null, p?.method, p?.params));
  }
  return fakeReply(payload?.id ?? null, payload?.method, payload?.params);
}

const server = createServer(async (req, res) => {
  const start = Date.now();
  const ip = req.headers["x-forwarded-for"]?.toString().split(",")[0].trim() || req.socket.remoteAddress || "";
  if (req.method === "GET" && (req.url === "/healthz" || req.url === "/")) {
    res.writeHead(200, { "content-type": "text/plain" });
    res.end("ok\n");
    return;
  }
  if (req.method !== "POST") {
    res.writeHead(405, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "method not allowed" }));
    return;
  }
  let body = "";
  try {
    body = await readBody(req);
  } catch (e) {
    res.writeHead(413, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "payload too large" }));
    logHit({ ip, err: String(e?.message || e) });
    return;
  }
  const reply = replyFor(body);
  const wait = delayMs();
  logHit({
    ip,
    method: (() => {
      try {
        const p = JSON.parse(body);
        return Array.isArray(p) ? p.map((x) => x?.method).join(",") : p?.method || "";
      } catch {
        return "";
      }
    })(),
    bytes: body.length,
    ua: req.headers["user-agent"] || "",
    waitMs: wait,
  });
  setTimeout(() => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(reply));
    process.stdout.write(
      `[tarpit] ip=${ip} bytes=${body.length} wait=${wait}ms rt=${Date.now() - start}ms\n`
    );
  }, wait);
});

server.listen(PORT, HOST, () => {
  process.stdout.write(`[tarpit] listening on ${HOST}:${PORT} (delay ${DELAY_MIN}-${DELAY_MAX}ms)\n`);
});

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    process.stdout.write(`[tarpit] ${sig} - shutting down\n`);
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(0), 3000).unref();
  });
}
