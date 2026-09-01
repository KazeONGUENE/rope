#!/usr/bin/env node
/**
 * Sign a JSON document (stdin or --file) for CERBER mesh / fleet-status.
 *
 *   cerber-sign.mjs --kind fleet_status --file fleet-status.json --out fleet-status.sig.json
 */
import { readFileSync, writeFileSync } from "node:fs";
import { ensureIdentity } from "../lib/identity.mjs";
import { signEnvelope } from "../lib/sign.mjs";

const args = process.argv.slice(2);
function opt(name, def) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : def;
}

const kind = opt("--kind", "fleet_status");
const file = opt("--file");
const out = opt("--out");
const keyPath =
  opt("--key") ||
  process.env.CERBER_IDENTITY_KEY ||
  "/var/lib/datachain-rope/cerber/identity.pem";
const peerId = opt("--peer-id") || process.env.CERBER_PEER_ID || "cerber-rope";

if (!file || !out) {
  process.stderr.write("usage: cerber-sign.mjs --kind <k> --file <body.json> --out <sig.json>\n");
  process.exit(64);
}

const identity = ensureIdentity(keyPath, { peerId });
const body = JSON.parse(readFileSync(file, "utf8"));
// Never sign a nested prior envelope
delete body.cerber_envelope;
const envelope = signEnvelope(identity, { kind, body });
writeFileSync(out, JSON.stringify(envelope, null, 2), { mode: 0o644 });
process.stdout.write(JSON.stringify({ ok: true, kid: identity.kid, peer_id: identity.id, out }) + "\n");
