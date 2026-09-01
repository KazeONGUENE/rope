#!/usr/bin/env node
/**
 * CERBER R14 - unit tests for rpc-classify.mjs
 *
 * Run: node deploy/cerber/test/rpc-classify.test.mjs
 * Exit code 0 = all pass, 1 = at least one fail.
 *
 * No test framework dependency: pure Node.js assert.
 */

import { strict as assert } from "node:assert";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  parseAccessLine,
  bucketByIp,
  classifyBucket,
  summarizeBuckets,
  defaultThresholds,
  writeMaliciousIps,
  readMaliciousIps,
  SCANNER_UA_PATTERNS,
  SCANNER_PATH_PATTERNS,
} from "../lib/rpc-classify.mjs";

const tests = [];
function test(name, fn) {
  tests.push({ name, fn });
}

// --------------------------------------------------------------------- parser

test("parseAccessLine parses a real production log line", () => {
  const line =
    '159.65.119.231 - - [04/Aug/2026:16:13:51 +0000] "POST / HTTP/1.1" 200 3673 "-" ' +
    '"-" ua_len=256 rt=0.020 uc=0.009 ur=0.019 us=200 ua=167.172.106.174:8545 uct=-';
  const r = parseAccessLine(line);
  assert.ok(r, "expected a parsed record");
  assert.equal(r.ip, "159.65.119.231");
  assert.equal(r.method, "POST");
  assert.equal(r.path, "/");
  assert.equal(r.status, 200);
  assert.equal(r.bytes, 3673);
  assert.equal(r.rt, 0.02);
  assert.equal(r.upstream, "167.172.106.174:8545");
});

test("parseAccessLine returns null on garbage input", () => {
  assert.equal(parseAccessLine(""), null);
  assert.equal(parseAccessLine("total garbage"), null);
  assert.equal(parseAccessLine(null), null);
});

test("parseAccessLine handles missing user agent (dash-only)", () => {
  const line =
    '10.0.0.1 - - [04/Aug/2026:16:13:51 +0000] "GET /healthz HTTP/1.1" 200 3 "-" ' +
    '"-" ua_len=20 rt=0.001 uc=- ur=- us=- ua=- uct=-';
  const r = parseAccessLine(line);
  assert.ok(r);
  assert.equal(r.ua, "");
  assert.equal(r.upstream, null);
});

// -------------------------------------------------------------- classification

function mkAgg(overrides = {}) {
  return {
    ip: "1.2.3.4",
    total: 100,
    firstSeenMs: Date.now() - 3600_000,
    lastSeenMs: Date.now(),
    byStatus: new Map([[200, 100]]),
    byMethod: new Map([["POST", 100]]),
    byPath: new Map([["/", 100]]),
    byUa: new Map([["ethers.js/6.9", 100]]),
    byUpstream: new Map(),
    rts: [0.01, 0.02, 0.03],
    bytesTotal: 100000,
    error4xx: 0,
    error5xx: 0,
    ok2xx: 100,
    ...overrides,
  };
}

test("classifyBucket returns normal for well-behaved integrator", () => {
  const c = classifyBucket(mkAgg());
  assert.equal(c.level, "normal");
});

test("classifyBucket promotes on scanner UA", () => {
  const agg = mkAgg({ byUa: new Map([["Mozilla/5.0 zgrab/0.x", 5]]) });
  const c = classifyBucket(agg);
  assert.equal(c.level, "malicious");
  assert.match(c.reasons.join(" "), /scanner UA/);
});

test("classifyBucket promotes on scanner path", () => {
  const agg = mkAgg({ byPath: new Map([["/.env", 1], ["/", 1]]) });
  const c = classifyBucket(agg);
  assert.equal(c.level, "malicious");
});

test("classifyBucket promotes on high request rate", () => {
  const agg = mkAgg({
    total: 20_000,
    firstSeenMs: Date.now() - 3_600_000, // 1h span
    lastSeenMs: Date.now(),
  });
  const c = classifyBucket(agg);
  assert.equal(c.level, "malicious");
  assert.match(c.reasons.join(" "), /rate .* malicious/);
});

test("classifyBucket promotes on high 4xx ratio (only above minSamples)", () => {
  const agg = mkAgg({
    total: 200,
    error4xx: 180,
    byStatus: new Map([[404, 180], [200, 20]]),
  });
  const c = classifyBucket(agg);
  assert.equal(c.level, "malicious");
  assert.match(c.reasons.join(" "), /4xx ratio/);
});

test("classifyBucket does NOT apply ratio rule below minSamples", () => {
  const agg = mkAgg({
    total: 5,
    error4xx: 5,
    byStatus: new Map([[404, 5]]),
  });
  const c = classifyBucket(agg);
  assert.notEqual(c.level, "malicious");
});

test("SCANNER_UA_PATTERNS covers the known-hostile UA family", () => {
  const hostileUAs = ["nmap 7", "masscan/1.3", "zgrab/0.5", "Nikto/2", "Censys survey"];
  for (const ua of hostileUAs) {
    assert.ok(
      SCANNER_UA_PATTERNS.some((re) => re.test(ua)),
      `expected ${ua} to match a scanner UA pattern`
    );
  }
});

test("SCANNER_PATH_PATTERNS matches scanner probe paths", () => {
  const paths = ["/.git/config", "/.env", "/wp-admin/", "/phpmyadmin/", "/xmlrpc.php", "/actuator/env"];
  for (const p of paths) {
    assert.ok(
      SCANNER_PATH_PATTERNS.some((re) => re.test(p)),
      `expected ${p} to match a scanner path pattern`
    );
  }
});

// ---------------------------------------------------------- summarize + write

test("summarizeBuckets ranks malicious first", () => {
  const buckets = new Map();
  buckets.set("1.1.1.1", mkAgg({ ip: "1.1.1.1", total: 50 })); // normal
  buckets.set("2.2.2.2", mkAgg({ ip: "2.2.2.2", byUa: new Map([["masscan", 50]]) })); // malicious
  const s = summarizeBuckets(buckets);
  assert.equal(s.rows[0].ip, "2.2.2.2");
  assert.equal(s.rows[0].level, "malicious");
  assert.equal(s.totals.malicious, 1);
  assert.equal(s.totals.normal, 1);
});

test("writeMaliciousIps writes a nginx-safe map partial (no duplicate `default`)", () => {
  const tmp = mkdtempSync(join(tmpdir(), "cerber-r14-"));
  try {
    process.env.CERBER_R14_MALICIOUS_IPS_PATH = join(tmp, "mal.txt");
    const buckets = new Map();
    buckets.set("9.9.9.9", mkAgg({ ip: "9.9.9.9", byUa: new Map([["Nmap", 5]]) }));
    const summary = summarizeBuckets(buckets);
    const res = writeMaliciousIps(summary);
    assert.ok(res.added.includes("9.9.9.9"));
    const readBack = readMaliciousIps(process.env.CERBER_R14_MALICIOUS_IPS_PATH);
    assert.ok(readBack.has("9.9.9.9"));
    // The include file must NOT declare `default normal;` - that would collide
    // with the outer map block in tarpit.map.conf.
    const raw = readFileSync(process.env.CERBER_R14_MALICIOUS_IPS_PATH, "utf8");
    assert.doesNotMatch(raw, /^default\s+/m);
  } finally {
    rmSync(tmp, { recursive: true, force: true });
    delete process.env.CERBER_R14_MALICIOUS_IPS_PATH;
  }
});

// -------------------------------------------------------------------- runner

let failed = 0;
for (const t of tests) {
  try {
    t.fn();
    process.stdout.write(`ok    ${t.name}\n`);
  } catch (e) {
    failed += 1;
    process.stdout.write(`FAIL  ${t.name}\n  ${e?.message || e}\n`);
  }
}
process.stdout.write(`\n${tests.length - failed}/${tests.length} passed\n`);
process.exit(failed > 0 ? 1 : 0);
