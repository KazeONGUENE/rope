#!/usr/bin/env node
/**
 * Aggregate CERBER mesh audit rows into a signed edge_probe POST for Rope ingest.
 *
 * Reads verify-rope audit NDJSON (rpc + fleet_status probes against erpc),
 * builds the frozen v1 body, signs with the peer mesh identity, and POSTs to
 * https://erpc.datachain.network/v1/cerber/edge-probe
 *
 *   node bin/cerber-edge-probe-push.mjs
 *
 * Env (required):
 *   CERBER_IDENTITY_KEY  - Ed25519 PEM (same as cerber-mesh.mjs)
 *   CERBER_PEER_ID       - e.g. cerber-dcswap, cerber-tanastok
 *
 * Env (optional):
 *   CERBER_AUDIT_DIR     - default /var/lib/cerber/mesh-audit
 *   CERBER_EDGE_PROBE_URL - default https://erpc.datachain.network/v1/cerber/edge-probe
 *   CERBER_EDGE_TARGET_URL - default https://erpc.datachain.network
 *   CERBER_EDGE_WINDOW_SECS - default 300
 *   CERBER_PEER_SOURCE_REGION - free-form region hint
 *   CERBER_EDGE_PROBE_QUEUE - local flush queue path (default /var/lib/cerber/edge-probe-queue.ndjson)
 */

import { appendFileSync, mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname } from "node:path";
import dns from "node:dns/promises";
import { loadIdentity } from "../lib/identity.mjs";
import { signEnvelope } from "../lib/sign.mjs";
import { readAuditRange } from "../lib/audit-store.mjs";
import { KNOWN_REASON_KEYS } from "../lib/edge-ingest.mjs";

const TARGET_URL = process.env.CERBER_EDGE_TARGET_URL ?? "https://erpc.datachain.network";
const POST_URL =
  process.env.CERBER_EDGE_PROBE_URL ?? "https://erpc.datachain.network/v1/cerber/edge-probe";
const WINDOW_SECS = Number(process.env.CERBER_EDGE_WINDOW_SECS ?? 300);
const QUEUE_PATH =
  process.env.CERBER_EDGE_PROBE_QUEUE ?? "/var/lib/cerber/edge-probe-queue.ndjson";
const EDGE_KINDS = new Set(["rpc", "fleet_status", "fleet_status_fetch"]);

function emptyReasons() {
  const o = {};
  for (const k of KNOWN_REASON_KEYS) o[k] = 0;
  return o;
}

function mapReason(raw) {
  if (!raw) return null;
  const r = String(raw).toLowerCase();
  if (r.includes("http 502")) return "http_502";
  if (r.includes("http 503")) return "http_503";
  if (r.includes("http 504")) return "http_504";
  if (r.includes("timeout") || r.includes("aborted")) return "timeout";
  if (r.includes("econnrefused") || r.includes("enotfound") || r.includes("connect")) {
    return "connect_error";
  }
  if (r.includes("tls") || r.includes("certificate")) return "tls_error";
  if (r.includes("missing_signature")) return "missing_signature";
  if (r.includes("body_hash_mismatch")) return "body_hash_mismatch";
  if (r.includes("bad_signature")) return "bad_signature";
  if (r.includes("stale_or_future")) return "stale_or_future";
  if (r.includes("untrusted")) return "untrusted_key";
  if (r.includes("empty_body")) return "empty_body";
  if (r.includes("bad_scheme")) return "bad_scheme";
  return null;
}

function pathForKind(kind) {
  if (kind === "fleet_status" || kind === "fleet_status_fetch") {
    return "/v1/fleet-status.signed.json";
  }
  return "/";
}

function aggregateWindow(rows) {
  const reasons = emptyReasons();
  const methods = {};
  const paths = new Set();
  let sampleOk = 0;
  let sampleFail = 0;

  for (const row of rows) {
    if (!EDGE_KINDS.has(row.kind)) continue;
    paths.add(pathForKind(row.kind));
    const ok = row.outcome === "verified";
    if (ok) sampleOk += 1;
    else sampleFail += 1;
    if (!ok) {
      const key = mapReason(row.reason);
      if (key) reasons[key] += 1;
    }
    if (row.kind === "rpc" && row.method) {
      const m = row.method;
      if (!methods[m]) methods[m] = { n: 0, ok: 0, fail: 0 };
      methods[m].n += 1;
      if (ok) methods[m].ok += 1;
      else methods[m].fail += 1;
    }
  }

  const sampleN = sampleOk + sampleFail;
  if (paths.size === 0) {
    paths.add("/");
    paths.add("/v1/fleet-status.signed.json");
  }

  return { sampleN, sampleOk, sampleFail, reasons, methods, targetPaths: [...paths].sort() };
}

async function resolveTargetIp() {
  const fromEnv = (process.env.CERBER_TARGET_RESOLVED_IP ?? "").trim();
  if (fromEnv) return fromEnv;
  try {
    const u = new URL(TARGET_URL);
    const res = await dns.lookup(u.hostname, { family: 4 });
    return res.address;
  } catch {
    return "0.0.0.0";
  }
}

function buildBody(identity, agg, windowStart, windowEnd, resolverIp) {
  if (agg.sampleN < 1) return null;
  const sampleN = agg.sampleN;
  const sampleOk = agg.sampleOk;
  const sampleFail = agg.sampleFail;
  const failRatio = Math.round((sampleFail / sampleN) * 1000) / 1000;
  return {
    kind: "edge_probe",
    schema: "datachain.erpc.edge-probe/v1",
    peer_id: identity.id,
    target_url: TARGET_URL,
    target_paths: agg.targetPaths,
    resolver_ip: resolverIp,
    peer_source_region: process.env.CERBER_PEER_SOURCE_REGION ?? undefined,
    window_start: windowStart,
    window_end: windowEnd,
    window_secs: windowEnd - windowStart,
    sample_n: sampleN,
    sample_ok: sampleOk,
    sample_fail: sampleFail,
    fail_ratio: failRatio,
    reasons: agg.reasons,
    methods: Object.keys(agg.methods).length ? agg.methods : undefined,
  };
}

async function postPayload(payload) {
  const res = await fetch(POST_URL, {
    method: "POST",
    headers: { "content-type": "application/json", "user-agent": "cerber-edge-probe-push/1" },
    body: JSON.stringify(payload),
    signal: AbortSignal.timeout(Number(process.env.CERBER_EDGE_PROBE_TIMEOUT_MS ?? 20_000)),
  });
  const text = await res.text();
  return { status: res.status, text };
}

function queuePayload(payload) {
  mkdirSync(dirname(QUEUE_PATH), { recursive: true, mode: 0o750 });
  appendFileSync(QUEUE_PATH, JSON.stringify({ queued_at: new Date().toISOString(), payload }) + "\n", {
    mode: 0o640,
  });
}

async function flushQueue() {
  if (!existsSync(QUEUE_PATH)) return;
  const lines = readFileSync(QUEUE_PATH, "utf8").split("\n").filter(Boolean);
  if (!lines.length) return;
  const kept = [];
  for (const line of lines) {
    let row;
    try {
      row = JSON.parse(line);
    } catch {
      continue;
    }
    try {
      const { status, text } = await postPayload(row.payload);
      if (status >= 200 && status < 300) {
        process.stdout.write(`[edge-probe-push] flushed queued post -> HTTP ${status}\n`);
      } else if (status === 429) {
        kept.push(line);
      } else if (status >= 400 && status < 500) {
        process.stderr.write(`[edge-probe-push] drop queued 4xx ${status}: ${text.slice(0, 200)}\n`);
      } else {
        kept.push(line);
      }
    } catch {
      kept.push(line);
    }
  }
  if (kept.length) writeFileSync(QUEUE_PATH, kept.join("\n") + "\n", { mode: 0o640 });
  else writeFileSync(QUEUE_PATH, "", { mode: 0o640 });
}

async function main() {
  const keyPath = process.env.CERBER_IDENTITY_KEY;
  if (!keyPath) {
    process.stderr.write("CERBER_IDENTITY_KEY unset\n");
    process.exit(1);
  }
  const identity = loadIdentity(keyPath, { peerId: process.env.CERBER_PEER_ID });
  await flushQueue();

  const now = Math.floor(Date.now() / 1000);
  const windowEnd = now;
  const windowStart = now - WINDOW_SECS;
  const sinceIso = new Date(windowStart * 1000).toISOString();
  const untilIso = new Date(windowEnd * 1000).toISOString();
  const rows = readAuditRange({ sinceIso, untilIso, limit: 50_000 });
  const agg = aggregateWindow(rows);
  const resolverIp = await resolveTargetIp();
  const body = buildBody(identity, agg, windowStart, windowEnd, resolverIp);
  if (!body) {
    process.stdout.write(
      `[edge-probe-push] ${identity.id} skip: no erpc audit samples in last ${WINDOW_SECS}s\n`
    );
    process.exit(0);
  }
  if (body.peer_source_region === undefined) delete body.peer_source_region;
  if (body.methods === undefined) delete body.methods;

  const envelope = signEnvelope(identity, { kind: "edge_probe", body });
  const payload = { body, envelope };

  let lastErr;
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const { status, text } = await postPayload(payload);
      if (status >= 200 && status < 300) {
        process.stdout.write(
          `[edge-probe-push] ${identity.id} ok HTTP ${status} window=${windowStart}-${windowEnd} n=${body.sample_n} fail=${body.sample_fail}\n`
        );
        process.exit(0);
      }
      if (status === 429) {
        queuePayload(payload);
        process.stderr.write(`[edge-probe-push] rate limited (429), queued for retry\n`);
        process.exit(0);
      }
      if (status >= 400 && status < 500) {
        process.stderr.write(`[edge-probe-push] rejected HTTP ${status}: ${text.slice(0, 400)}\n`);
        process.exit(1);
      }
      lastErr = new Error(`HTTP ${status}: ${text.slice(0, 200)}`);
    } catch (e) {
      lastErr = e;
    }
  }

  queuePayload(payload);
  process.stderr.write(`[edge-probe-push] network error, queued: ${lastErr?.message || lastErr}\n`);
  process.exit(0);
}

main().catch((e) => {
  process.stderr.write(`[edge-probe-push] fatal: ${e?.stack || e}\n`);
  process.exit(1);
});
