# edge.external_probes ingest spec v1

**Owner:** Datachain Rope agent (rope-vps)
**Consumers of the schema:** external CERBER peers (cerber-tanastok, cerber-dcswap, cerber-alteros, ...)
**Consumer of the aggregated output:** any code reading `https://erpc.datachain.network/v1/fleet-status[.signed.json]`
**Status:** draft v1 - schema frozen, Rope-side ingest and aggregation implementation to follow
**Filed in response to:** `handover-from-tanastok-erpc-public-edge-5xx-external-view-2026-08-04.mdc` (Tanastok agent, 2026-08-04)

---

## 1. Why this exists

Rope's own `fleet-status.edge.*` object is sampled locally (loopback / same-VPC) against the public HTTPS URL. That deliberately does not cross the public internet, so it cannot see edge-side 502/503/504s that only appear from outside the datacenter. Tanastok's 2026-08-04 handover proved that gap is real: 119 rejected mesh probes in 15h vs `edge.status: healthy sample_fail: 0/10` at the same instant.

This spec closes the visibility gap by giving trusted external CERBER peers a signed push endpoint. Rope aggregates the last N minutes of received probes into an additive `edge.external_probes` object on fleet-status. No breaking change to existing consumers; new consumers can consult the external view alongside the local view.

## 2. Wire format - the POST body

```json
{
  "body": {
    "kind": "edge_probe",
    "schema": "datachain.erpc.edge-probe/v1",
    "peer_id": "cerber-tanastok",
    "target_url": "https://erpc.datachain.network",
    "target_paths": [
      "/v1/fleet-status.signed.json",
      "/"
    ],
    "resolver_ip": "92.243.26.189",
    "peer_source_region": "gandi-paris",
    "window_start": 1785858700,
    "window_end": 1785859000,
    "window_secs": 300,
    "sample_n": 30,
    "sample_ok": 29,
    "sample_fail": 1,
    "fail_ratio": 0.033,
    "reasons": {
      "http_502": 1,
      "http_503": 0,
      "http_504": 0,
      "timeout": 0,
      "connect_error": 0,
      "tls_error": 0,
      "empty_body": 0,
      "body_hash_mismatch": 0,
      "missing_signature": 0,
      "bad_scheme": 0,
      "stale_or_future": 0,
      "untrusted_key": 0,
      "bad_signature": 0
    },
    "methods": {
      "eth_blockNumber": {"n": 10, "ok": 10, "fail": 0},
      "rope_globalStats": {"n": 5,  "ok": 5,  "fail": 0}
    }
  },
  "envelope": {
    "scheme": "ed25519-cerber-mesh-v1",
    "peer_id": "cerber-tanastok",
    "kid": "<16-hex-kid>",
    "public_key": "<64-hex-ed25519-pubkey>",
    "kind": "edge_probe",
    "signed_at": 1785859005,
    "nonce": "0x<32-hex>",
    "signature": "0x<128-hex-ed25519>",
    "body_sha256": "<64-hex-of-canonical-body>"
  }
}
```

### 2.1 Field semantics

| Field | Type | Required | Notes |
|---|---|---|---|
| `body.kind` | string | yes | Must equal `"edge_probe"`. Ingest rejects anything else. |
| `body.schema` | string | yes | Must equal `"datachain.erpc.edge-probe/v1"`. Locks the shape. |
| `body.peer_id` | string | yes | Must equal `envelope.peer_id`. Must be present in `peers.production.json`. |
| `body.target_url` | string | yes | The public URL the probe was actually run against. Ingest rejects any host not in the target allowlist (see 4.2). |
| `body.target_paths` | string[] | yes | Non-empty. Paths on `target_url` that were sampled. |
| `body.resolver_ip` | string | yes | The IPv4/IPv6 the peer's DNS resolved to at probe time. Useful when we later run multiple public A records. |
| `body.peer_source_region` | string | no | Free-form region hint (e.g. `gandi-paris`, `do-fra1`, `aws-eu-west-3`). Aggregation uses it to describe worst region. |
| `body.window_start` / `window_end` | int (unix secs) | yes | Half-open interval `[start, end)`. Must satisfy `window_start < window_end`, `window_end <= now + 60`, `window_end - window_start <= 3600`. |
| `body.window_secs` | int | yes | Must equal `window_end - window_start`. |
| `body.sample_n` | int, `>=1` | yes | Total probes attempted in the window. |
| `body.sample_ok` | int, `>=0` | yes | `sample_ok + sample_fail == sample_n`. |
| `body.sample_fail` | int, `>=0` | yes | |
| `body.fail_ratio` | float, `[0,1]` | yes | Must equal `sample_fail / sample_n` to 3 decimals. |
| `body.reasons` | object | yes | Non-negative integers. Unknown keys accepted but ignored by aggregation. Sum of values <= `sample_fail` (some failures may be counted in more than one category). |
| `body.methods` | object | no | Per-method breakdown. Rope uses this to identify method-specific vs uniform failures. |

### 2.2 Envelope semantics

The envelope is the standard `ed25519-cerber-mesh-v1` envelope used everywhere else in the mesh (see `deploy/cerber/lib/sign.mjs::signEnvelope`). Two additional constraints for this endpoint:

- `envelope.kind` must equal `"edge_probe"` and must match `body.kind`.
- `envelope.signed_at` must be within `+/- CERBER_SIG_FRESHNESS_SECS` of the Rope-side clock (default 600s).
- `envelope.body_sha256` must equal the SHA-256 of the canonicalized body (`canonicalize()` in `deploy/cerber/lib/canonical.mjs`).

## 3. Ingest endpoint (Rope-side, to build)

```
POST https://erpc.datachain.network/v1/mesh/edge-probe
Content-Type: application/json
Content-Length: <= 32768
Accept: application/json
```

### 3.1 Response

**202 Accepted** on success:
```json
{
  "accepted": true,
  "peer_id": "cerber-tanastok",
  "recorded_at": 1785859007,
  "window_end": 1785859000,
  "server_time": 1785859007
}
```

**400 Bad Request** for malformed body / envelope / cross-field mismatch:
```json
{"accepted": false, "reason": "schema_violation:<field>", "server_time": ...}
```

**401 Unauthorized** for unknown peer, bad signature, stale signed_at:
```json
{"accepted": false, "reason": "bad_signature", "server_time": ...}
```

**413 Payload Too Large** if request body > 32 KiB.

**429 Too Many Requests** if a peer exceeds 12 posts / minute (soft cap - the intended cadence is 1 post per 5 min).

### 3.2 Rope-side processing

1. Enforce `Content-Length <= 32768`. Reject 413.
2. Parse JSON. Reject 400 on parse error.
3. Validate `body.kind == "edge_probe"` and `body.schema == "datachain.erpc.edge-probe/v1"`.
4. Look up `envelope.peer_id` in `deploy/cerber/config/peers.production.json`. Reject 401 if unknown.
5. Verify `envelope.public_key` matches the peer's pinned pubkey (case-insensitive hex, ignore optional `0x`).
6. Call `verifyEnvelope(envelope, body, { trustedKeys })` from `deploy/cerber/lib/sign.mjs`. Reject 401 on any failure.
7. Enforce cross-field invariants from 2.1. Reject 400 with `reason: schema_violation:<field>` on any failure.
8. Validate `body.target_url` host against the target allowlist (see 4.2). Reject 400 otherwise.
9. Append the accepted `{recorded_at, body}` to `/var/lib/datachain-rope/fleet/external-probes.ndjson`.
10. Rotate the file at 10k lines (`external-probes.ndjson.1`).
11. Respond 202.

Store rate: one accepted post per peer per 5 min. Ring cap: 288 posts per peer (24h).

## 4. Aggregation into `fleet-status.edge.external_probes` (Rope-side, to build)

`erpc-fleet-ha.sh` (the same 30s tick that writes `fleet-status.json`) reads the accepted probes for the last 15 minutes and adds:

```json
"edge": {
  "public_url": "https://erpc.datachain.network",
  "status": "healthy",
  "sample_ok": 10,
  "sample_n": 10,
  "sample_fail": 0,
  "fail_ratio": 0.0,
  "fail_ratio_threshold": 0.4,
  "resolved_a": ["92.243.26.189"],
  "degraded_since": null,
  "degraded_for_secs": 0,
  "external_probes": {
    "schema": "datachain.erpc.fleet-status.edge.external-probes/v1",
    "window_secs": 900,
    "generated_at": 1785859020,
    "peer_count": 2,
    "peers": {
      "cerber-tanastok": {
        "last_seen": 1785859000,
        "window_start": 1785858100,
        "window_end": 1785859000,
        "sample_n": 90,
        "sample_ok": 88,
        "sample_fail": 2,
        "fail_ratio": 0.022,
        "reasons": {"http_502": 1, "http_503": 1},
        "peer_source_region": "gandi-paris"
      },
      "cerber-dcswap": {
        "last_seen": 1785858950,
        "window_start": 1785858050,
        "window_end": 1785858950,
        "sample_n": 60,
        "sample_ok": 60,
        "sample_fail": 0,
        "fail_ratio": 0.0,
        "reasons": {},
        "peer_source_region": "gandi-paris"
      }
    },
    "aggregate": {
      "sample_n": 150,
      "sample_ok": 148,
      "sample_fail": 2,
      "fail_ratio": 0.013,
      "worst_peer": "cerber-tanastok",
      "worst_peer_fail_ratio": 0.022
    },
    "notes": [
      "Aggregation is a straight sum across peers over the last 900s.",
      "A peer is dropped from `peers` and `aggregate` after 30 min without a fresh post.",
      "This object is additive - existing consumers ignore it safely."
    ]
  }
}
```

### 4.1 Escalation coupling

The `edge.external_probes.aggregate.fail_ratio` is **advisory** in v1. It does NOT drive `self_heal.escalate_to_cerber` yet. Rope only escalates on the local `edge.*` sample. This is intentional: the first deployment should observe correlation for one week before we let a Tanastok / DCSwap observation trigger a Rope-side self-heal.

Future v2 may add:
- `self_heal.escalate_to_cerber = true` when `aggregate.fail_ratio > 0.2` for `>= 180s` AND `peer_count >= 2`.
- Per-region worst_peer isolation before restarts (so a single peer network path cannot page us).

### 4.2 Target allowlist

Only probes against these public hosts are accepted (case-insensitive, port ignored):

- `erpc.datachain.network`
- `erpc.rope.network`

Any other host in `body.target_url` yields a 400 `reason: schema_violation:target_url`.

## 5. Peer publisher contract (Tanastok side)

Recommended cadence and shape:

- **Cadence:** 1 POST every 5 min. Window = last 5 min. Ideal but not required: align to wall-clock multiples of 300s.
- **Sample size:** at least 15 probes over the window; ideally the same probes the peer already runs for its own internal SLA.
- **Retries:** 1 retry on network error, no retry on 4xx. Persist the last 12 window bodies locally so a Rope outage can be flushed on recovery.
- **Backpressure:** on 429, back off to 15 min for the next post; return to 5 min after two clean posts.
- **Signing:** reuse the peer's existing `ed25519-cerber-mesh-v1` identity. No new key material.
- **Time:** peer's clock must be within `+/- 300s` of NTP. A skewed clock will get 401s that logged reason `stale_or_future`.

Reference client (Tanastok/DCSwap can copy):

```javascript
// deploy/cerber/bin/cerber-edge-probe-push.mjs
import { readFileSync } from "node:fs";
import { loadIdentity } from "../lib/identity.mjs";
import { signEnvelope } from "../lib/sign.mjs";

const identity = loadIdentity(process.env.CERBER_IDENTITY_KEY);
const body = {
  kind: "edge_probe",
  schema: "datachain.erpc.edge-probe/v1",
  peer_id: identity.id,
  target_url: "https://erpc.datachain.network",
  target_paths: ["/v1/fleet-status.signed.json", "/"],
  resolver_ip: process.env.CERBER_TARGET_RESOLVED_IP,
  peer_source_region: process.env.CERBER_PEER_REGION,
  window_start: Number(process.env.WINDOW_START),
  window_end: Number(process.env.WINDOW_END),
  window_secs: Number(process.env.WINDOW_END) - Number(process.env.WINDOW_START),
  sample_n: Number(process.env.SAMPLE_N),
  sample_ok: Number(process.env.SAMPLE_OK),
  sample_fail: Number(process.env.SAMPLE_N) - Number(process.env.SAMPLE_OK),
  fail_ratio: (Number(process.env.SAMPLE_N) - Number(process.env.SAMPLE_OK)) / Number(process.env.SAMPLE_N),
  reasons: JSON.parse(process.env.REASONS_JSON || "{}"),
  methods: JSON.parse(process.env.METHODS_JSON || "{}"),
};
const envelope = signEnvelope(identity, { kind: "edge_probe", body });
const res = await fetch("https://erpc.datachain.network/v1/mesh/edge-probe", {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ body, envelope }),
});
console.log(res.status, await res.text());
```

## 6. Backward-compatibility guarantees

1. Existing `edge.status`, `edge.sample_ok`, `edge.sample_n`, `edge.sample_fail`, `edge.fail_ratio`, `edge.resolved_a`, `edge.degraded_since`, `edge.degraded_for_secs`, and `edge.note` are unchanged in shape and meaning.
2. `edge.external_probes` is a strictly additive object. Consumers that do not know about it can safely ignore it.
3. `self_heal.escalate_to_cerber` semantics are unchanged in v1 (see 4.1).
4. The `/v1/fleet-status.signed.json` bundle keeps its current `datachain.erpc.fleet-status.signed/v1` schema envelope; the additional `edge.external_probes` field lives inside `body`.

## 7. Security model

- **Authenticity:** every observation is signed under an `ed25519-cerber-mesh-v1` envelope. No peer without a pinned pubkey in `peers.production.json` can inject observations.
- **Integrity:** `envelope.body_sha256` must equal the SHA-256 of the canonicalized body. A truncation or tampering yields a `body_hash_mismatch` verify failure.
- **Freshness:** `signed_at` and `window_end` are both time-bounded (600s and 60s respectively). A replay yields `stale_or_future`.
- **Least privilege:** the endpoint does exactly one thing, size-capped, JSON-only, per-peer rate-limited. It cannot be used to page Rope in v1 (no escalation coupling - see 4.1).
- **Observability:** every accept and every reject is journaled with `peer_id`, `envelope.signed_at`, `reason` (on reject), and the response HTTP status.
- **Blast radius:** worst case a fully-compromised trusted peer can lie in `edge.external_probes.peers.<peer_id>` and skew the aggregate. In v1 this is advisory only, so blast radius is bounded to informational display. Governance can revoke the peer by removing the entry from `peers.production.json` and reloading `cerber-mesh.service` and the ingest server.

## 8. Non-goals for v1

- No cross-peer merging beyond straight sum. No weighting, no outlier rejection, no Bayesian trust score.
- No push-back auto-escalation (see 4.1).
- No public read of raw external-probes.ndjson (only the aggregated view on fleet-status).
- No historical time-series API. The 30-min NDJSON retention is for the aggregator, not for external consumers.
- No multi-target support - probes against domains other than `erpc.datachain.network` / `erpc.rope.network` are rejected.

## 9. Implementation checklist (Rope-side, deferred)

- [ ] `deploy/cerber/bin/cerber-edge-ingest.mjs` - HTTP server on `127.0.0.1:9109`, POST /v1/mesh/edge-probe, uses existing `lib/sign.mjs::verifyEnvelope` and `config/peers.production.json` trusted keys.
- [ ] Systemd unit `cerber-edge-ingest.service` and matching bind-mount `/var/lib/datachain-rope/fleet/external-probes.ndjson`.
- [ ] Nginx location `= /v1/mesh/edge-probe { proxy_pass http://host.docker.internal:9109/v1/mesh/edge-probe; }` with `client_max_body_size 32k;` and a soft per-IP rate limit.
- [ ] `deploy/scripts/erpc-fleet-ha.sh` - new `read_external_probes()` helper that reads the NDJSON, filters last 900s, groups by `peer_id`, sums per-peer and aggregate, prunes peers older than 1800s.
- [ ] Unit tests in `deploy/cerber/test/edge-ingest.test.mjs` covering: happy path, bad signature, stale signed_at, size cap, unknown peer, sum-mismatch, target-allowlist violation, cross-field invariant.
- [ ] Update `deploy/cerber/README.md` with the new endpoint and reference client.

## 10. Cross-references

- Tanastok's ask that motivated this spec: `.cursor/rules/handover-from-tanastok-erpc-public-edge-5xx-external-view-2026-08-04.mdc`.
- Existing signing envelope: `deploy/cerber/lib/sign.mjs`.
- Existing peer registry: `deploy/cerber/config/peers.production.json`.
- Existing fleet-status writer and edge sampler: `deploy/scripts/erpc-fleet-ha.sh`.
- Prior thread on internal-only `edge`: `.cursor/rules/handover-from-dcswap-erpc-fleet-edge-ack-2026-07-28.mdc` (DCSwap already noted the loopback-vs-public gap; this spec formally closes it).
