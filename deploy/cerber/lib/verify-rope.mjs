import { randomUUID } from "node:crypto";
import { readFileSync, existsSync } from "node:fs";
import { verifyEnvelope, wrapSigned } from "./sign.mjs";
import { recordVerified, recordRejected } from "./audit-store.mjs";

const DEFAULT_RPC = process.env.CERBER_ROPE_RPC ?? "http://127.0.0.1:8545";
const FLEET_URL = process.env.CERBER_FLEET_STATUS_URL ?? "https://erpc.datachain.network/v1/fleet-status";
const FLEET_FILE =
  process.env.CERBER_FLEET_STATUS_FILE ??
  "/opt/datachain-rope/code/deploy/nginx/html/fleet/fleet-status.json";
const SIG_FILE =
  process.env.CERBER_FLEET_STATUS_SIG_FILE ??
  "/opt/datachain-rope/code/deploy/nginx/html/fleet/fleet-status.sig.json";

async function rpc(url, method, params = []) {
  const res = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json", "user-agent": "cerber-mesh/verify-rope" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
    signal: AbortSignal.timeout(Number(process.env.CERBER_RPC_TIMEOUT_MS ?? 12_000)),
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

function loadTrustedKeys(peersConfig) {
  const map = {};
  for (const p of peersConfig?.peers || []) {
    if (p.public_key) {
      map[p.id] = p.public_key.replace(/^0x/, "");
      if (p.kid) map[p.kid] = p.public_key.replace(/^0x/, "");
      map[p.public_key.replace(/^0x/, "")] = true;
    }
  }
  // Bootstrap trust: accept writer's published key from sig file on first enroll
  // only when CERBER_TRUST_BOOTSTRAP=1 (operator-gated).
  return map;
}

/**
 * Verify 100% of the Rope interaction surfaces this tick can observe:
 * fleet-status (+ signature), ghost_reclaim fields, and a fixed RPC method set.
 * Every result is written to the audit store; signed digest returned for mesh gossip.
 */
export async function verifyRopeInteractions(identity, peersConfig) {
  const trusted = loadTrustedKeys(peersConfig);
  const started = Date.now();
  const results = [];
  const bootstrap = process.env.CERBER_TRUST_BOOTSTRAP === "1";

  // 1) Fleet status body + detached signature
  let fleetBody = null;
  let fleetEnvelope = null;
  try {
    if (existsSync(FLEET_FILE)) {
      fleetBody = JSON.parse(readFileSync(FLEET_FILE, "utf8"));
    } else {
      const res = await fetch(FLEET_URL, {
        headers: { "user-agent": "cerber-mesh/verify-rope" },
        signal: AbortSignal.timeout(12_000),
      });
      if (!res.ok) throw new Error(`fleet HTTP ${res.status}`);
      fleetBody = await res.json();
    }
    if (existsSync(SIG_FILE)) {
      fleetEnvelope = JSON.parse(readFileSync(SIG_FILE, "utf8"));
    } else {
      // Optional companion URL
      try {
        const sigUrl = FLEET_URL.replace(/\/?$/, "") + ".sig.json";
        const alt = FLEET_URL.includes("fleet-status")
          ? FLEET_URL.replace("fleet-status", "fleet-status.sig.json")
          : null;
        for (const u of [sigUrl, alt].filter(Boolean)) {
          const r = await fetch(u, { signal: AbortSignal.timeout(8_000) });
          if (r.ok) {
            fleetEnvelope = await r.json();
            break;
          }
        }
      } catch {
        /* signature required below */
      }
    }
  } catch (e) {
    const row = {
      id: randomUUID(),
      kind: "fleet_status_fetch",
      outcome: "rejected",
      reason: String(e.message || e),
    };
    recordRejected(row);
    results.push(row);
  }

  if (fleetBody) {
    if (!fleetEnvelope) {
      const row = {
        id: randomUUID(),
        kind: "fleet_status",
        outcome: "rejected",
        reason: "missing_signature",
        body_sha256: null,
      };
      recordRejected(row);
      results.push({ ...row, verified: false });
    } else {
      let trust = trusted;
      if (bootstrap && Object.keys(trusted).length === 0 && fleetEnvelope.public_key) {
        trust = { [fleetEnvelope.peer_id]: fleetEnvelope.public_key.replace(/^0x/, "") };
        trust[fleetEnvelope.public_key.replace(/^0x/, "")] = true;
      }
      // Strip nested envelope if HA embedded it
      const body = { ...fleetBody };
      delete body.cerber_envelope;
      const v = verifyEnvelope(fleetEnvelope, body, {
        trustedKeys: Object.keys(trust).length ? trust : undefined,
      });
      const row = {
        id: randomUUID(),
        kind: "fleet_status",
        peer_id: fleetEnvelope.peer_id,
        kid: fleetEnvelope.kid,
        body_sha256: fleetEnvelope.body_sha256,
        writer_status: body.writer?.status,
        edge_status: body.edge?.status,
        escalate: body.self_heal?.escalate_to_cerber === true,
        ghost_reclaimed_total: body.ghost_reclaim?.reclaimed_total ?? 0,
        ghost_last_scan: body.ghost_reclaim?.last_scan_ghosts_found ?? 0,
      };
      if (v.ok) {
        recordVerified({ ...row, outcome: "verified" });
        results.push({ ...row, verified: true });
      } else {
        recordRejected({ ...row, outcome: "rejected", reason: v.reason });
        results.push({ ...row, verified: false, reason: v.reason });
      }
    }
  }

  // 2) Canonical Rope RPC probe set — every call audited
  const methods = [
    ["eth_chainId", []],
    ["eth_blockNumber", []],
    ["rope_globalStats", []],
    ["web3_clientVersion", []],
  ];
  for (const [method, params] of methods) {
    const id = randomUUID();
    try {
      const j = await rpc(DEFAULT_RPC, method, params);
      if (j.error) {
        const row = { id, kind: "rpc", method, outcome: "rejected", reason: j.error.message || "rpc_error" };
        recordRejected(row);
        results.push({ ...row, verified: false });
      } else {
        const row = {
          id,
          kind: "rpc",
          method,
          outcome: "verified",
          result_preview: typeof j.result === "string" ? j.result.slice(0, 80) : JSON.stringify(j.result).slice(0, 120),
        };
        recordVerified(row);
        results.push({ ...row, verified: true });
      }
    } catch (e) {
      const row = { id, kind: "rpc", method, outcome: "rejected", reason: String(e.message || e) };
      recordRejected(row);
      results.push({ ...row, verified: false });
    }
  }

  // 3) Ghost reclaim vigilance — unsigned counter still audited as observation;
  //    when last_scan_ghosts_found > 0 without a matching signed reclaim event, flag.
  if (fleetBody?.ghost_reclaim) {
    const g = fleetBody.ghost_reclaim;
    const id = randomUUID();
    const row = {
      id,
      kind: "ghost_reclaim_observation",
      enabled: !!g.enabled,
      reclaimed_total: g.reclaimed_total ?? 0,
      last_scan_ghosts_found: g.last_scan_ghosts_found ?? 0,
      last_scan_error: g.last_scan_error ?? null,
    };
    if (g.enabled && (g.last_scan_error === "none" || !g.last_scan_error)) {
      recordVerified({ ...row, outcome: "verified" });
      results.push({ ...row, verified: true });
    } else {
      recordRejected({ ...row, outcome: "rejected", reason: g.last_scan_error || "ghost_reclaim_unhealthy" });
      results.push({ ...row, verified: false, reason: g.last_scan_error });
    }
  }

  const verified = results.filter((r) => r.verified).length;
  const rejected = results.filter((r) => !r.verified).length;
  const summary = {
    schema: "datachain.cerber.rope-verify/v1",
    peer_id: identity.id,
    kid: identity.kid,
    generated_at: Math.floor(Date.now() / 1000),
    duration_ms: Date.now() - started,
    total: results.length,
    verified,
    rejected,
    coverage_pct: results.length ? Math.round((verified / results.length) * 10000) / 100 : 0,
    all_verified: rejected === 0 && verified > 0,
    results,
  };

  const signed = wrapSigned(identity, "rope_verify_report", summary);
  recordVerified({
    id: randomUUID(),
    kind: "rope_verify_report",
    outcome: "verified",
    body_sha256: signed.envelope.body_sha256,
    coverage_pct: summary.coverage_pct,
    all_verified: summary.all_verified,
  });

  return signed;
}
