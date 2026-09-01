#!/usr/bin/env node
import { ensureIdentity } from "../lib/identity.mjs";
import { createMeshState, loadPeersConfig } from "../lib/mesh.mjs";
import { buildDetailedReport, writeReportFile } from "../lib/report.mjs";

const keyPath =
  process.env.CERBER_IDENTITY_KEY || "/var/lib/datachain-rope/cerber/identity.pem";
const peerId = process.env.CERBER_PEER_ID || "cerber-rope";
const identity = ensureIdentity(keyPath, { peerId });
const peersFile =
  process.env.CERBER_PEERS_FILE ||
  new URL("../config/peers.production.json", import.meta.url).pathname;
const state = createMeshState(identity, loadPeersConfig(peersFile));
const since = process.argv.includes("--since")
  ? process.argv[process.argv.indexOf("--since") + 1]
  : undefined;
const report = buildDetailedReport(identity, { meshState: state, sinceIso: since });
const path = writeReportFile(report);
process.stdout.write(JSON.stringify({ path, coverage: report.body.coverage, mesh: report.body.mesh }, null, 2) + "\n");
