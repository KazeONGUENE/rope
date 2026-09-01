#!/usr/bin/env node
/**
 * CERBER edge-probe ingest daemon.
 *
 * Implements the frozen v1 spec at
 * `datachain-rope/docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md`.
 *
 * Listens on 127.0.0.1:9109 (loopback-only by default) for signed
 * `POST /v1/mesh/edge-probe` from trusted CERBER peers, verifies the envelope
 * using the shared `lib/sign.mjs` primitives + `config/peers.production.json`,
 * and appends the accepted body to
 * `/var/lib/datachain-rope/fleet/external-probes.ndjson` for the
 * `erpc-fleet-ha.sh::read_external_probes()` aggregator to consume.
 *
 *   node bin/cerber-edge-ingest.mjs
 *
 * Env:
 *   CERBER_PEERS_FILE            - path to peers.production.json
 *                                  (default: ../config/peers.production.json)
 *   CERBER_EDGE_INGEST_PORT      - listen port (default 9109)
 *   CERBER_EDGE_INGEST_HOST      - listen host (default 127.0.0.1)
 *   CERBER_EDGE_PROBES_FILE      - NDJSON path
 *                                  (default: /var/lib/datachain-rope/fleet/external-probes.ndjson)
 *   CERBER_PEERS_RUNTIME         - optional runtime overlay with enrolled keys
 *                                  (default: /var/lib/datachain-rope/cerber/peers.runtime.json)
 */

import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildTrustedKeys,
  createRateLimitState,
  startEdgeIngestServer,
  ndjsonPath,
} from "../lib/edge-ingest.mjs";
import { loadPeersConfig } from "../lib/mesh.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const peersFile =
  process.env.CERBER_PEERS_FILE ||
  join(__dirname, "../config/peers.production.json");

function loadPeersWithRuntimeOverlay() {
  const cfg = loadPeersConfig(peersFile);
  const runtime =
    process.env.CERBER_PEERS_RUNTIME ||
    "/var/lib/datachain-rope/cerber/peers.runtime.json";
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
      /* ignore malformed overlay */
    }
  }
  return cfg;
}

const peersConfig = loadPeersWithRuntimeOverlay();
const trustedKeys = buildTrustedKeys(peersConfig);
const rateLimit = createRateLimitState();

const state = {
  peersConfig,
  trustedKeys,
  rateLimit,
};

const { port, host } = startEdgeIngestServer(state);
const peerCount = (peersConfig.peers || []).filter((p) => p.public_key).length;
process.stdout.write(
  `[cerber-edge-ingest] listening ${host}:${port} trusted_peers=${peerCount} ndjson=${ndjsonPath()}\n`
);

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    process.stdout.write(`[cerber-edge-ingest] ${sig} - shutting down\n`);
    process.exit(0);
  });
}
