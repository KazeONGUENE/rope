#!/usr/bin/env node
/**
 * CERBER R15 - unit tests for docs-drift.mjs
 *
 * Runs the drift check against an in-process HTTP server that emulates
 * each of the failure modes we saw in production:
 *   1. Nginx returning an HTML error page during a BLUE flap (must
 *      RETRY and PASS on the next tick, not page L4).
 *   2. Testnet + faucet live (must PASS, not warn "PLANNED - NOT YET
 *      DEPLOYED").
 *   3. Regressions: docs reverted to pre-milestone stub text, faucet
 *      backend fell through to the SPA, etc. (must FAIL).
 *
 * Run:
 *   node --test deploy/cerber/test/docs-drift.test.mjs
 * or the whole suite:
 *   npm test --prefix deploy/cerber
 */

import { test } from "node:test";
import { strict as assert } from "node:assert";
import { createServer } from "node:http";

import { runDriftCheck, __test__ } from "../lib/docs-drift.mjs";

/* -------------------------------------------------------------------------- */
/* Fixture builders                                                           */
/* -------------------------------------------------------------------------- */

const LIVE_DOCS_BODY = `
<!doctype html>
<html>
<head><title>Datachain Rope docs</title></head>
<body>
<h1>Datachain Rope</h1>
<p>Mainnet chainId <code>271828</code> (<code>0x425D4</code>). RPC:
<a href="https://erpc.datachain.network">erpc.datachain.network</a>.</p>
<h2>Install</h2>
<pre>curl -fsSL https://get.datachain.network | sh</pre>
<p>Or use the <a href="https://console.datachain.network/console/">console</a>.
Contributors can also
<code>git clone https://github.com/KazeONGUENE/rope.git</code> and
<code>cargo build --release -p rope-cli</code>.</p>
<h2>Testnet</h2>
<p>Testnet chainId <code>271829</code> (<code>0x425D5</code>), symbol
<b>xFAT</b>. RPC: <a href="https://testnet.erpc.datachain.network">
testnet.erpc.datachain.network</a>. Faucet:
<a href="https://faucet.datachain.network">faucet.datachain.network</a>.</p>
</body>
</html>
`;

const LIVE_FAUCET_BODY = `
<!doctype html>
<html>
<head><title>Datachain Rope testnet faucet</title></head>
<body>
<h1>xFAT faucet - testnet 271829 (0x425D5)</h1>
<form id="drip"><input id="addr" placeholder="0x..."></form>
<script>
document.getElementById('drip').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  await fetch('/api/drip', {method:'POST', body:JSON.stringify({address:'0x0'})});
});
</script>
</body>
</html>
`;

const STALE_DOCS_BODY = `
<!doctype html>
<html><body>
<p>Chain ID 271828 (0x425D4). Testnet: PLANNED - NOT YET DEPLOYED.</p>
<pre>cargo install rope-cli</pre>
<p>RPC: https://erpc.datachain.network</p>
</body></html>
`;

const STALE_FAUCET_BODY = `
<!doctype html>
<html><body>
<p>Testnet PLANNED - NOT YET DEPLOYED. Coming soon.</p>
<script>document.getElementById('drip').addEventListener('click', () =>
setTimeout(() => alert('faucet coming soon'), 1000));</script>
</body></html>
`;

/**
 * Build an in-process HTTP server that behaves like production on the
 * happy path (mainnet + testnet + faucet all live). Individual routes
 * can be overridden per test (e.g. to inject a flaky nginx error page).
 */
function buildFakeStack({
  docsBody = LIVE_DOCS_BODY,
  faucetBody = LIVE_FAUCET_BODY,
  mainnetChainId = "0x425D4",
  testnetChainId = "0x425D5",
  globalStats = { invariant_holds: true, total_strings: 149, total_knots: 790087 },
  mainnetFlapUntilAttempt = 0,
  faucetApiHandler = null,
} = {}) {
  const state = { rpcCalls: [], attempt: 0 };
  const server = createServer(async (req, res) => {
    // The intended host is encoded as the first path segment (see
    // withHostRewrite() below). Strip it so route matching sees the
    // real path.
    const raw = req.url || "/";
    const firstSlash = raw.indexOf("/", 1);
    const host = raw.slice(1, firstSlash === -1 ? raw.length : firstSlash);
    const rest = firstSlash === -1 ? "/" : raw.slice(firstSlash);
    const url = new URL(rest, "http://placeholder");

    // faucet subdomain
    if (host === "faucet.datachain.network") {
      if (req.method === "GET" && url.pathname === "/") {
        res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
        res.end(faucetBody);
        return;
      }
      if (req.method === "POST" && url.pathname === "/api/drip") {
        if (faucetApiHandler) return faucetApiHandler(req, res);
        // Default: real backend rejects invalid body with 400 JSON.
        res.writeHead(400, { "content-type": "application/json" });
        res.end(JSON.stringify({ ok: false, error: "address is not a valid EVM address" }));
        return;
      }
      res.writeHead(404);
      res.end();
      return;
    }

    // mainnet RPC
    if (host === "erpc.datachain.network") {
      if (req.method !== "POST") {
        res.writeHead(405);
        res.end();
        return;
      }
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        state.attempt += 1;
        state.rpcCalls.push({ host, body });
        if (state.attempt <= mainnetFlapUntilAttempt) {
          // Emulate nginx 502 during BLUE restart: HTML body.
          res.writeHead(502, { "content-type": "text/html" });
          res.end("<html>\n<head><title>502 Bad Gateway</title></head></html>");
          return;
        }
        let msg;
        try {
          msg = JSON.parse(body);
        } catch {
          res.writeHead(400);
          res.end();
          return;
        }
        if (msg.method === "eth_chainId") {
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: mainnetChainId }));
          return;
        }
        if (msg.method === "rope_globalStats") {
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: globalStats }));
          return;
        }
        res.writeHead(404);
        res.end();
      });
      return;
    }

    // testnet RPC
    if (host === "testnet.erpc.datachain.network") {
      if (req.method !== "POST") {
        res.writeHead(405);
        res.end();
        return;
      }
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        let msg;
        try {
          msg = JSON.parse(body);
        } catch {
          res.writeHead(400);
          res.end();
          return;
        }
        if (msg.method === "eth_chainId") {
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: testnetChainId }));
          return;
        }
        res.writeHead(404);
        res.end();
      });
      return;
    }

    // docs on datachain.network
    if (host === "datachain.network") {
      if (req.method === "GET" && url.pathname === "/docs") {
        res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
        res.end(docsBody);
        return;
      }
      res.writeHead(404);
      res.end();
      return;
    }

    res.writeHead(404);
    res.end();
  });

  return { server, state };
}

/**
 * Rewrite fetch() so that requests to the well-known production
 * hostnames land on our in-process server. Node's undici drops any
 * caller-supplied Host header (it always echoes the URL host), so
 * instead we encode the intended host as the first path segment and
 * strip it back off inside the server. Restores the original fetch
 * on teardown.
 */
function withHostRewrite(port, fn) {
  const HOST_MAP = new Set([
    "datachain.network",
    "faucet.datachain.network",
    "erpc.datachain.network",
    "testnet.erpc.datachain.network",
  ]);
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (input, init) => {
    const url = typeof input === "string" ? input : input.url;
    const parsed = new URL(url);
    if (HOST_MAP.has(parsed.hostname)) {
      const rewritten = new URL(`http://127.0.0.1:${port}`);
      rewritten.pathname = `/${parsed.hostname}${parsed.pathname}`;
      rewritten.search = parsed.search;
      return originalFetch(rewritten.toString(), init);
    }
    return originalFetch(input, init);
  };
  return fn().finally(() => {
    globalThis.fetch = originalFetch;
  });
}

async function startServer(builderOpts = {}) {
  const { server, state } = buildFakeStack(builderOpts);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  return { server, state, port };
}

async function stopServer(server) {
  await new Promise((resolve) => server.close(resolve));
}

/* -------------------------------------------------------------------------- */
/* Tests                                                                      */
/* -------------------------------------------------------------------------- */

test("happy path: live testnet + faucet + mainnet RPC = 0 fail", async () => {
  const { server, port } = await startServer();
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(check.summary.fail, 0, `unexpected fails: ${JSON.stringify(check.findings.filter((f) => f.status === "fail"))}`);
    assert.equal(byId["docs-chainid"].status, "pass");
    assert.equal(byId["docs-cli-recipe"].status, "pass");
    assert.equal(byId["docs-testnet-live"].status, "pass");
    assert.equal(byId["docs-rpc-url"].status, "pass");
    assert.equal(byId["faucet-live"].status, "pass");
    assert.equal(byId["faucet-backend"].status, "pass");
    assert.equal(byId["rpc-chainid"].status, "pass");
    assert.equal(byId["rpc-globalstats"].status, "pass");
    assert.equal(byId["rpc-testnet-chainid"].status, "pass");
  } finally {
    await stopServer(server);
  }
});

test("transient nginx 502 HTML body: retries and passes (does NOT page L4)", async () => {
  // First TWO mainnet RPC attempts return HTML; the retry loop then
  // gets a real answer on attempt 3. Both eth_chainId and
  // rope_globalStats must therefore pass.
  const { server, port } = await startServer({ mainnetFlapUntilAttempt: 2 });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(
      byId["rpc-chainid"].status,
      "pass",
      `expected pass after retry, got: ${byId["rpc-chainid"].detail}`
    );
    assert.equal(
      byId["rpc-globalstats"].status,
      "pass",
      `expected pass after retry, got: ${byId["rpc-globalstats"].detail}`
    );
  } finally {
    await stopServer(server);
  }
});

test("persistent nginx 502 HTML body: fails after retries exhausted", async () => {
  // Every attempt returns HTML; the retry loop exhausts and the RPC
  // check fails cleanly with a non-JSON diagnostic (not "Unexpected
  // token '<'").
  const { server, port } = await startServer({ mainnetFlapUntilAttempt: 999 });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(byId["rpc-chainid"].status, "fail");
    assert.match(
      byId["rpc-chainid"].detail,
      /non-JSON response/i,
      `diagnostic must mention non-JSON, got: ${byId["rpc-chainid"].detail}`
    );
    assert.equal(byId["rpc-globalstats"].status, "fail");
  } finally {
    await stopServer(server);
  }
});

test("regression: docs still say testnet is PLANNED = fail (not warn)", async () => {
  const { server, port } = await startServer({ docsBody: STALE_DOCS_BODY });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(byId["docs-testnet-live"].status, "fail");
    assert.match(byId["docs-testnet-live"].detail, /PLANNED|regressed/i);
    // And the cargo install line should still be caught.
    assert.equal(byId["docs-cli-recipe"].status, "fail");
    assert.match(byId["docs-cli-recipe"].detail, /cargo install rope-cli/);
  } finally {
    await stopServer(server);
  }
});

test("regression: faucet shows PLANNED + setTimeout(alert) stub = fail", async () => {
  const { server, port } = await startServer({ faucetBody: STALE_FAUCET_BODY });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(byId["faucet-live"].status, "fail");
    // setTimeout(alert(...)) wins over PLANNED-NOT-YET-DEPLOYED because
    // the stub pattern is checked first.
    assert.match(byId["faucet-live"].detail, /stub|PLANNED|regressed/i);
  } finally {
    await stopServer(server);
  }
});

test("regression: faucet backend falls through to SPA HTML = fail", async () => {
  // The vhost lost its /api/drip route and the SPA catch-all served
  // the marketing HTML on a POST. This must be caught.
  const spaFallthrough = (req, res) => {
    res.writeHead(200, { "content-type": "text/html" });
    res.end("<html><body>faucet SPA</body></html>");
  };
  const { server, port } = await startServer({ faucetApiHandler: spaFallthrough });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(byId["faucet-backend"].status, "fail");
    assert.match(byId["faucet-backend"].detail, /content-type|SPA|fallen/i);
  } finally {
    await stopServer(server);
  }
});

test("regression: faucet backend approves empty body = fail (ratelimit off)", async () => {
  // Simulate a very bad regression: backend returned 200 to an empty
  // body, meaning the address-validation and ratelimit shields were
  // not enforcing.
  const acceptsEmpty = (req, res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ ok: true, tx: "0xdeadbeef" }));
  };
  const { server, port } = await startServer({ faucetApiHandler: acceptsEmpty });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(byId["faucet-backend"].status, "fail");
    assert.match(byId["faucet-backend"].detail, /empty body|expected 4xx/i);
  } finally {
    await stopServer(server);
  }
});

test("regression: mainnet chainId drift = fail", async () => {
  const { server, port } = await startServer({ mainnetChainId: "0x42644" });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(byId["rpc-chainid"].status, "fail");
    assert.match(byId["rpc-chainid"].detail, /expected 0x425D4/);
  } finally {
    await stopServer(server);
  }
});

test("regression: testnet chainId drift = fail", async () => {
  const { server, port } = await startServer({ testnetChainId: "0x425D4" });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(byId["rpc-testnet-chainid"].status, "fail");
    assert.match(byId["rpc-testnet-chainid"].detail, /expected 0x425D5/);
  } finally {
    await stopServer(server);
  }
});

test("regression: global stats invariant violated = fail", async () => {
  const { server, port } = await startServer({
    globalStats: { invariant_holds: false, total_strings: 100, total_knots: 50 },
  });
  try {
    const check = await withHostRewrite(port, () => runDriftCheck());
    const byId = Object.fromEntries(check.findings.map((f) => [f.id, f]));
    assert.equal(byId["rpc-globalstats"].status, "fail");
    assert.match(byId["rpc-globalstats"].detail, /invariant_holds=false/);
  } finally {
    await stopServer(server);
  }
});

test("rpc() helper retries transparently on non-JSON body", async () => {
  const { server, port } = await startServer({ mainnetFlapUntilAttempt: 1 });
  try {
    const result = await withHostRewrite(port, () =>
      __test__.rpc("https://erpc.datachain.network", "eth_chainId", [], {
        retries: 3,
        backoffMs: 5,
      })
    );
    assert.equal(result.body.result, "0x425D4");
    assert.equal(result.attempt, 2, "should have succeeded on attempt 2");
  } finally {
    await stopServer(server);
  }
});
