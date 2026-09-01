import http from "node:http";
import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { wrapSigned, verifyEnvelope } from "./sign.mjs";
import { recordVerified, recordRejected } from "./audit-store.mjs";
import { randomUUID } from "node:crypto";

const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * CERBER mesh — authenticated HTTP gossip of signed interaction digests.
 * Every peer (Rope, DCSwap, Tanastok, Alteros) runs the same protocol.
 *
 * Endpoints:
 *   GET  /v1/cerber/healthz
 *   GET  /v1/cerber/peer-info          — local identity + trusted peer ids
 *   GET  /v1/cerber/report             — latest detailed verification report
 *   POST /v1/cerber/ingest             — { body, envelope } from a peer
 *   POST /v1/cerber/heartbeat          — signed liveness
 *   GET  /v1/cerber/mesh-status        — peer reachability + last verified
 */

export function loadPeersConfig(path) {
  const p =
    path ||
    process.env.CERBER_PEERS_FILE ||
    join(__dirname, "../config/peers.production.json");
  return JSON.parse(readFileSync(p, "utf8"));
}

function trustedFromConfig(cfg) {
  const map = {};
  for (const peer of cfg.peers || []) {
    if (!peer.public_key) continue;
    const hex = peer.public_key.replace(/^0x/, "");
    map[peer.id] = hex;
    if (peer.kid) map[peer.kid] = hex;
    map[hex] = true;
  }
  return map;
}

export function createMeshState(identity, peersConfig) {
  return {
    identity,
    peersConfig,
    trusted: trustedFromConfig(peersConfig),
    peerStatus: Object.fromEntries(
      (peersConfig.peers || []).map((p) => [
        p.id,
        { url: p.url, role: p.role, reachable: null, last_heartbeat_at: null, last_error: null, last_report_coverage: null },
      ])
    ),
    latestReport: null,
    ingestCount: 0,
    startedAt: Math.floor(Date.now() / 1000),
  };
}

export function persistPeerKeys(identity, peersConfig, path) {
  const outPath =
    path ||
    process.env.CERBER_PEERS_RUNTIME ||
    "/var/lib/datachain-rope/cerber/peers.runtime.json";
  mkdirSync(dirname(outPath), { recursive: true, mode: 0o750 });
  const peers = (peersConfig.peers || []).map((p) =>
    p.id === identity.id
      ? { ...p, public_key: identity.publicKeyHex, kid: identity.kid, url: p.url }
      : p
  );
  // Ensure self is listed
  if (!peers.some((p) => p.id === identity.id)) {
    peers.push({
      id: identity.id,
      role: process.env.CERBER_PEER_ROLE || "rope",
      url: process.env.CERBER_PUBLIC_URL || "http://127.0.0.1:9107",
      public_key: identity.publicKeyHex,
      kid: identity.kid,
    });
  }
  const doc = { ...peersConfig, peers, updated_at: new Date().toISOString() };
  writeFileSync(outPath, JSON.stringify(doc, null, 2), { mode: 0o640 });
  return outPath;
}

function json(res, code, obj) {
  const body = JSON.stringify(obj);
  res.writeHead(code, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(body),
    "access-control-allow-origin": "*",
  });
  res.end(body);
}

async function readJson(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  const raw = Buffer.concat(chunks).toString("utf8");
  if (!raw) return null;
  return JSON.parse(raw);
}

export function startMeshServer(state, { port, host } = {}) {
  const listenPort = Number(port ?? process.env.CERBER_MESH_PORT ?? 9107);
  const listenHost = host ?? process.env.CERBER_MESH_HOST ?? "0.0.0.0";

  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url || "/", `http://${req.headers.host || "localhost"}`);
      if (req.method === "GET" && url.pathname === "/v1/cerber/healthz") {
        return json(res, 200, { ok: true, peer_id: state.identity.id, kid: state.identity.kid });
      }
      if (req.method === "GET" && url.pathname === "/v1/cerber/peer-info") {
        return json(res, 200, {
          peer_id: state.identity.id,
          kid: state.identity.kid,
          public_key: state.identity.publicKeyHex,
          public_key_pem: state.identity.publicKeyPem,
          role: process.env.CERBER_PEER_ROLE || "rope",
          mesh_schema: "datachain.cerber.mesh/v1",
          trusted_peer_ids: (state.peersConfig.peers || []).map((p) => p.id),
          started_at: state.startedAt,
        });
      }
      if (req.method === "GET" && url.pathname === "/v1/cerber/report") {
        if (!state.latestReport) return json(res, 404, { error: "no_report_yet" });
        return json(res, 200, state.latestReport);
      }
      if (req.method === "GET" && url.pathname === "/v1/cerber/mesh-status") {
        return json(res, 200, {
          schema: "datachain.cerber.mesh-status/v1",
          self: state.identity.id,
          peers: state.peerStatus,
          ingest_count: state.ingestCount,
          latest_coverage_pct: state.latestReport?.body?.coverage_pct ?? null,
          latest_all_verified: state.latestReport?.body?.all_verified ?? null,
        });
      }
      if (req.method === "POST" && url.pathname === "/v1/cerber/heartbeat") {
        const payload = await readJson(req);
        const v = verifyEnvelope(payload?.envelope, payload?.body, { trustedKeys: state.trusted });
        if (!v.ok) {
          recordRejected({ id: randomUUID(), kind: "mesh_heartbeat", reason: v.reason, peer_id: payload?.envelope?.peer_id });
          return json(res, 401, { ok: false, reason: v.reason });
        }
        const pid = payload.envelope.peer_id;
        if (state.peerStatus[pid]) {
          state.peerStatus[pid].reachable = true;
          state.peerStatus[pid].last_heartbeat_at = payload.body?.ts || payload.envelope.signed_at;
          state.peerStatus[pid].last_error = null;
        }
        // Enroll key if operator allowed bootstrap and peer was keyless
        if (process.env.CERBER_TRUST_BOOTSTRAP === "1" && !state.trusted[pid]) {
          state.trusted[pid] = payload.envelope.public_key.replace(/^0x/, "");
          state.trusted[payload.envelope.public_key.replace(/^0x/, "")] = true;
        }
        recordVerified({ id: randomUUID(), kind: "mesh_heartbeat", peer_id: pid, outcome: "verified" });
        return json(res, 200, { ok: true });
      }
      if (req.method === "POST" && url.pathname === "/v1/cerber/ingest") {
        const payload = await readJson(req);
        const v = verifyEnvelope(payload?.envelope, payload?.body, { trustedKeys: state.trusted });
        if (!v.ok) {
          recordRejected({
            id: randomUUID(),
            kind: "mesh_ingest",
            reason: v.reason,
            peer_id: payload?.envelope?.peer_id,
          });
          return json(res, 401, { ok: false, reason: v.reason });
        }
        state.ingestCount += 1;
        const pid = payload.envelope.peer_id;
        if (state.peerStatus[pid]) {
          state.peerStatus[pid].reachable = true;
          state.peerStatus[pid].last_report_coverage = payload.body?.coverage_pct ?? null;
          state.peerStatus[pid].last_error = null;
        }
        recordVerified({
          id: randomUUID(),
          kind: "mesh_ingest",
          peer_id: pid,
          envelope_kind: payload.envelope.kind,
          body_sha256: payload.envelope.body_sha256,
          outcome: "verified",
        });
        return json(res, 200, { ok: true, ingest_count: state.ingestCount });
      }
      return json(res, 404, { error: "not_found" });
    } catch (e) {
      return json(res, 500, { error: String(e.message || e) });
    }
  });

  server.listen(listenPort, listenHost);
  return { server, port: listenPort, host: listenHost };
}

export async function meshHeartbeat(state) {
  const body = {
    ts: Math.floor(Date.now() / 1000),
    peer_id: state.identity.id,
    kid: state.identity.kid,
    role: process.env.CERBER_PEER_ROLE || "rope",
  };
  const signed = wrapSigned(state.identity, "mesh_heartbeat", body);
  await fanout(state, "/v1/cerber/heartbeat", signed);
  return signed;
}

export async function meshPublishReport(state, signedReport) {
  state.latestReport = signedReport;
  // Persist latest for GET /report and local report CLI
  const path = process.env.CERBER_LATEST_REPORT ?? "/var/lib/datachain-rope/cerber/latest-report.json";
  mkdirSync(dirname(path), { recursive: true, mode: 0o750 });
  writeFileSync(path, JSON.stringify(signedReport, null, 2), { mode: 0o640 });
  await fanout(state, "/v1/cerber/ingest", signedReport);
  return signedReport;
}

async function fanout(state, path, payload) {
  const selfId = state.identity.id;
  for (const peer of state.peersConfig.peers || []) {
    if (peer.id === selfId) continue;
    if (!peer.url) continue;
    const url = peer.url.replace(/\/$/, "") + path;
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json", "user-agent": "cerber-mesh/fanout" },
        body: JSON.stringify(payload),
        signal: AbortSignal.timeout(Number(process.env.CERBER_MESH_TIMEOUT_MS ?? 10_000)),
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      if (state.peerStatus[peer.id]) {
        state.peerStatus[peer.id].reachable = true;
        state.peerStatus[peer.id].last_error = null;
      }
    } catch (e) {
      if (state.peerStatus[peer.id]) {
        state.peerStatus[peer.id].reachable = false;
        state.peerStatus[peer.id].last_error = String(e.message || e);
      }
    }
  }
}

export async function enrollPeerKeys(state) {
  for (const peer of state.peersConfig.peers || []) {
    if (peer.id === state.identity.id) continue;
    if (!peer.url) continue;
    try {
      const res = await fetch(peer.url.replace(/\/$/, "") + "/v1/cerber/peer-info", {
        signal: AbortSignal.timeout(Number(process.env.CERBER_ENROLL_TIMEOUT_MS ?? 3_000)),
        headers: { "user-agent": "cerber-mesh/enroll" },
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const info = await res.json();
      if (info.public_key) {
        const hex = info.public_key.replace(/^0x/, "");
        state.trusted[info.peer_id] = hex;
        state.trusted[info.kid] = hex;
        state.trusted[hex] = true;
        peer.public_key = hex;
        peer.kid = info.kid;
        if (state.peerStatus[peer.id]) {
          state.peerStatus[peer.id].reachable = true;
          state.peerStatus[peer.id].last_error = null;
        }
      }
    } catch (e) {
      if (state.peerStatus[peer.id]) {
        state.peerStatus[peer.id].reachable = false;
        state.peerStatus[peer.id].last_error = String(e.message || e);
      }
    }
  }
  if (existsSync(process.env.CERBER_PEERS_FILE || "") || true) {
    persistPeerKeys(state.identity, state.peersConfig);
  }
}
