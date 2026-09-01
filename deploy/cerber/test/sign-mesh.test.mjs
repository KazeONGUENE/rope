import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ensureIdentity } from "../lib/identity.mjs";
import { signEnvelope, verifyEnvelope, wrapSigned } from "../lib/sign.mjs";
import { canonicalize, buildSignInput } from "../lib/canonical.mjs";
import { appendAudit, readAuditRange, merkleRootOfBodies } from "../lib/audit-store.mjs";
import { createMeshState, startMeshServer } from "../lib/mesh.mjs";

test("canonical sort is stable", () => {
  assert.equal(canonicalize({ b: 1, a: 2 }), canonicalize({ a: 2, b: 1 }));
});

test("sign + verify round trip", () => {
  const dir = mkdtempSync(join(tmpdir(), "cerber-id-"));
  const id = ensureIdentity(join(dir, "identity.pem"), { peerId: "cerber-test" });
  const body = { hello: "world", n: 1 };
  const env = signEnvelope(id, { kind: "unit_test", body });
  const v = verifyEnvelope(env, body, {
    trustedKeys: { [id.id]: id.publicKeyHex, [id.publicKeyHex]: true },
  });
  assert.equal(v.ok, true);
  const bad = verifyEnvelope(env, { hello: "nope" }, { trustedKeys: { [id.publicKeyHex]: true } });
  assert.equal(bad.ok, false);
  rmSync(dir, { recursive: true, force: true });
});

test("buildSignInput length-prefixes kind and body", () => {
  const nonce = Buffer.alloc(16, 7);
  const buf = buildSignInput({ kind: "k", body: { a: 1 }, signedAt: 100, nonce });
  assert.ok(buf.length > 40);
});

test("audit store + merkle", () => {
  const dir = mkdtempSync(join(tmpdir(), "cerber-audit-"));
  process.env.CERBER_AUDIT_DIR = dir;
  appendAudit({ id: "1", kind: "t", outcome: "verified", body_sha256: "aa" });
  appendAudit({ id: "2", kind: "t", outcome: "verified", body_sha256: "bb" });
  const rows = readAuditRange({});
  assert.equal(rows.length, 2);
  assert.ok(merkleRootOfBodies(rows.filter((r) => r.outcome === "verified")));
  rmSync(dir, { recursive: true, force: true });
});

test("mesh heartbeat endpoint accepts signed peer", async () => {
  const dir = mkdtempSync(join(tmpdir(), "cerber-mesh-"));
  const idA = ensureIdentity(join(dir, "a.pem"), { peerId: "cerber-a" });
  const idB = ensureIdentity(join(dir, "b.pem"), { peerId: "cerber-b" });
  const peers = {
    schema: "datachain.cerber.peers/v1",
    peers: [
      { id: "cerber-a", role: "rope", url: "http://127.0.0.1:0", public_key: idA.publicKeyHex, kid: idA.kid },
      { id: "cerber-b", role: "dcswap", url: "http://127.0.0.1:0", public_key: idB.publicKeyHex, kid: idB.kid },
    ],
  };
  const state = createMeshState(idA, peers);
  const { server, port } = startMeshServer(state, { port: 0, host: "127.0.0.1" });
  await new Promise((r) => server.once("listening", r));
  const addr = server.address();
  const listenPort = typeof addr === "object" ? addr.port : port;
  const signed = wrapSigned(idB, "mesh_heartbeat", { ts: Math.floor(Date.now() / 1000), peer_id: idB.id });
  const res = await fetch(`http://127.0.0.1:${listenPort}/v1/cerber/heartbeat`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(signed),
  });
  assert.equal(res.status, 200);
  const info = await fetch(`http://127.0.0.1:${listenPort}/v1/cerber/peer-info`).then((r) => r.json());
  assert.equal(info.peer_id, "cerber-a");
  server.close();
  rmSync(dir, { recursive: true, force: true });
});
