import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ensureIdentity } from "../lib/identity.mjs";
import { signEnvelope, wrapSigned } from "../lib/sign.mjs";
import {
  SCHEMA_ID,
  KIND,
  MAX_BODY_BYTES,
  RATE_LIMIT_MAX_POSTS,
  KNOWN_REASON_KEYS,
  buildTrustedKeys,
  createRateLimitState,
  admitRateLimit,
  validateAndAccept,
  extractHost,
  validateBodyInvariants,
  appendExternalProbe,
  startEdgeIngestServer,
} from "../lib/edge-ingest.mjs";

// Shared helper: build a well-formed body for peer `peerId` at `now`, then
// return { body, envelope, persistCalls } wrapped by wrapSigned(identity,...).
function buildValidPost({ identity, now, overrides = {} }) {
  const body = {
    schema: SCHEMA_ID,
    kind: KIND,
    peer_id: identity.id,
    target_url: "https://erpc.datachain.network",
    target_paths: ["/", "/v1/fleet-status"],
    resolver_ip: "92.243.26.189",
    window_start: now - 300,
    window_end: now - 10,
    window_secs: 290,
    sample_n: 30,
    sample_ok: 28,
    sample_fail: 2,
    fail_ratio: 2 / 30,
    reasons: { http_502: 1, http_504: 1 },
    ...overrides,
  };
  return wrapSigned(identity, KIND, body);
}

function buildCtx({ peersConfig, now, persistSink }) {
  return {
    peersConfig,
    trustedKeys: buildTrustedKeys(peersConfig),
    rateLimit: createRateLimitState(),
    now,
    nowMs: now * 1000,
    contentLength: 0, // caller sets when it matters
    persist: (recordedAt, body) => persistSink?.push({ recordedAt, body }),
  };
}

test("extractHost accepts https + strips trailing dot + rejects http", () => {
  assert.equal(extractHost("https://erpc.datachain.network"), "erpc.datachain.network");
  assert.equal(extractHost("https://ERPC.DATACHAIN.NETWORK./"), "erpc.datachain.network");
  assert.equal(extractHost("http://erpc.datachain.network"), null);
  assert.equal(extractHost("not-a-url"), null);
  assert.equal(extractHost(null), null);
  assert.equal(extractHost(""), null);
});

test("buildTrustedKeys derives peer_id, kid, and hex-true entries", () => {
  const cfg = {
    peers: [
      { id: "cerber-x", kid: "abcd", public_key: "0xDEADBEEF" },
      { id: "cerber-y", public_key: "cafebabe" },
      { id: "cerber-empty" }, // no public_key -> skipped
    ],
  };
  const map = buildTrustedKeys(cfg);
  assert.equal(map["cerber-x"], "deadbeef");
  assert.equal(map["abcd"], "deadbeef");
  assert.equal(map["deadbeef"], true);
  assert.equal(map["cerber-y"], "cafebabe");
  assert.equal(map["cafebabe"], true);
  assert.equal(map["cerber-empty"], undefined);
});

test("admitRateLimit permits up to cap then 429s with retry-after", () => {
  const rl = createRateLimitState();
  const now = 1_000_000_000_000;
  for (let i = 0; i < RATE_LIMIT_MAX_POSTS; i++) {
    const r = admitRateLimit(rl, "peer-a", now + i);
    assert.equal(r.ok, true, `post ${i} should be admitted`);
  }
  const blocked = admitRateLimit(rl, "peer-a", now + RATE_LIMIT_MAX_POSTS);
  assert.equal(blocked.ok, false);
  assert.ok(blocked.retryAfter >= 1);
  // A different peer is unaffected.
  const otherPeer = admitRateLimit(rl, "peer-b", now + RATE_LIMIT_MAX_POSTS);
  assert.equal(otherPeer.ok, true);
});

test("admitRateLimit prunes old entries after window", () => {
  const rl = createRateLimitState();
  const t0 = 1_000_000_000_000;
  for (let i = 0; i < RATE_LIMIT_MAX_POSTS; i++) {
    admitRateLimit(rl, "peer-a", t0 + i);
  }
  // 61s later - all old posts pruned.
  const later = admitRateLimit(rl, "peer-a", t0 + 61_000);
  assert.equal(later.ok, true);
});

test("validateBodyInvariants happy path returns null", () => {
  const now = Math.floor(Date.now() / 1000);
  const body = {
    target_url: "https://erpc.datachain.network",
    target_paths: ["/"],
    resolver_ip: "1.2.3.4",
    window_start: now - 300,
    window_end: now - 10,
    window_secs: 290,
    sample_n: 10,
    sample_ok: 9,
    sample_fail: 1,
    fail_ratio: 0.1,
    reasons: { http_502: 1 },
  };
  assert.equal(validateBodyInvariants(body, now), null);
});

test("validateBodyInvariants rejects sample_sum mismatch", () => {
  const now = Math.floor(Date.now() / 1000);
  const bad = {
    target_url: "https://x",
    target_paths: ["/"],
    resolver_ip: "1.2.3.4",
    window_start: now - 300,
    window_end: now - 10,
    window_secs: 290,
    sample_n: 10,
    sample_ok: 5,
    sample_fail: 3, // 5+3=8 != 10
    fail_ratio: 0.3,
    reasons: {},
  };
  assert.equal(validateBodyInvariants(bad, now), "schema_violation:sample_sum");
});

test("validateBodyInvariants rejects fail_ratio inconsistent with samples", () => {
  const now = Math.floor(Date.now() / 1000);
  const bad = {
    target_url: "https://x",
    target_paths: ["/"],
    resolver_ip: "1.2.3.4",
    window_start: now - 300,
    window_end: now - 10,
    window_secs: 290,
    sample_n: 10,
    sample_ok: 5,
    sample_fail: 5,
    fail_ratio: 0.9, // should be 0.5
    reasons: {},
  };
  assert.equal(validateBodyInvariants(bad, now), "schema_violation:fail_ratio");
});

test("validateBodyInvariants rejects window_secs mismatch", () => {
  const now = Math.floor(Date.now() / 1000);
  const bad = {
    target_url: "https://x",
    target_paths: ["/"],
    resolver_ip: "1.2.3.4",
    window_start: now - 300,
    window_end: now - 10,
    window_secs: 999, // should be 290
    sample_n: 1,
    sample_ok: 1,
    sample_fail: 0,
    fail_ratio: 0,
    reasons: {},
  };
  assert.equal(validateBodyInvariants(bad, now), "schema_violation:window_secs");
});

test("validateBodyInvariants rejects future window_end beyond 60s", () => {
  const now = Math.floor(Date.now() / 1000);
  const bad = {
    target_url: "https://x",
    target_paths: ["/"],
    resolver_ip: "1.2.3.4",
    window_start: now - 60,
    window_end: now + 3600, // way in the future
    window_secs: 3660,
    sample_n: 1,
    sample_ok: 1,
    sample_fail: 0,
    fail_ratio: 0,
    reasons: {},
  };
  assert.equal(validateBodyInvariants(bad, now), "schema_violation:window_end");
});

test("validateBodyInvariants rejects reasons summing above sample_fail", () => {
  const now = Math.floor(Date.now() / 1000);
  const bad = {
    target_url: "https://x",
    target_paths: ["/"],
    resolver_ip: "1.2.3.4",
    window_start: now - 300,
    window_end: now - 10,
    window_secs: 290,
    sample_n: 10,
    sample_ok: 8,
    sample_fail: 2,
    fail_ratio: 0.2,
    reasons: { http_502: 5, http_504: 3 }, // sums to 8 > 2 sample_fail
  };
  assert.equal(validateBodyInvariants(bad, now), "schema_violation:reasons_sum");
});

test("validateBodyInvariants ignores unknown reason keys in sum but keeps them", () => {
  const now = Math.floor(Date.now() / 1000);
  const body = {
    target_url: "https://x",
    target_paths: ["/"],
    resolver_ip: "1.2.3.4",
    window_start: now - 300,
    window_end: now - 10,
    window_secs: 290,
    sample_n: 10,
    sample_ok: 9,
    sample_fail: 1,
    fail_ratio: 0.1,
    // "malicious_extra_reason" is unknown but positive; must NOT count toward sum
    reasons: { http_502: 1, malicious_extra_reason: 99 },
  };
  assert.equal(validateBodyInvariants(body, now), null);
});

test("validateAndAccept happy path returns 202 + persists body", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-happy-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-happy" });
  const peersConfig = {
    peers: [{ id: id.id, role: "test", url: "http://127.0.0.1:0", public_key: id.publicKeyHex, kid: id.kid }],
  };
  const now = Math.floor(Date.now() / 1000);
  const post = buildValidPost({ identity: id, now });
  const persistSink = [];
  const ctx = buildCtx({ peersConfig, now, persistSink });
  ctx.contentLength = JSON.stringify(post).length;
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 202, JSON.stringify(r));
  assert.equal(r.body.accepted, true);
  assert.equal(r.body.peer_id, id.id);
  assert.equal(persistSink.length, 1);
  assert.equal(persistSink[0].body.peer_id, id.id);
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept rejects body_too_large with 413", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-large-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-lg" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex }] };
  const ctx = buildCtx({ peersConfig, now: Math.floor(Date.now() / 1000), persistSink: [] });
  ctx.contentLength = MAX_BODY_BYTES + 1;
  const r = validateAndAccept({ any: "thing" }, ctx);
  assert.equal(r.status, 413);
  assert.equal(r.reason, "body_too_large");
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept rejects wrong schema id", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-schema-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-sch" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex, kid: id.kid }] };
  const now = Math.floor(Date.now() / 1000);
  const post = buildValidPost({ identity: id, now, overrides: { schema: "malicious/vsomething" } });
  const ctx = buildCtx({ peersConfig, now, persistSink: [] });
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 400);
  assert.equal(r.reason, "schema_violation:schema");
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept rejects peer_id mismatch body vs envelope", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-pid-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-pid" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex, kid: id.kid }] };
  const now = Math.floor(Date.now() / 1000);
  const post = buildValidPost({ identity: id, now, overrides: { peer_id: "cerber-other" } });
  const ctx = buildCtx({ peersConfig, now, persistSink: [] });
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 400);
  assert.equal(r.reason, "schema_violation:peer_id_mismatch");
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept rejects unknown peer with 401", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-unk-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-unregistered" });
  const peersConfig = { peers: [] }; // no peers registered at all
  const now = Math.floor(Date.now() / 1000);
  const post = buildValidPost({ identity: id, now });
  const ctx = buildCtx({ peersConfig, now, persistSink: [] });
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 401);
  assert.equal(r.reason, "unknown_peer");
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept rejects public_key_mismatch when envelope key != pinned", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-pk-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const idA = ensureIdentity(join(dir, "a.pem"), { peerId: "cerber-a" });
  const idB = ensureIdentity(join(dir, "b.pem"), { peerId: "cerber-a" }); // same id, different key
  // Registry pins idA's pubkey for cerber-a; envelope will carry idB's key.
  const peersConfig = { peers: [{ id: "cerber-a", role: "test", url: "x", public_key: idA.publicKeyHex, kid: idA.kid }] };
  const now = Math.floor(Date.now() / 1000);
  const post = buildValidPost({ identity: idB, now });
  const ctx = buildCtx({ peersConfig, now, persistSink: [] });
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 401);
  assert.equal(r.reason, "public_key_mismatch");
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept rejects tampered body via body_hash_mismatch", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-tamper-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-tmp" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex, kid: id.kid }] };
  const now = Math.floor(Date.now() / 1000);
  const post = buildValidPost({ identity: id, now });
  // Tamper: change sample_ok after signing.
  post.body.sample_ok = 27;
  post.body.sample_fail = 3;
  post.body.fail_ratio = 3 / 30;
  const ctx = buildCtx({ peersConfig, now, persistSink: [] });
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 401);
  assert.equal(r.reason, "body_hash_mismatch");
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept rejects stale signature", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-stale-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-st" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex, kid: id.kid }] };
  const now = Math.floor(Date.now() / 1000);
  // Sign at now - 7200s (well past the 600s freshness window).
  const body = {
    schema: SCHEMA_ID, kind: KIND, peer_id: id.id,
    target_url: "https://erpc.datachain.network", target_paths: ["/"], resolver_ip: "1.2.3.4",
    window_start: now - 7500, window_end: now - 7210, window_secs: 290,
    sample_n: 1, sample_ok: 1, sample_fail: 0, fail_ratio: 0, reasons: {},
  };
  const envelope = signEnvelope(id, { kind: KIND, body, signedAt: now - 7200 });
  const post = { body, envelope };
  const ctx = buildCtx({ peersConfig, now, persistSink: [] });
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 401);
  assert.equal(r.reason, "stale_or_future");
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept rejects target_url not in allowlist", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-target-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-tgt" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex, kid: id.kid }] };
  const now = Math.floor(Date.now() / 1000);
  const post = buildValidPost({ identity: id, now, overrides: { target_url: "https://attacker.example" } });
  const ctx = buildCtx({ peersConfig, now, persistSink: [] });
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 400);
  assert.equal(r.reason, "schema_violation:target_url");
  rmSync(dir, { recursive: true, force: true });
});

test("validateAndAccept enforces rate limit and returns 429 with retry_after_secs", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-rl-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-rl" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex, kid: id.kid }] };
  const now = Math.floor(Date.now() / 1000);
  const ctx = buildCtx({ peersConfig, now, persistSink: [] });
  // Prefill the peer's rate-limit slot to the cap.
  for (let i = 0; i < RATE_LIMIT_MAX_POSTS; i++) {
    admitRateLimit(ctx.rateLimit, id.id, now * 1000 + i);
  }
  const post = buildValidPost({ identity: id, now });
  const r = validateAndAccept(post, ctx);
  assert.equal(r.status, 429);
  assert.equal(r.reason, "rate_limited");
  assert.ok(r.body.retry_after_secs >= 1);
  rmSync(dir, { recursive: true, force: true });
});

test("appendExternalProbe writes NDJSON line to override path", () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-nd-"));
  const path = join(dir, "sub/dir/probes.ndjson");
  const now = Math.floor(Date.now() / 1000);
  appendExternalProbe(now, { peer_id: "x", sample_n: 5 }, path);
  appendExternalProbe(now + 1, { peer_id: "y", sample_n: 6 }, path);
  const text = readFileSync(path, "utf8").trim().split("\n").map((l) => JSON.parse(l));
  assert.equal(text.length, 2);
  assert.equal(text[0].body.peer_id, "x");
  assert.equal(text[1].body.peer_id, "y");
  assert.equal(text[0].recorded_at, now);
  rmSync(dir, { recursive: true, force: true });
});

test("HTTP server accepts a signed probe and appends to NDJSON", async () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-srv-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-srv" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex, kid: id.kid }] };
  const ndjsonPath = join(dir, "probes.ndjson");
  const state = {
    peersConfig,
    trustedKeys: buildTrustedKeys(peersConfig),
    ndjsonPath,
    rateLimit: createRateLimitState(),
  };
  const { server, port } = startEdgeIngestServer(state, { port: 0, host: "127.0.0.1" });
  await new Promise((r) => server.once("listening", r));
  const addr = server.address();
  const listenPort = typeof addr === "object" ? addr.port : port;

  const now = Math.floor(Date.now() / 1000);
  const post = buildValidPost({ identity: id, now });
  const res = await fetch(`http://127.0.0.1:${listenPort}/v1/mesh/edge-probe`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(post),
  });
  assert.equal(res.status, 202);
  const json = await res.json();
  assert.equal(json.accepted, true);
  assert.equal(json.peer_id, id.id);
  assert.ok(existsSync(ndjsonPath));
  const line = JSON.parse(readFileSync(ndjsonPath, "utf8").trim().split("\n")[0]);
  assert.equal(line.body.peer_id, id.id);
  server.close();
  rmSync(dir, { recursive: true, force: true });
});

test("HTTP server 404s on wrong path and 200s on /healthz", async () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-404-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const id = ensureIdentity(join(dir, "id.pem"), { peerId: "cerber-404" });
  const peersConfig = { peers: [{ id: id.id, role: "test", url: "x", public_key: id.publicKeyHex, kid: id.kid }] };
  const state = {
    peersConfig,
    trustedKeys: buildTrustedKeys(peersConfig),
    ndjsonPath: join(dir, "probes.ndjson"),
    rateLimit: createRateLimitState(),
  };
  const { server, port } = startEdgeIngestServer(state, { port: 0, host: "127.0.0.1" });
  await new Promise((r) => server.once("listening", r));
  const addr = server.address();
  const listenPort = typeof addr === "object" ? addr.port : port;

  const notFound = await fetch(`http://127.0.0.1:${listenPort}/v1/mesh/other`, { method: "POST" });
  assert.equal(notFound.status, 404);

  const health = await fetch(`http://127.0.0.1:${listenPort}/healthz`);
  assert.equal(health.status, 200);
  const h = await health.json();
  assert.equal(h.ok, true);
  assert.equal(h.service, "cerber-edge-ingest");

  server.close();
  rmSync(dir, { recursive: true, force: true });
});

test("HTTP server returns 400 on malformed JSON", async () => {
  const dir = mkdtempSync(join(tmpdir(), "edge-mal-"));
  process.env.CERBER_AUDIT_DIR = join(dir, "audit");
  const state = {
    peersConfig: { peers: [] },
    trustedKeys: {},
    ndjsonPath: join(dir, "probes.ndjson"),
    rateLimit: createRateLimitState(),
  };
  const { server, port } = startEdgeIngestServer(state, { port: 0, host: "127.0.0.1" });
  await new Promise((r) => server.once("listening", r));
  const addr = server.address();
  const listenPort = typeof addr === "object" ? addr.port : port;

  const res = await fetch(`http://127.0.0.1:${listenPort}/v1/mesh/edge-probe`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "not json {",
  });
  assert.equal(res.status, 400);
  const j = await res.json();
  assert.equal(j.reason, "json_parse_error");
  server.close();
  rmSync(dir, { recursive: true, force: true });
});

test("KNOWN_REASON_KEYS matches the 13 canonical reasons in the spec", () => {
  assert.equal(KNOWN_REASON_KEYS.size, 13);
  for (const k of [
    "http_502", "http_503", "http_504", "timeout", "connect_error",
    "tls_error", "empty_body", "body_hash_mismatch", "missing_signature",
    "bad_scheme", "stale_or_future", "untrusted_key", "bad_signature",
  ]) {
    assert.ok(KNOWN_REASON_KEYS.has(k), `missing reason key: ${k}`);
  }
});
