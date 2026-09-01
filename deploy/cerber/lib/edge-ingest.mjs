/**
 * edge.external_probes ingest library.
 *
 * Implements the frozen v1 spec at
 * `datachain-rope/docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md`.
 *
 * Two entry points:
 *   - `validateAndAccept(payload, ctx)` - pure validation + verification path.
 *     Returns `{status, code, reason?, body?}`; callers wire the HTTP response.
 *   - `startEdgeIngestServer(state, opts)` - HTTP server on 127.0.0.1:9109 that
 *     accepts `POST /v1/mesh/edge-probe`, delegates to validateAndAccept, appends
 *     the accepted body to `external-probes.ndjson`, and audits every outcome.
 *
 * No `fetch()` / no upstream calls. This is a leaf receiver; the aggregator
 * that turns the NDJSON into `fleet-status.edge.external_probes` lives in
 * `deploy/scripts/erpc-fleet-ha.sh::read_external_probes()`.
 */

import http from "node:http";
import { appendFileSync, mkdirSync, existsSync, statSync, renameSync } from "node:fs";
import { dirname } from "node:path";
import { verifyEnvelope } from "./sign.mjs";
import { recordVerified, recordRejected } from "./audit-store.mjs";
import { randomUUID } from "node:crypto";

// Spec-frozen constants. Do NOT change without a spec revision.
export const SCHEMA_ID = "datachain.erpc.edge-probe/v1";
export const KIND = "edge_probe";
export const MAX_BODY_BYTES = 32 * 1024; // 32 KiB
export const MAX_WINDOW_SECS = 3600;
export const MAX_WINDOW_END_FUTURE_SECS = 60;
// Target allowlist: lowercase, port ignored. Any other host in body.target_url
// yields schema_violation:target_url. Add hosts here in a coordinated spec
// revision, never inline via env - a compromised env should not open new hosts.
export const TARGET_ALLOWLIST = new Set([
  "erpc.datachain.network",
  "erpc.rope.network",
]);
// Reason keys the aggregator understands. Extra keys are accepted (spec 2.1)
// but ignored downstream so a malicious/mistaken peer cannot make Rope invent
// new failure categories.
export const KNOWN_REASON_KEYS = new Set([
  "http_502",
  "http_503",
  "http_504",
  "timeout",
  "connect_error",
  "tls_error",
  "empty_body",
  "body_hash_mismatch",
  "missing_signature",
  "bad_scheme",
  "stale_or_future",
  "untrusted_key",
  "bad_signature",
]);
// Per-peer soft cap: 12 posts / 60 s. Intended cadence is 1 / 300 s so this
// is 60x headroom for retries and clock skew. On breach we 429.
export const RATE_LIMIT_WINDOW_MS = 60_000;
export const RATE_LIMIT_MAX_POSTS = 12;
// NDJSON rotation threshold - spec 3.2 §10.
export const NDJSON_ROTATE_LINES = 10_000;

export function ndjsonPath() {
  return process.env.CERBER_EDGE_PROBES_FILE
    ?? "/var/lib/datachain-rope/fleet/external-probes.ndjson";
}

/**
 * Build the trusted-keys map exactly as `mesh.mjs::trustedFromConfig` does but
 * exposed here for the ingest server (which may run stand-alone without the
 * full mesh state).
 */
export function buildTrustedKeys(peersConfig) {
  const map = {};
  for (const peer of peersConfig?.peers ?? []) {
    if (!peer.public_key) continue;
    const hex = String(peer.public_key).replace(/^0x/, "").toLowerCase();
    map[peer.id] = hex;
    if (peer.kid) map[peer.kid] = hex;
    map[hex] = true;
  }
  return map;
}

/**
 * In-memory rate-limit state. Injected via `state.rateLimit` so tests can
 * fabricate a fresh Map per case.
 */
export function createRateLimitState() {
  return { posts: new Map(), pruneAt: Date.now() + RATE_LIMIT_WINDOW_MS };
}

function pruneRateLimit(rl, nowMs) {
  if (nowMs < rl.pruneAt) return;
  const cutoff = nowMs - RATE_LIMIT_WINDOW_MS;
  for (const [peer, arr] of rl.posts) {
    const kept = arr.filter((t) => t >= cutoff);
    if (kept.length === 0) rl.posts.delete(peer);
    else rl.posts.set(peer, kept);
  }
  rl.pruneAt = nowMs + RATE_LIMIT_WINDOW_MS;
}

/**
 * Returns `{ok:true}` if peer is under the cap, else `{ok:false, retryAfter}`.
 * On accept the current post is recorded.
 */
export function admitRateLimit(rl, peerId, nowMs = Date.now()) {
  pruneRateLimit(rl, nowMs);
  const arr = rl.posts.get(peerId) ?? [];
  const cutoff = nowMs - RATE_LIMIT_WINDOW_MS;
  const recent = arr.filter((t) => t >= cutoff);
  if (recent.length >= RATE_LIMIT_MAX_POSTS) {
    const oldest = recent[0];
    const retryAfterSecs = Math.max(1, Math.ceil((oldest + RATE_LIMIT_WINDOW_MS - nowMs) / 1000));
    return { ok: false, retryAfter: retryAfterSecs };
  }
  recent.push(nowMs);
  rl.posts.set(peerId, recent);
  return { ok: true };
}

/**
 * Validate + verify one probe post. Returns:
 *   { status: 202, body: {...} }              on success
 *   { status: 400, reason: "schema_violation:<field>", body: {...} }
 *   { status: 401, reason: "<verify-fail>", body: {...} }
 *   { status: 413, reason: "body_too_large", body: {...} }
 *   { status: 429, reason: "rate_limited", body: {...}, retryAfter: N }
 *
 * `ctx` must include:
 *   - peersConfig: parsed peers.production.json
 *   - trustedKeys: map from buildTrustedKeys(peersConfig)
 *   - rateLimit: createRateLimitState()
 *   - now: unix seconds override (tests); defaults to Date.now()/1000
 *   - contentLength: raw byte length of the POST body (for 413)
 *   - persist: (recordedAt, body) => void  (side effect; tests can no-op)
 */
export function validateAndAccept(payload, ctx) {
  const now = ctx.now ?? Math.floor(Date.now() / 1000);
  const respond = (status, reason, extra = {}) => ({
    status,
    reason,
    body: {
      accepted: status === 202,
      ...(reason ? { reason } : {}),
      server_time: now,
      ...extra,
    },
  });

  if (typeof ctx.contentLength === "number" && ctx.contentLength > MAX_BODY_BYTES) {
    return respond(413, "body_too_large");
  }
  if (!payload || typeof payload !== "object") {
    return respond(400, "schema_violation:payload");
  }
  const { body, envelope } = payload;
  if (!body || typeof body !== "object") return respond(400, "schema_violation:body");
  if (!envelope || typeof envelope !== "object") return respond(400, "schema_violation:envelope");

  // Kind / schema locks (spec 2.1 + 3.2 step 3).
  if (body.kind !== KIND) return respond(400, "schema_violation:kind");
  if (body.schema !== SCHEMA_ID) return respond(400, "schema_violation:schema");
  if (envelope.kind !== KIND) return respond(400, "schema_violation:envelope_kind");

  // peer_id agreement + registry presence (spec 3.2 step 4).
  if (typeof envelope.peer_id !== "string" || !envelope.peer_id) {
    return respond(400, "schema_violation:envelope_peer_id");
  }
  if (body.peer_id !== envelope.peer_id) {
    return respond(400, "schema_violation:peer_id_mismatch");
  }
  const peer = (ctx.peersConfig?.peers ?? []).find((p) => p.id === envelope.peer_id);
  if (!peer) return respond(401, "unknown_peer");
  if (!peer.public_key) return respond(401, "peer_key_unpinned");

  // public_key must match pinned pubkey exactly (spec 3.2 step 5).
  const envHex = String(envelope.public_key ?? "").replace(/^0x/, "").toLowerCase();
  const pinnedHex = String(peer.public_key).replace(/^0x/, "").toLowerCase();
  if (envHex !== pinnedHex) {
    return respond(401, "public_key_mismatch");
  }

  // Rate limit BEFORE crypto to keep an unauthenticated peer from burning CPU
  // via bad signatures. Peer is authenticated by pinned pubkey at this point.
  const rl = admitRateLimit(ctx.rateLimit, envelope.peer_id, (ctx.nowMs ?? now * 1000));
  if (!rl.ok) {
    return respond(429, "rate_limited", { retry_after_secs: rl.retryAfter });
  }

  // Cryptographic verification.
  const v = verifyEnvelope(envelope, body, { trustedKeys: ctx.trustedKeys, now });
  if (!v.ok) {
    return respond(401, v.reason || "bad_signature");
  }

  // Cross-field invariants (spec 2.1).
  const err = validateBodyInvariants(body, now);
  if (err) return respond(400, err);

  // Target allowlist (spec 4.2).
  const host = extractHost(body.target_url);
  if (!host || !TARGET_ALLOWLIST.has(host)) {
    return respond(400, "schema_violation:target_url");
  }

  // Persist. On persist failure we return 500 without audit-recording success -
  // caller's server-side error path handles this. Tests pass a no-op persist.
  try {
    ctx.persist(now, body);
  } catch (e) {
    return { status: 500, reason: `persist_failed:${e?.message || e}`, body: { accepted: false, reason: "persist_failed", server_time: now } };
  }

  return {
    status: 202,
    body: {
      accepted: true,
      peer_id: envelope.peer_id,
      recorded_at: now,
      window_end: body.window_end,
      server_time: now,
    },
  };
}

/**
 * Extract lowercase hostname from a URL, ignore port + trailing dot. Returns
 * null on parse failure or missing hostname. Reject any non-https to keep the
 * publisher on TLS.
 */
export function extractHost(url) {
  if (typeof url !== "string") return null;
  let u;
  try {
    u = new URL(url);
  } catch {
    return null;
  }
  if (u.protocol !== "https:") return null;
  const host = u.hostname.toLowerCase().replace(/\.$/, "");
  return host || null;
}

/**
 * Validate cross-field invariants on `body`. Returns null on success, else the
 * spec reason string `schema_violation:<field>`.
 */
export function validateBodyInvariants(body, nowSecs) {
  if (typeof body.target_url !== "string" || !body.target_url) return "schema_violation:target_url";
  if (!Array.isArray(body.target_paths) || body.target_paths.length === 0) return "schema_violation:target_paths";
  for (const p of body.target_paths) {
    if (typeof p !== "string" || !p.startsWith("/")) return "schema_violation:target_paths";
  }
  if (typeof body.resolver_ip !== "string" || !body.resolver_ip) return "schema_violation:resolver_ip";
  if (!Number.isInteger(body.window_start)) return "schema_violation:window_start";
  if (!Number.isInteger(body.window_end)) return "schema_violation:window_end";
  if (body.window_start >= body.window_end) return "schema_violation:window_start";
  if (body.window_end > nowSecs + MAX_WINDOW_END_FUTURE_SECS) return "schema_violation:window_end";
  if (body.window_end - body.window_start > MAX_WINDOW_SECS) return "schema_violation:window_secs";
  if (!Number.isInteger(body.window_secs)) return "schema_violation:window_secs";
  if (body.window_secs !== body.window_end - body.window_start) return "schema_violation:window_secs";
  if (!Number.isInteger(body.sample_n) || body.sample_n < 1) return "schema_violation:sample_n";
  if (!Number.isInteger(body.sample_ok) || body.sample_ok < 0) return "schema_violation:sample_ok";
  if (!Number.isInteger(body.sample_fail) || body.sample_fail < 0) return "schema_violation:sample_fail";
  if (body.sample_ok + body.sample_fail !== body.sample_n) return "schema_violation:sample_sum";
  if (typeof body.fail_ratio !== "number" || !Number.isFinite(body.fail_ratio)) return "schema_violation:fail_ratio";
  if (body.fail_ratio < 0 || body.fail_ratio > 1) return "schema_violation:fail_ratio";
  const expectedRatio = body.sample_n === 0 ? 0 : body.sample_fail / body.sample_n;
  if (Math.abs(expectedRatio - body.fail_ratio) > 0.001) return "schema_violation:fail_ratio";
  if (!body.reasons || typeof body.reasons !== "object" || Array.isArray(body.reasons)) {
    return "schema_violation:reasons";
  }
  let reasonsSum = 0;
  for (const [k, v] of Object.entries(body.reasons)) {
    if (typeof v !== "number" || !Number.isInteger(v) || v < 0) return "schema_violation:reasons";
    if (KNOWN_REASON_KEYS.has(k)) reasonsSum += v;
  }
  if (reasonsSum > body.sample_fail) return "schema_violation:reasons_sum";
  if (body.methods !== undefined) {
    if (!body.methods || typeof body.methods !== "object" || Array.isArray(body.methods)) {
      return "schema_violation:methods";
    }
    for (const stats of Object.values(body.methods)) {
      if (!stats || typeof stats !== "object" || Array.isArray(stats)) return "schema_violation:methods";
      if (!Number.isInteger(stats.n) || stats.n < 0) return "schema_violation:methods";
      if (!Number.isInteger(stats.ok) || stats.ok < 0) return "schema_violation:methods";
      if (!Number.isInteger(stats.fail) || stats.fail < 0) return "schema_violation:methods";
      if (stats.ok + stats.fail !== stats.n) return "schema_violation:methods";
    }
  }
  return null;
}

/**
 * Append one `{recorded_at, body}` line to the NDJSON store. Rotates the file
 * to `<file>.1` when it crosses NDJSON_ROTATE_LINES.
 *
 * This is called from ctx.persist and is stubbed in tests to keep them
 * hermetic. Callers that want durability should not swap out this function.
 */
export function appendExternalProbe(recordedAt, body, filePathOverride) {
  const path = filePathOverride ?? ndjsonPath();
  mkdirSync(dirname(path), { recursive: true, mode: 0o750 });
  const line = JSON.stringify({ recorded_at: recordedAt, body }) + "\n";
  appendFileSync(path, line, { mode: 0o640 });
  // Rotate lazily: cheap size check via stat; full line count would be
  // expensive on hot paths, so approximate at 32 KiB average line * threshold.
  try {
    const st = statSync(path);
    if (st.size > NDJSON_ROTATE_LINES * 4096) {
      renameSync(path, path + ".1");
    }
  } catch {
    /* rotation is best-effort */
  }
  return path;
}

async function readBody(req, cap = MAX_BODY_BYTES) {
  const chunks = [];
  let size = 0;
  for await (const c of req) {
    size += c.length;
    if (size > cap) {
      // Drain then throw; keeps the socket healthy.
      const err = new Error("body_too_large");
      err.code = "BODY_TOO_LARGE";
      throw err;
    }
    chunks.push(c);
  }
  const raw = Buffer.concat(chunks).toString("utf8");
  return { raw, size };
}

function jsonResponse(res, status, body, headers = {}) {
  const raw = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(raw),
    "access-control-allow-origin": "*",
    ...headers,
  });
  res.end(raw);
}

/**
 * Start the edge-ingest HTTP server. Loopback-bound by default so it can only
 * be reached from the local nginx (docker bridge → host.docker.internal).
 */
export function startEdgeIngestServer(state, { port, host } = {}) {
  const listenPort = Number(port ?? process.env.CERBER_EDGE_INGEST_PORT ?? 9109);
  const listenHost = host ?? process.env.CERBER_EDGE_INGEST_HOST ?? "127.0.0.1";
  const rateLimit = state.rateLimit ?? createRateLimitState();

  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url || "/", `http://${req.headers.host || "localhost"}`);
      if (req.method === "GET" && url.pathname === "/healthz") {
        return jsonResponse(res, 200, { ok: true, service: "cerber-edge-ingest" });
      }
      if (req.method !== "POST" || url.pathname !== "/v1/mesh/edge-probe") {
        return jsonResponse(res, 404, { accepted: false, reason: "not_found" });
      }
      const contentLength = Number(req.headers["content-length"] ?? 0);
      if (contentLength > MAX_BODY_BYTES) {
        return jsonResponse(res, 413, { accepted: false, reason: "body_too_large" });
      }
      let raw;
      try {
        const parsed = await readBody(req);
        raw = parsed.raw;
      } catch (e) {
        if (e.code === "BODY_TOO_LARGE") {
          return jsonResponse(res, 413, { accepted: false, reason: "body_too_large" });
        }
        return jsonResponse(res, 400, { accepted: false, reason: "read_error" });
      }
      let payload;
      try {
        payload = raw ? JSON.parse(raw) : null;
      } catch {
        return jsonResponse(res, 400, { accepted: false, reason: "json_parse_error" });
      }

      const result = validateAndAccept(payload, {
        peersConfig: state.peersConfig,
        trustedKeys: state.trustedKeys,
        rateLimit,
        contentLength: Buffer.byteLength(raw || ""),
        persist: (recordedAt, body) => {
          if (state.persistOverride) return state.persistOverride(recordedAt, body);
          appendExternalProbe(recordedAt, body, state.ndjsonPath);
        },
      });

      // Audit the outcome.
      if (result.status === 202) {
        try {
          recordVerified({
            id: randomUUID(),
            kind: "edge_probe_ingest",
            peer_id: payload.envelope?.peer_id,
            window_end: payload.body?.window_end,
            sample_n: payload.body?.sample_n,
            fail_ratio: payload.body?.fail_ratio,
          });
        } catch {
          /* audit failure never blocks accept */
        }
      } else {
        try {
          recordRejected({
            id: randomUUID(),
            kind: "edge_probe_ingest",
            peer_id: payload?.envelope?.peer_id,
            reason: result.reason,
            http_status: result.status,
          });
        } catch {
          /* audit failure never blocks reject */
        }
      }

      const extraHeaders = result.status === 429 && result.body?.retry_after_secs
        ? { "retry-after": String(result.body.retry_after_secs) }
        : {};
      return jsonResponse(res, result.status, result.body, extraHeaders);
    } catch (e) {
      return jsonResponse(res, 500, { accepted: false, reason: `server_error:${e?.message || e}` });
    }
  });

  server.listen(listenPort, listenHost);
  return { server, port: listenPort, host: listenHost, rateLimit };
}
