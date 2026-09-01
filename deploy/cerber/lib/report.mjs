import { readFileSync, existsSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { readAuditRange, merkleRootOfBodies } from "./audit-store.mjs";
import { wrapSigned } from "./sign.mjs";

/**
 * Detailed CERBER report: every audited interaction in the window, mesh peer
 * status, coverage %, merkle root of verified bodies for cross-peer reconcile.
 */
export function buildDetailedReport(identity, { meshState, sinceIso, untilIso } = {}) {
  const rows = readAuditRange({ sinceIso, untilIso });
  const verified = rows.filter((r) => r.outcome === "verified");
  const rejected = rows.filter((r) => r.outcome === "rejected");
  const byKind = {};
  for (const r of rows) {
    const k = r.kind || "unknown";
    byKind[k] = byKind[k] || { total: 0, verified: 0, rejected: 0 };
    byKind[k].total += 1;
    if (r.outcome === "verified") byKind[k].verified += 1;
    else byKind[k].rejected += 1;
  }

  const latestPath = process.env.CERBER_LATEST_REPORT ?? "/var/lib/datachain-rope/cerber/latest-report.json";
  let latestVerify = null;
  if (existsSync(latestPath)) {
    try {
      latestVerify = JSON.parse(readFileSync(latestPath, "utf8"));
    } catch {
      latestVerify = null;
    }
  }

  const body = {
    schema: "datachain.cerber.detailed-report/v1",
    peer_id: identity.id,
    kid: identity.kid,
    generated_at: Math.floor(Date.now() / 1000),
    generated_at_iso: new Date().toISOString(),
    window: {
      since: sinceIso || null,
      until: untilIso || null,
      interactions: rows.length,
    },
    coverage: {
      verified: verified.length,
      rejected: rejected.length,
      total: rows.length,
      pct: rows.length ? Math.round((verified.length / rows.length) * 10000) / 100 : 0,
      target_pct: 100,
      meets_target: rejected.length === 0 && verified.length > 0,
    },
    by_kind: byKind,
    merkle_root_verified: merkleRootOfBodies(verified),
    latest_rope_verify: latestVerify?.body
      ? {
          coverage_pct: latestVerify.body.coverage_pct,
          all_verified: latestVerify.body.all_verified,
          total: latestVerify.body.total,
          verified: latestVerify.body.verified,
          rejected: latestVerify.body.rejected,
          envelope_kid: latestVerify.envelope?.kid,
        }
      : null,
    mesh: meshState
      ? {
          peers: meshState.peerStatus,
          ingest_count: meshState.ingestCount,
          self: identity.id,
        }
      : null,
    // Cap inline detail; full trail remains in audit NDJSON.
    sample_rejected: rejected.slice(-25),
    sample_verified: verified.slice(-25).map((r) => ({
      ts: r.ts,
      kind: r.kind,
      method: r.method,
      peer_id: r.peer_id,
      body_sha256: r.body_sha256,
    })),
  };

  return wrapSigned(identity, "detailed_report", body);
}

export function writeReportFile(signedReport, path) {
  const out =
    path ||
    process.env.CERBER_DETAILED_REPORT_PATH ||
    "/var/lib/datachain-rope/cerber/detailed-report.json";
  mkdirSync(dirname(out), { recursive: true, mode: 0o750 });
  writeFileSync(out, JSON.stringify(signedReport, null, 2), { mode: 0o640 });
  // Also publish under nginx html if configured
  const pub =
    process.env.CERBER_DETAILED_REPORT_PUBLIC ||
    "/opt/datachain-rope/code/deploy/nginx/html/fleet/cerber-detailed-report.json";
  try {
    mkdirSync(dirname(pub), { recursive: true });
    writeFileSync(pub, JSON.stringify(signedReport, null, 2));
  } catch {
    /* optional public path */
  }
  return out;
}
