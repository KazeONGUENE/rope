/**
 * CERBER R15 - docs-vs-production drift monitor.
 *
 * Checks the small set of facts a developer copies from the public docs and
 * pastes into their code / MetaMask on day one. If any one of them drifts
 * from production truth, we page contact@onguene.com.
 *
 * 2026-08-30 refresh (post-milestone build):
 *   The testnet + faucet went LIVE (chainId 271829, symbol xFAT,
 *   testnet.erpc.datachain.network, faucet.datachain.network). The two
 *   pre-milestone assertions ("docs must say testnet is PLANNED" and
 *   "faucet must show a not-yet-deployed disclaimer") therefore became
 *   stale-positive: they would page whenever the docs correctly reflected
 *   the new live state. They are inverted here so the monitor now catches
 *   the OPPOSITE regression - docs or faucet silently reverting to the
 *   pre-milestone stub language.
 *
 *   The JSON-RPC probes also got a retry-with-content-type-guard pass.
 *   Every other Rope consumer (DCSwap ResilientRopeClient, dc-explorer,
 *   bot retry loops) treats a transient HTML error page from nginx during
 *   a BLUE restart / edge failover as a retryable event. R15 now does the
 *   same, so a brief flap can no longer flip an L4 page and wake the
 *   operator at 04:07Z.
 *
 * Facts covered (2026-08-30):
 *   1. Mainnet chain-id text on /docs matches erpc.datachain.network
 *      eth_chainId (must be 271828 / 0x425D4).
 *   2. /docs does NOT recommend `cargo install rope-cli` (crate not
 *      published) and offers the two supported install paths (console +
 *      curl | sh installer). The historical `git clone + cargo build`
 *      contributor path is accepted but not required.
 *   3. /docs presents the testnet as LIVE: mentions chainId 271829,
 *      xFAT, testnet.erpc.datachain.network, faucet.datachain.network.
 *      Fails if docs regressed to "PLANNED - NOT YET DEPLOYED".
 *   4. /faucet is a real backend, not a stub: HTTP 200, mentions xFAT +
 *      271829, no interactive setTimeout(alert) stub, no wrong chain-id
 *      references (314159 / 314160), backend /api/drip returns JSON on
 *      a malformed POST.
 *   5. eth_chainId on erpc.datachain.network returns 0x425D4 exactly
 *      (retry-guarded).
 *   6. rope_globalStats on erpc.datachain.network returns
 *      invariant_holds=true (retry-guarded).
 *   7. /docs surfaces the mainnet + testnet RPC URLs.
 *   8. eth_chainId on testnet.erpc.datachain.network returns 0x425D5
 *      exactly (retry-guarded).
 *
 * Everything is done via fetch(); we never trust the disk copy on the
 * same host. That way this rule also verifies the CDN / nginx / failover
 * actually serve what we deployed.
 */

const MAINNET_HEX = "0x425D4"; // 271828
const MAINNET_DEC = "271828";
const TESTNET_HEX = "0x425D5"; // 271829
const TESTNET_DEC = "271829";
const KNOWN_WRONG_HEX = ["0x42644"]; // README typo class from KJ email
const KNOWN_WRONG_CHAIN_IDS = ["314159", "314160"]; // legacy DatabØx era

const DEFAULT_TESTNET_RPC = "https://testnet.erpc.datachain.network";

function normalize(s) {
  return String(s || "").toLowerCase();
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function fetchText(url, { timeoutMs = 15000 } = {}) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeoutMs);
  try {
    const res = await fetch(url, {
      redirect: "follow",
      signal: ctl.signal,
      headers: { "user-agent": "cerber-r15-docs-drift/1.1" },
    });
    const text = await res.text();
    return { status: res.status, headers: res.headers, text };
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Hardened JSON-RPC probe. Retries on the four transient shapes we
 * empirically see during a BLUE flap:
 *
 *   1. Non-JSON content-type (nginx error page)
 *   2. HTML body (starts with '<')
 *   3. Network error / abort
 *   4. JSON parse failure
 *
 * These are the same states DCSwap's `ResilientRopeClient` retries, and
 * the same states dc-explorer's inline retry helpers absorb. We copy the
 * pattern here so R15 can only page when the failure is durable.
 */
async function rpc(
  url,
  method,
  params = [],
  { retries = 2, timeoutMs = 10_000, backoffMs = 500 } = {}
) {
  let lastError = new Error("no attempts made");
  for (let attempt = 0; attempt <= retries; attempt += 1) {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), timeoutMs);
    let status = 0;
    let contentType = "";
    let text = "";
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "user-agent": "cerber-r15-docs-drift/1.1",
        },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
        signal: ctl.signal,
      });
      status = res.status;
      contentType = res.headers.get("content-type") || "";
      text = await res.text();
    } catch (e) {
      lastError = new Error(
        `attempt ${attempt + 1}/${retries + 1} network error: ${e?.message || e}`
      );
      clearTimeout(timer);
      if (attempt < retries) {
        await sleep(backoffMs * (attempt + 1));
        continue;
      }
      throw lastError;
    }
    clearTimeout(timer);

    // nginx serves `text/html` when it 502/503s while BLUE is restarting;
    // every downstream client we run treats this as retryable.
    const isJsonType = /^application\/(json|.+\+json)/i.test(contentType);
    const looksLikeJson = /^\s*[\{\[]/.test(text);
    if (!isJsonType && !looksLikeJson) {
      lastError = new Error(
        `attempt ${attempt + 1}/${retries + 1} non-JSON response ` +
          `(status=${status} content-type=${contentType || "?"} ` +
          `body-head=${JSON.stringify(text.slice(0, 48))})`
      );
      if (attempt < retries) {
        await sleep(backoffMs * (attempt + 1));
        continue;
      }
      throw lastError;
    }

    let body;
    try {
      body = JSON.parse(text);
    } catch (e) {
      lastError = new Error(
        `attempt ${attempt + 1}/${retries + 1} JSON parse failed: ${e?.message || e}`
      );
      if (attempt < retries) {
        await sleep(backoffMs * (attempt + 1));
        continue;
      }
      throw lastError;
    }
    if (body?.error) {
      lastError = new Error(
        `attempt ${attempt + 1}/${retries + 1} JSON-RPC error ${body.error.code}: ` +
          `${body.error.message}`
      );
      if (attempt < retries) {
        await sleep(backoffMs * (attempt + 1));
        continue;
      }
      throw lastError;
    }
    return { status, contentType, body, attempt: attempt + 1 };
  }
  throw lastError;
}

/**
 * @typedef {Object} Finding
 * @property {string} id
 * @property {'pass'|'fail'|'warn'} status
 * @property {string} detail
 */

/**
 * Run the full docs-vs-production drift check.
 * @returns {Promise<{ findings: Finding[], summary: { pass:number, warn:number, fail:number } }>}
 */
export async function runDriftCheck({
  docsUrl = "https://datachain.network/docs",
  // 2026-08-29: the honest faucet page lives on the subdomain; hitting
  // datachain.network/faucet used to silently fall through to the SPA
  // `try_files ... /index.html` and return the marketing homepage (which
  // trivially had no "not yet deployed" disclaimer, so R15 warned every
  // run). The nginx vhost now 301s /faucet -> https://faucet.datachain.network/
  // and this monitor follows the same canonical URL.
  faucetUrl = "https://faucet.datachain.network/",
  faucetApiUrl = "https://faucet.datachain.network/api/drip",
  rpcUrl = "https://erpc.datachain.network",
  testnetRpcUrl = DEFAULT_TESTNET_RPC,
} = {}) {
  const findings = [];
  const push = (id, status, detail) => findings.push({ id, status, detail });

  let docs;
  try {
    docs = await fetchText(docsUrl);
    if (docs.status !== 200) push("docs-http", "fail", `${docsUrl} returned HTTP ${docs.status}`);
    else push("docs-http", "pass", `${docsUrl} 200 (${docs.text.length} bytes)`);
  } catch (e) {
    push("docs-http", "fail", `fetch failed: ${e?.message || e}`);
    return { findings, summary: countSummary(findings) };
  }

  const docsText = docs.text;
  const docsLc = normalize(docsText);

  // 1. Mainnet chain-id references in /docs.
  const hasCorrectHex = docsLc.includes(normalize(MAINNET_HEX));
  const hasCorrectDec = docsLc.includes(MAINNET_DEC);
  const hasWrongHex = KNOWN_WRONG_HEX.some((w) => docsLc.includes(normalize(w)));
  const hasWrongDec = KNOWN_WRONG_CHAIN_IDS.some((w) => docsLc.includes(w));
  if (!hasCorrectHex || !hasCorrectDec) {
    push("docs-chainid", "fail", `docs missing correct chain-id ${MAINNET_DEC}/${MAINNET_HEX}`);
  } else if (hasWrongHex || hasWrongDec) {
    push(
      "docs-chainid",
      "fail",
      `docs contains a known-wrong chain-id (${KNOWN_WRONG_HEX.join(",")} / ${KNOWN_WRONG_CHAIN_IDS.join(",")})`
    );
  } else {
    push("docs-chainid", "pass", "correct chain-id present, no known-wrong id references");
  }

  // 2. rope-cli install recipe. Console + curl|sh are the supported paths
  //    after the 2026-08-30 milestone; the historical `git clone + cargo
  //    build` contributor path is accepted but not required.
  const hasCargoInstall = /cargo\s+install\s+rope-cli/i.test(docsText);
  const hasConsolePath = /console\.datachain\.network/i.test(docsText);
  const hasCurlInstaller = /get\.datachain\.network/i.test(docsText) &&
    /curl\s+-[a-zA-Z]*L\s+https:\/\/get\.datachain\.network/i.test(docsText);
  const hasSourceRecipe = /git\s+clone[^\n]+rope\.?git/i.test(docsText) &&
    /cargo\s+build\s+--release\s+-p\s+rope-cli/i.test(docsText);
  if (hasCargoInstall) {
    push("docs-cli-recipe", "fail", "docs still recommends `cargo install rope-cli` (crate not published)");
  } else if (!hasConsolePath || !hasCurlInstaller) {
    push(
      "docs-cli-recipe",
      "fail",
      `docs is missing a supported install path (console=${hasConsolePath}, curl|sh installer=${hasCurlInstaller}, source=${hasSourceRecipe})`
    );
  } else {
    push(
      "docs-cli-recipe",
      "pass",
      `docs surfaces console + curl|sh installer (source path also present=${hasSourceRecipe})`
    );
  }

  // 3. Testnet is LIVE. Fail if docs regressed to "PLANNED - NOT YET
  //    DEPLOYED"; require chain-id 271829, xFAT, and the testnet RPC +
  //    faucet hostnames.
  const testnetRegressed = /PLANNED\s*-\s*NOT\s+YET\s+DEPLOYED/i.test(docsText) ||
    /testnet[^\n]{0,120}(not\s+yet\s+deployed|coming\s+soon)/i.test(docsText);
  const hasTestnetDec = docsText.includes(TESTNET_DEC);
  const hasTestnetHex = docsLc.includes(normalize(TESTNET_HEX));
  const hasXfat = /xFAT/i.test(docsText);
  const hasTestnetRpcHost = /testnet\.erpc\.datachain\.network/i.test(docsText);
  const hasFaucetHost = /faucet\.datachain\.network/i.test(docsText);
  if (testnetRegressed) {
    push(
      "docs-testnet-live",
      "fail",
      "docs regressed to 'PLANNED - NOT YET DEPLOYED' language for testnet (testnet is live since 2026-08-30)"
    );
  } else if (!hasTestnetDec || !hasTestnetHex || !hasXfat || !hasTestnetRpcHost || !hasFaucetHost) {
    const missing = [];
    if (!hasTestnetDec) missing.push(TESTNET_DEC);
    if (!hasTestnetHex) missing.push(TESTNET_HEX);
    if (!hasXfat) missing.push("xFAT");
    if (!hasTestnetRpcHost) missing.push("testnet.erpc.datachain.network");
    if (!hasFaucetHost) missing.push("faucet.datachain.network");
    push("docs-testnet-live", "fail", `docs missing live-testnet references: ${missing.join(", ")}`);
  } else {
    push(
      "docs-testnet-live",
      "pass",
      `docs presents testnet as live (${TESTNET_DEC}/${TESTNET_HEX} + xFAT + testnet.erpc + faucet)`
    );
  }

  // 4. Mainnet + testnet RPC URLs surfaced in docs.
  if (!/erpc\.datachain\.network/i.test(docsText)) {
    push("docs-rpc-url", "fail", "docs does not mention https://erpc.datachain.network");
  } else if (!hasTestnetRpcHost) {
    push("docs-rpc-url", "fail", "docs mentions mainnet RPC but not https://testnet.erpc.datachain.network");
  } else {
    push("docs-rpc-url", "pass", "docs surfaces mainnet + testnet RPC hostnames");
  }

  // 5. Faucet page + backend honesty.
  let faucet;
  try {
    faucet = await fetchText(faucetUrl);
    if (faucet.status !== 200) {
      push("faucet-http", "fail", `${faucetUrl} returned HTTP ${faucet.status}`);
    } else {
      push("faucet-http", "pass", `${faucetUrl} 200 (${faucet.text.length} bytes)`);
    }
  } catch (e) {
    push("faucet-http", "fail", `fetch failed: ${e?.message || e}`);
  }

  if (faucet && faucet.status === 200) {
    const wrongIds = KNOWN_WRONG_CHAIN_IDS.filter((w) => faucet.text.includes(w));
    if (wrongIds.length) push("faucet-chainid", "fail", `faucet mentions known-wrong chain-id: ${wrongIds.join(",")}`);
    else push("faucet-chainid", "pass", "no known-wrong chain-id references");

    // Live-faucet assertions (post 2026-08-30 milestone):
    //   - no setTimeout(alert(...)) stub
    //   - mentions xFAT + testnet chain id 271829
    //   - no "PLANNED - NOT YET DEPLOYED" regression language
    const hasStubAlert = /setTimeout\s*\([^)]*alert\s*\(/i.test(faucet.text);
    const stubRegressed = /PLANNED\s*-\s*NOT\s+YET\s+DEPLOYED/i.test(faucet.text) ||
      /faucet[^\n]{0,120}(not\s+yet\s+deployed|coming\s+soon)/i.test(faucet.text);
    const faucetMentionsXfat = /xFAT/i.test(faucet.text);
    const faucetMentionsTestnetChain = faucet.text.includes(TESTNET_DEC) ||
      normalize(faucet.text).includes(normalize(TESTNET_HEX));
    if (hasStubAlert) {
      push("faucet-live", "fail", "faucet still contains a setTimeout(alert(...)) stub");
    } else if (stubRegressed) {
      push(
        "faucet-live",
        "fail",
        "faucet regressed to 'PLANNED - NOT YET DEPLOYED' language (faucet is live since 2026-08-30)"
      );
    } else if (!faucetMentionsXfat || !faucetMentionsTestnetChain) {
      const missing = [];
      if (!faucetMentionsXfat) missing.push("xFAT");
      if (!faucetMentionsTestnetChain) missing.push(`${TESTNET_DEC}/${TESTNET_HEX}`);
      push("faucet-live", "fail", `faucet missing live-testnet references: ${missing.join(", ")}`);
    } else {
      push("faucet-live", "pass", `faucet is live (mentions xFAT + testnet ${TESTNET_DEC})`);
    }

    // Backend probe: POST /api/drip with an invalid body. Real backend
    // returns 4xx application/json; a regressed vhost that falls through
    // to the SPA would return 200 text/html.
    try {
      const ctl = new AbortController();
      const t = setTimeout(() => ctl.abort(), 10_000);
      let apiRes;
      try {
        apiRes = await fetch(faucetApiUrl, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "user-agent": "cerber-r15-docs-drift/1.1",
          },
          body: JSON.stringify({}),
          signal: ctl.signal,
        });
      } finally {
        clearTimeout(t);
      }
      const apiCt = apiRes.headers.get("content-type") || "";
      const apiText = await apiRes.text();
      const isJson = /^application\/(json|.+\+json)/i.test(apiCt) ||
        /^\s*[\{\[]/.test(apiText);
      if (!isJson) {
        push(
          "faucet-backend",
          "fail",
          `POST ${faucetApiUrl} returned status=${apiRes.status} content-type=${apiCt || "?"} (expected application/json - vhost may have fallen through to the SPA)`
        );
      } else if (apiRes.status >= 200 && apiRes.status < 300) {
        // A 2xx to a bogus body would mean the backend accepted the drip,
        // which is a real regression: the ratelimit / address-validation
        // shield is not enforcing.
        push(
          "faucet-backend",
          "fail",
          `POST ${faucetApiUrl} with empty body returned HTTP ${apiRes.status} (expected 4xx JSON)`
        );
      } else {
        push(
          "faucet-backend",
          "pass",
          `POST ${faucetApiUrl} rejects empty body with HTTP ${apiRes.status} application/json`
        );
      }
    } catch (e) {
      push("faucet-backend", "fail", `POST ${faucetApiUrl} failed: ${e?.message || e}`);
    }
  }

  // 6. Live RPC truth (mainnet).
  //
  // Budget notes (2026-08-30, informed by the 20:07Z L4 page):
  //   - eth_chainId is a constant-time call; 4 attempts × 10 s each is
  //     ample (~40 s worst case + backoff).
  //   - rope_globalStats iterates the string registry and can take
  //     several seconds on a healthy edge and much longer while BLUE is
  //     restarting behind nginx. We give it 4 attempts × 20 s each so a
  //     legit slow answer converges instead of tripping AbortError.
  try {
    const res = await rpc(rpcUrl, "eth_chainId", [], {
      retries: 3,
      timeoutMs: 10_000,
      backoffMs: 500,
    });
    const got = normalize(res.body?.result);
    if (got !== normalize(MAINNET_HEX)) {
      push("rpc-chainid", "fail", `eth_chainId returned ${res.body?.result} (expected ${MAINNET_HEX})`);
    } else {
      push("rpc-chainid", "pass", `eth_chainId=${res.body.result} (attempts=${res.attempt})`);
    }
  } catch (e) {
    push("rpc-chainid", "fail", `eth_chainId call failed: ${e?.message || e}`);
  }

  try {
    const res = await rpc(rpcUrl, "rope_globalStats", [], {
      retries: 3,
      timeoutMs: 20_000,
      backoffMs: 1_000,
    });
    const result = res.body?.result;
    if (!result) push("rpc-globalstats", "fail", `rope_globalStats returned ${JSON.stringify(res.body)}`);
    else if (result.invariant_holds !== true)
      push("rpc-globalstats", "fail", `rope_globalStats.invariant_holds=${result.invariant_holds}`);
    else
      push(
        "rpc-globalstats",
        "pass",
        `rope_globalStats invariant_holds=true (strings=${result.total_strings}, attempts=${res.attempt})`
      );
  } catch (e) {
    push("rpc-globalstats", "fail", `rope_globalStats call failed: ${e?.message || e}`);
  }

  // 7. Live RPC truth (testnet).
  if (testnetRpcUrl) {
    try {
      const res = await rpc(testnetRpcUrl, "eth_chainId", [], {
        retries: 3,
        timeoutMs: 10_000,
        backoffMs: 500,
      });
      const got = normalize(res.body?.result);
      if (got !== normalize(TESTNET_HEX)) {
        push(
          "rpc-testnet-chainid",
          "fail",
          `eth_chainId on ${testnetRpcUrl} returned ${res.body?.result} (expected ${TESTNET_HEX})`
        );
      } else {
        push(
          "rpc-testnet-chainid",
          "pass",
          `${testnetRpcUrl} eth_chainId=${res.body.result} (attempts=${res.attempt})`
        );
      }
    } catch (e) {
      push("rpc-testnet-chainid", "fail", `testnet eth_chainId call failed: ${e?.message || e}`);
    }
  }

  return { findings, summary: countSummary(findings) };
}

function countSummary(findings) {
  return findings.reduce(
    (acc, f) => {
      acc[f.status] = (acc[f.status] ?? 0) + 1;
      return acc;
    },
    { pass: 0, warn: 0, fail: 0 }
  );
}

export function renderDriftReport(check) {
  const { findings, summary } = check;
  const lines = [];
  lines.push("CERBER R15 - docs-vs-production drift check");
  lines.push(`pass=${summary.pass ?? 0}  warn=${summary.warn ?? 0}  fail=${summary.fail ?? 0}`);
  lines.push("=".repeat(60));
  for (const f of findings) {
    const badge = f.status === "pass" ? "[ok]  " : f.status === "warn" ? "[warn]" : "[FAIL]";
    lines.push(`${badge} ${f.id.padEnd(24)} ${f.detail}`);
  }
  return lines.join("\n");
}

// Test-only exports (Node's `--test` importer uses these).
export const __test__ = { rpc, fetchText, countSummary };
