import { appendFileSync, mkdirSync, readFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { createHash } from "node:crypto";

/**
 * Append-only NDJSON audit of every verified (or rejected) Rope/mesh interaction.
 * Daily files under CERBER_AUDIT_DIR for retention and report generation.
 */

export function auditDir() {
  return process.env.CERBER_AUDIT_DIR ?? "/var/lib/datachain-rope/cerber/audit";
}

function dayFile(ts = new Date()) {
  const d = ts.toISOString().slice(0, 10);
  return join(auditDir(), `interactions-${d}.ndjson`);
}

export function appendAudit(record) {
  const path = dayFile();
  mkdirSync(dirname(path), { recursive: true, mode: 0o750 });
  const line =
    JSON.stringify({
      ts: new Date().toISOString(),
      ...record,
    }) + "\n";
  appendFileSync(path, line, { mode: 0o640 });
  return path;
}

export function recordVerified(interaction) {
  return appendAudit({
    outcome: "verified",
    ...interaction,
  });
}

export function recordRejected(interaction) {
  return appendAudit({
    outcome: "rejected",
    ...interaction,
  });
}

export function readAuditRange({ sinceIso, untilIso, limit = 50_000 } = {}) {
  const dir = auditDir();
  if (!existsSync(dir)) return [];
  const files = readdirSync(dir)
    .filter((f) => f.startsWith("interactions-") && f.endsWith(".ndjson"))
    .sort();
  const out = [];
  const since = sinceIso ? Date.parse(sinceIso) : 0;
  const until = untilIso ? Date.parse(untilIso) : Date.now() + 60_000;
  for (const f of files) {
    const full = join(dir, f);
    if (!statSync(full).isFile()) continue;
    const text = readFileSync(full, "utf8");
    for (const line of text.split("\n")) {
      if (!line.trim()) continue;
      let row;
      try {
        row = JSON.parse(line);
      } catch {
        continue;
      }
      const t = Date.parse(row.ts || 0);
      if (t < since || t > until) continue;
      out.push(row);
      if (out.length >= limit) return out;
    }
  }
  return out;
}

export function merkleRootOfBodies(rows) {
  if (rows.length === 0) return null;
  let layer = rows.map((r) =>
    createHash("sha256")
      .update(JSON.stringify({ id: r.id, outcome: r.outcome, kind: r.kind, body_sha256: r.body_sha256 }))
      .digest()
  );
  while (layer.length > 1) {
    const next = [];
    for (let i = 0; i < layer.length; i += 2) {
      const a = layer[i];
      const b = layer[i + 1] ?? a;
      next.push(createHash("sha256").update(Buffer.concat([a, b])).digest());
    }
    layer = next;
  }
  return layer[0].toString("hex");
}
