#!/usr/bin/env node
/**
 * CERBER mesh daemon (Rope / DCSwap / Tanastok / Alteros).
 *
 *   node bin/cerber-mesh.mjs serve          # HTTP mesh + periodic verify + heartbeat
 *   node bin/cerber-mesh.mjs verify-rope    # one-shot Rope verification
 *   node bin/cerber-mesh.mjs enroll         # fetch peer public keys
 */
import { ensureIdentity } from "../lib/identity.mjs";
import { verifyRopeInteractions } from "../lib/verify-rope.mjs";
import {
  loadPeersConfig,
  createMeshState,
  startMeshServer,
  meshHeartbeat,
  meshPublishReport,
  enrollPeerKeys,
  persistPeerKeys,
} from "../lib/mesh.mjs";
import { buildDetailedReport, writeReportFile } from "../lib/report.mjs";
import { alertEmailConfigured, pageEmail } from "../lib/page-email.mjs";
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const cmd = process.argv[2] || "serve";
const keyPath =
  process.env.CERBER_IDENTITY_KEY || "/var/lib/datachain-rope/cerber/identity.pem";
const peerId = process.env.CERBER_PEER_ID || "cerber-rope";
const peersFile =
  process.env.CERBER_PEERS_FILE || join(__dirname, "../config/peers.production.json");

// Allow Alteros URL override without editing committed peers file
function loadPeers() {
  const cfg = loadPeersConfig(peersFile);
  const alterosUrl = process.env.CERBER_PEER_ALTEROS_URL;
  const dcswapUrl = process.env.CERBER_PEER_DCSWAP_URL;
  const tanastokUrl = process.env.CERBER_PEER_TANASTOK_URL;
  for (const p of cfg.peers) {
    if (p.id === "cerber-alteros" && alterosUrl) p.url = alterosUrl;
    if (p.id === "cerber-dcswap" && dcswapUrl) p.url = dcswapUrl;
    if (p.id === "cerber-tanastok" && tanastokUrl) p.url = tanastokUrl;
    if (p.id === peerId && process.env.CERBER_PUBLIC_MESH_URL) {
      p.url = process.env.CERBER_PUBLIC_MESH_URL;
    }
  }
  // Runtime overlay with enrolled keys
  const runtime = process.env.CERBER_PEERS_RUNTIME || "/var/lib/datachain-rope/cerber/peers.runtime.json";
  if (existsSync(runtime)) {
    try {
      const rt = JSON.parse(readFileSync(runtime, "utf8"));
      const byId = Object.fromEntries((rt.peers || []).map((p) => [p.id, p]));
      for (const p of cfg.peers) {
        if (byId[p.id]?.public_key) {
          p.public_key = byId[p.id].public_key;
          p.kid = byId[p.id].kid;
        }
      }
    } catch {
      /* keep static */
    }
  }
  return cfg;
}

const identity = ensureIdentity(keyPath, { peerId });
const peersConfig = loadPeers();
persistPeerKeys(identity, peersConfig);

if (cmd === "verify-rope") {
  const signed = await verifyRopeInteractions(identity, peersConfig);
  process.stdout.write(JSON.stringify(signed, null, 2) + "\n");
  process.exit(signed.body.all_verified ? 0 : 2);
}

if (cmd === "enroll") {
  const state = createMeshState(identity, peersConfig);
  await enrollPeerKeys(state);
  process.stdout.write(JSON.stringify({ ok: true, peers: state.peerStatus }, null, 2) + "\n");
  process.exit(0);
}

if (cmd !== "serve") {
  process.stderr.write("usage: cerber-mesh.mjs <serve|verify-rope|enroll>\n");
  process.exit(64);
}

const state = createMeshState(identity, peersConfig);
const { port, host } = startMeshServer(state);
process.stdout.write(
  `[cerber-mesh] listening ${host}:${port} peer=${identity.id} kid=${identity.kid}\n`
);

const VERIFY_MS = Number(process.env.CERBER_VERIFY_INTERVAL_MS ?? 60_000);
const HEARTBEAT_MS = Number(process.env.CERBER_HEARTBEAT_INTERVAL_MS ?? 45_000);
const REPORT_MS = Number(process.env.CERBER_REPORT_INTERVAL_MS ?? 120_000);
// Re-enroll so peers that boot after us are not stuck reachable=false forever.
const ENROLL_MS = Number(process.env.CERBER_ENROLL_INTERVAL_MS ?? 300_000);

async function pageOnVerifyBreach(signed) {
  const body = signed?.body || {};
  const coverage = Number(body.coverage_pct ?? 0);
  const allOk = body.all_verified === true && coverage >= 100;
  if (allOk) return;

  if (!alertEmailConfigured()) {
    process.stderr.write(
      `[cerber-mesh] page skipped: SMTP not configured (set EMAIL_* in /etc/cerber-alert.env); recipient would be ${process.env.CERBER_ALERT_EMAIL || "unset"}\n`
    );
    return;
  }

  const rejected = body.rejected ?? body.rejected_count ?? 0;
  const reasons = Array.isArray(body.failures)
    ? body.failures.map((f) => f.reason || f.code || JSON.stringify(f)).slice(0, 8)
    : [];
  const result = await pageEmail({
    rule: `${peerId}-rope-verify`,
    threatLevel: coverage === 0 ? 5 : 4,
    subject: `coverage=${coverage}% all_verified=${body.all_verified}`,
    dedupeKey: `${peerId}:rope-verify:${coverage < 100 ? "low-coverage" : "not-all-verified"}`,
    body: [
      `Peer          : ${peerId}`,
      `Coverage      : ${coverage}%`,
      `All verified  : ${body.all_verified}`,
      `Total audited : ${body.total ?? "?"}`,
      `Rejected      : ${rejected}`,
      reasons.length ? `Failures:\n  - ${reasons.join("\n  - ")}` : "(no failure detail in report body)",
      ``,
      `Mesh status   : http://127.0.0.1:${port}/v1/cerber/mesh-status`,
      `Public report : https://erpc.datachain.network/v1/cerber/report`,
    ].join("\n"),
  });
  process.stdout.write(
    `[cerber-mesh] page ${result.sent ? "SENT" : "not-sent"} to=${result.to || "?"} ${result.reason || ""}\n`
  );
}

async function tickVerify() {
  try {
    const signed = await verifyRopeInteractions(identity, state.peersConfig);
    await meshPublishReport(state, signed);
    process.stdout.write(
      `[cerber-mesh] rope-verify coverage=${signed.body.coverage_pct}% all_verified=${signed.body.all_verified} total=${signed.body.total}\n`
    );
    await pageOnVerifyBreach(signed);
  } catch (e) {
    process.stderr.write(`[cerber-mesh] verify failed: ${e.message || e}\n`);
    if (alertEmailConfigured()) {
      const result = await pageEmail({
        rule: `${peerId}-verify-exception`,
        threatLevel: 5,
        subject: `verify threw: ${String(e.message || e).slice(0, 80)}`,
        dedupeKey: `${peerId}:verify-exception`,
        body: String(e?.stack || e),
      });
      process.stdout.write(
        `[cerber-mesh] page ${result.sent ? "SENT" : "not-sent"} to=${result.to || "?"} ${result.reason || ""}\n`
      );
    }
  }
}

async function tickHeartbeat() {
  try {
    await meshHeartbeat(state);
  } catch (e) {
    process.stderr.write(`[cerber-mesh] heartbeat failed: ${e.message || e}\n`);
  }
}

async function tickReport() {
  try {
    const report = buildDetailedReport(identity, { meshState: state });
    writeReportFile(report);
    process.stdout.write(
      `[cerber-mesh] detailed-report interactions=${report.body.window.interactions} coverage=${report.body.coverage.pct}%\n`
    );
  } catch (e) {
    process.stderr.write(`[cerber-mesh] report failed: ${e.message || e}\n`);
  }
}

// Do not block the listener on slow/unreachable mesh peers (DCSwap/Tanastok
// may not listen yet). Verify Rope first, then enroll + heartbeat in background.
await tickVerify();
await tickReport();
setImmediate(() => {
  enrollPeerKeys(state)
    .then(() => tickHeartbeat())
    .catch((e) => process.stderr.write(`[cerber-mesh] enroll/heartbeat: ${e.message || e}\n`));
});

setInterval(tickVerify, VERIFY_MS);
setInterval(tickHeartbeat, HEARTBEAT_MS);
setInterval(tickReport, REPORT_MS);
setInterval(() => {
  enrollPeerKeys(state).catch((e) =>
    process.stderr.write(`[cerber-mesh] periodic enroll: ${e.message || e}\n`)
  );
}, ENROLL_MS);

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    process.stdout.write(`[cerber-mesh] ${sig} shutting down\n`);
    process.exit(0);
  });
}
