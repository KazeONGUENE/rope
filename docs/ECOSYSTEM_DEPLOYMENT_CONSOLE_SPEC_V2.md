# Ecosystem Deployment Console (EDC)

**Self-Service Sovereign Infrastructure for Predictive Maintenance and Environmental Monitoring on Datachain Rope**

*Functional & Technical Specification — v2.0 — Enriched and implementation-bound*

**Datachain Foundation**

> v2.0 supersedes the v1.0 draft (`Ecosystem_Deployment_Console_Specification.md`, 2026-07).
> Everything in v1.0 remains normative unless amended below. v2.0 adds: (a) a codebase
> reconciliation — several components v1.0 listed as "TO BUILD" already exist in the
> `datachain-rope` workspace and are now referenced by crate path; (b) the concrete AI
> orchestration design on top of the production `AlterOSOrchestrator`; (c) the on-chain
> AccessGrant object model and its key-minting scheme; (d) the dcscan.io `/ecosystem`
> public projects page and its disintermediated stakeholder links; (e) the deployment
> model that ships the whole dashboard stack to the project's own primary node IP;
> (f) the `rope-edc` crate that implements all of it.

---

## 1. What v2.0 changes — summary for reviewers of v1.0

| v1.0 statement | v2.0 reality |
|---|---|
| "IoT protocol gateway (MQTT/CoAP/LoRaWAN) — TO BUILD" | **EXISTS**: `crates/rope-iot-gateway` (MQTT 3.1.1 :1883, CoAP :5683, device registry, telemetry → `rope_appendToLedger`). EDC consumes it, does not rebuild it. |
| "Sovereign node auto-provisioning — TO BUILD" | **EXISTS**: `crates/rope-deployer` (Exoscale + DigitalOcean + Local adapters behind a `CloudProvider` trait, tenant-DID-scoped instances). EDC drives it through `ProviderRegistry`. |
| "AI orchestration layer (Alteros / Claude / OpenAI router) — FEASIBLE" | **EXISTS**: `crates/rope-agent-runtime/src/ai/` ships `AlterOSOrchestrator` (query-complexity analysis, cost-optimized routing across Ollama/OpenAI/Anthropic, availability health checks). EDC wraps it with grounding + guardrails (§6). |
| "Fleet-level batch query — FEASIBLE" | **BUILT in EDC**: the project registry holds the asset index; fleet health is served from EDC's own store plus per-asset `rope_getLedgerStatus` refresh. |
| "AccessGrant governance object — FEASIBLE" | **BUILT in EDC**: `rope-edc::grants` (§5), anchored on-chain as `AccessGrantIssued` / `AccessGrantRevoked` knots. |
| "External REST/WebSocket API gateway — TO BUILD" | **BUILT in EDC**: grant-scoped REST + Server-Sent Events stream + CSV export (§5.4). |
| "Evolutive-interface dashboard shell — TO BUILD" | **BUILT in EDC**: `crates/rope-edc/static/` — the Braincities/Bosch facet-search pattern (facet tabs, provenance-labeled result cards, live metrics panel, filter rail) generated from the project's own inventory taxonomy (§7). |
| "dcscan projects page" (only implied in v1.0 §6) | **SPECIFIED + BUILT**: dcscan.io `/ecosystem` page + `/api/v1/ecosystem/projects` (§8). |

The consequence: the EDC is no longer "application-layer engineering to be scheduled" —
it is a shipping crate (`crates/rope-edc`) with the v1.0 wizard, the AccessGrant engine,
the AI analytics layer, the stakeholder gateway, and the dcscan listing implemented.

---

## 2. Architecture

```
                       ┌────────────────────────────────────────────────────┐
                       │        PROJECT'S PRIMARY NODE (own IP)             │
                       │                                                    │
 field sensors ──MQTT──►  rope-iot-gateway ──► rope-node (loopback RPC)     │
 field sensors ──CoAP──►        │                   │                       │
 cloud feeds ───HTTP───►        │                   │  personal strings,    │
                       │        ▼                   │  OES, erasure         │
                       │   ┌─────────┐              │                       │
 owner / team ──HTTPS──►   │ rope-edc│──────────────┘                       │
 (console UI)          │   │  :9310  │                                      │
                       │   │         │──► rope-deployer (provisioning)      │
 regulator ────HTTPS───►   │         │──► AlterOSOrchestrator               │
 investor  (stakeholder│   │         │     ├─ Alteros routing (local model) │
 government  dashboard,│   │         │     ├─ Anthropic Claude API          │
 data buyer) API, SSE) │   └─────────┘     └─ OpenAI API                    │
                       └────────────────────────────────────────────────────┘
                                   │  public project card knot
                                   ▼
                     Rope registry wallet 0x…ec01 (EcosystemProjectRegistered)
                                   │
                                   ▼
                        dcscan.io /ecosystem  (auto-listed, links out to
                        each project's own stakeholder dashboard)
```

Sovereignty invariants preserved end to end:

1. **The dashboard runs on the project's node, not the Foundation's.** `rope-edc` is a
   single self-contained binary + static assets; `rope-deployer` installs it on the
   primary node at provisioning time. A regulator connecting to a municipality's
   dashboard connects to the municipality's own IP.
2. **AI provider credentials live in the project node's environment** (`ANTHROPIC_API_KEY`,
   `OPENAI_API_KEY`, `EDC_OLLAMA_ENDPOINT`), never centralized at the Foundation.
3. **dcscan.io holds only the public project card** (name, category, geography, status,
   stakeholder-link URL) — anchored on the Rope by the project itself, so the listing is
   automatic, tamper-evident, and rebuildable from chain.

---

## 3. The nine-step wizard (v1.0 §3–4, unchanged) — implementation binding

| Step | v1.0 section | `rope-edc` binding |
|---|---|---|
| 1 Identity & Compliance | 4.1 | `Project.identity` — DID + ONCHAINID fields; claim verification delegated to the T-REX stack already deployed |
| 2 Project Definition | 4.2 | `PUT /api/v1/edc/projects/:id/definition`; opens the project-kind genesis string at deploy |
| 3 Crypto Asset Decision | 4.3 | `PUT …/crypto` — `CryptoAssetKind::{None,Dcr20,Erc3643,Dcnft,Hybrid}` + full supply/rights sub-form |
| 4 Team & Governance | 4.4 | `PUT …/team` — `Role::{Owner,Administrator,Operator,Auditor,AiAgent}`; sensitive actions carry `timelock_eta` |
| 5 Asset & Data Source Inventory | 4.5 | `PUT …/inventory` (manual) + `POST …/inventory/import` (CSV, RFC-4180) across all five layers: assets, sensors, mesh nodes, external sources, AI agents |
| 6 Sovereign Node Sizing | 5 | `GET …/sizing` — recommendation computed from the inventory just entered (§4 below) |
| 7 Data Governance & Mutability | 8 | `PUT …/governance` — per-data-type `MutabilityClass` policy mirroring `rope-core::types::MutabilityClass` |
| 8 Review & Deploy | 9 | `POST …/deploy` — anchors the project string, provisions nodes via `rope-deployer`, flips status to Live |
| 9 Live Console | 6 | fleet dashboard + stakeholder access management (§5–§7) |

## 4. Node sizing (v1.0 §5) — the recommendation function

Implemented in `rope-edc::types::NodePlan::recommend`. Inputs: total asset count,
aggregate event rate (readings/hour computed from every sensor's declared cadence),
jurisdiction count. The v1.0 tier table is the normative output; the algorithm:

```
tier   = max(tier_by_assets, tier_by_event_rate, tier_by_jurisdictions)
assets:       ≤50 Pilot | ≤500 Standard | ≤5k Growth | ≤50k LargeScale | else Sovereign
events/hour:  ≤600 Pilot | ≤6k Standard | ≤60k Growth | ≤600k LargeScale | else Sovereign
jurisdictions: 1 → no constraint | ≥2 → at least Sovereign/Multi-Site
```

Node roles per v1.0 §5 (Ingestion Gateway, Storage/Ledger, AI-Agent Host, optional
Federation Validator) are emitted in the plan; at Pilot/Standard a single Databox hosts
several roles. The owner can only override upward.

## 5. External stakeholder access (v1.0 §6) — the AccessGrant engine

### 5.1 The on-chain object

Every grant is a first-class record (`rope-edc::grants::AccessGrant`):

```json
{
  "id": "gr_9f3a…",
  "project_id": "prj_…",
  "grantee": {"kind": "wallet|did|claim_class|public", "value": "0x… | did:dwp:… | Regulator | *"},
  "stakeholder_class": "Regulator|Government|Investor|Public|CommercialBuyer",
  "scope": {"facets": ["assets","sensors","readings","diagnoses","approvals","external"], "asset_ids": [], "categories": []},
  "starts_at": 1783765000, "expires_at": 1815301000,
  "price": {"model": "free|one_time|subscription|metered", "amount": 0, "currency": "FAT|PROJECT_TOKEN|EUR", "period": "monthly"},
  "delivery": ["rest","stream","export"],
  "status": "pending_timelock|active|revoked|expired",
  "effective_at": 1783768600,
  "created_by": "0x…owner",
  "anchor_knot": "0x…"
}
```

Lifecycle rules (all enforced in code, not by convention):

- Creation and revocation are anchored on the project's governance string as
  `AccessGrantIssued` / `AccessGrantRevoked` knots — the full history of who was given
  access to what, for how long, at what price, is itself auditable.
- Grants whose `stakeholder_class` is `Regulator` or `Public` get
  `effective_at = now + EDC_TIMELOCK_DELAY_SECS` (default 3600). The pending window is
  visible via the public API before the grant takes effect — same discipline as the
  DCSwap governance Timelock.
- Expiry is enforced at request time; an expired grant authenticates nothing.

### 5.2 API keys are minted FROM grants

`POST /api/v1/edc/grants/:id/keys` derives a bearer token bound to the grant. The server
stores only `blake3(token)`; possession of the token is possession of the grant's exact
scope, duration, and metering terms. Revoking the grant (or its expiry) kills every key
minted from it instantly. Wallet-signature authentication (EIP-191, same scheme as the
Phase-2 destructive-RPC verifier) is accepted as an alternative for grantees identified
by wallet.

### 5.3 Metering

Every stakeholder request increments the grant's `calls` counter and stamps
`last_used_at`. For `metered` and `subscription` price models the counters are the
billing source of truth; settlement in FAT or the project token reuses the standard
transfer path, fiat settlement is delegated to the operator's payment processor with the
counters as the invoice basis.

### 5.4 Stakeholder surface (served from the project's own node)

| Endpoint | Purpose |
|---|---|
| `GET /api/v1/stakeholder/:token/project` | Project card + the grant's own scope/expiry (self-describing access) |
| `GET /api/v1/stakeholder/:token/data?facet=readings&asset=…&since=…` | Scoped, provenance-labeled rows |
| `GET /api/v1/stakeholder/:token/stream` | Server-Sent Events: live readings/diagnoses/approvals as they are written |
| `POST /api/v1/stakeholder/:token/ask` | Natural-language question → AI answer grounded in scoped data (§6) |
| `GET /api/v1/stakeholder/:token/export?facet=…` | CSV bulk export for auditors/statutory reporting |
| `GET /stakeholder?key=:token` | The stakeholder dashboard UI itself (§7) |

## 6. AI-powered analytics (v1.0 §6.4) — concrete design

### 6.0 The deterministic analytics catalogue (normative, implemented in `rope-edc::analytics`)

The AI layer never computes numbers itself. Every question is answered in two
stages: first the deterministic catalogue below runs over the scoped readings
and produces the **analytics dossier**; then (if a provider is configured) the
model narrates the dossier and selects charts. Every figure in an AI answer
traces to one of these methods applied to on-chain-anchored data. The
catalogue covers every known family of data-analytics methods relevant to
telemetry, predictive maintenance, and environmental monitoring:

| Family | Methods (all implemented as pure functions in `crates/rope-edc/src/analytics.rs`) |
|---|---|
| **Descriptive statistics** | count, sum, mean, median, mode, min/max, range, variance, standard deviation, coefficient of variation, percentiles (p05/p25/p75/p95), quartiles, IQR, skewness, excess kurtosis |
| **Time series** | time-bucketed resampling, simple moving average (SMA), exponential moving average (EMA), rate of change, least-squares linear trend (slope, intercept, R²), autocorrelation function, seasonality detection, additive decomposition (trend + seasonal + residual) |
| **Anomaly detection** | z-score outliers, modified z-score (median absolute deviation), IQR fences, declared-band breaches (Plage/Optimum/Frequence model from §4.5.2), EWMA control limits, CUSUM drift detection |
| **Forecasting** | linear-trend extrapolation, Holt double exponential smoothing (level + trend), Holt-Winters triple exponential smoothing (additive seasonal) |
| **Correlation** | Pearson product-moment, Spearman rank, lagged cross-correlation, full correlation matrix across sensors |
| **Distribution** | histogram, frequency table, normality assessment (Jarque-Bera statistic) |
| **Clustering / segmentation** | k-means (1-D value clustering), band segmentation |
| **Comparative / cohort** | group-by aggregation (per asset / sensor / category), top-N ranking, period-over-period delta |
| **Predictive maintenance / reliability** | degradation-slope → remaining useful life (RUL), MTBF, failure rate, availability %, cadence conformity |
| **Compliance** | in-optimum percentage, breach counts by severity, SLA conformity report |
| **Data quality** | completeness vs declared cadence, staleness, gap detection |

Adding a new analytics method means adding a deterministic function + unit
test to this module and registering it in the dossier builder
(`rope-edc::ai::build_dossier`) — the AI narration picks it up with no
prompt changes.

### 6.1 Provider layer

EDC does not implement providers; it instantiates the production
`rope_agent_runtime::ai::AlterOSOrchestrator`, which already:

- analyzes query complexity (`QueryAnalyzer`, ported from AlterOS `salad_llm_client`),
- routes security/code/complex queries to Anthropic, standard analytics to OpenAI or a
  local Ollama model, simple lookups to the cheapest healthy backend,
- health-checks each backend and fails over.

Configuration is environment-only, on the project's node:

| Variable | Meaning |
|---|---|
| `ANTHROPIC_API_KEY` / `EDC_ANTHROPIC_MODEL` | Claude access (default model `claude-3-haiku-20240307`) |
| `OPENAI_API_KEY` / `EDC_OPENAI_MODEL` | OpenAI access (default `gpt-4o-mini`) |
| `EDC_OLLAMA_ENDPOINT` / `EDC_OLLAMA_MODEL` | Local/Alteros-managed model |
| `EDC_AI_DISABLE` | Set to run the console with the deterministic analytics engine only |

### 6.2 The four functions and their guardrails

1. **Natural-language querying** (`/ask`): the question plus a bounded, scope-filtered
   snapshot of the underlying rows (never more than the grant allows) is sent to the
   orchestrator. The response is returned **with the grounding rows attached** — every
   AI statement is traceable to the specific on-chain data it was computed from.
2. **Automatic chart selection** (`/chart`): the AI returns a strict-JSON chart spec
   (`{"chart":"line|bar|donut|gauge|map","x":…,"y":…,"series":…,"title":…}`) which the
   dashboard renders locally with its own SVG engine — the model never returns markup,
   only a declarative spec, which is validated before rendering. When no AI provider is
   configured, a deterministic selector picks the chart from the data shape (time-series
   → line, categorical → bar, share-of-whole → donut, single KPI → gauge), so the
   dashboard is fully functional without any cloud dependency.
3. **Anomaly narration**: threshold breaches (the Plage/Optimum/Frequence bands declared
   per sensor in step 4.5.2) are narrated in plain language for non-technical
   stakeholders, with the triggering readings attached as grounding.
4. **Scheduled reports**: report definitions are stored per project; generation walks the
   same grounded pipeline and the output can be anchored as a `Custom` knot when the
   owner wants a permanent record of what was told to a regulator.

**Hard guardrail, enforced in code:** the AI layer holds no writer handle. It is
constructed over a read-only view of the project store; the only write path for AI output
is the explicit owner-triggered "anchor this report" action.

## 7. The evolutive interface (v1.0 §6.5) — binding

The dashboard shell (`crates/rope-edc/static/`) implements the 2019 Braincities/Bosch
pattern, generated from the project's own data rather than hand-built per project:

- **Facet tabs**: `All | Assets | Sensors | Readings | Diagnoses | Approvals | External
  Sources` — each tab shows its live result count and query time ("About 345 references
  in 0.66 seconds" style), computed from the project store.
- **Result cards**: every card is provenance-labeled — which sensor produced the reading,
  which agent produced the diagnosis, which wallet signed the approval — and deep-links
  to the underlying knot on dcscan.io.
- **Metrics panel**: updates to whatever entity is selected (asset health score, last
  reading, cadence conformity, open recommendations), the same way the Bosch interface
  showed active/terminated role counts per selection.
- **Filter rail**: category, sub-type, geography, health band, time window.
- **Theming**: `EDC_THEME` JSON (palette, logo, facet taxonomy overrides) — the
  customizable-template requirement; the shell reads it at startup, no rebuild needed.

## 8. dcscan.io public projects page — NEW (normative)

- Every deployed project anchors a **public project card** as an
  `EcosystemProjectRegistered` knot on the well-known registry wallet
  `0x…ec01` (`EDC_REGISTRY_WALLET`), exactly the pattern production already uses for the
  node-request queue (`0x…d001`). The card contains only owner-approved public fields:
  name, archetype, category tags, geography (coarse), status, asset count band, crypto
  asset kind, and the **stakeholder access URL** (the project's own node).
- dc-explorer exposes `GET /api/v1/ecosystem/directory` (filterable by
  `archetype`, `country`, `status`, `q`) and `GET /api/v1/ecosystem/directory/:id`
  (live detail proxied from the project's own EDC instance, including its current
  public grant offers and stakeholder API base). The directory is aggregated every
  60 s from the EDC instances listed in `EDC_DIRECTORY_URLS` (comma-separated base
  URLs; each instance serves `GET /api/v1/ecosystem/public/projects`); partial
  instance failures degrade gracefully and the last-known directory is preserved.
  The `/ecosystem` page lists every project with: status pill, archetype,
  geography, asset/sensor counts, on-chain string link, and a "Data Access" card
  that goes **directly to the project's own stakeholder gateway** — the
  disintermediated link for regulators, government entities, and investors.
  dcscan is a directory, not a middleman: no project data transits the Foundation.
  (Namespace note: `/api/v1/projects` remains the pre-existing community-voting
  endpoint; the EDC directory lives under `/api/v1/ecosystem/*`.)
- Cards are updated by appending a newer `EcosystemProjectRegistered` knot (last-write-
  wins per project id) and removed with `EcosystemProjectDelisted`.

## 9. Deployment & template model (v1.0 §6.6) — binding

| Layer | Implementation | Where it runs |
|---|---|---|
| Console + dashboard shell | `rope-edc` static assets | project primary node, `:9310` |
| Visualization engine | self-contained SVG chart renderer (no CDN dependency — air-gapped/sovereign deployments stay functional) | same |
| AI orchestration | `AlterOSOrchestrator` behind `rope-edc::ai` | same |
| API gateway | `rope-edc::api` (axum) — grant-scoped REST/SSE/export | same |
| Node provisioning | `rope-deployer` `ProviderRegistry` (Exoscale, DigitalOcean, Local) invoked by `POST …/deploy` | Foundation service or project node |
| Field ingestion | `rope-iot-gateway` (MQTT/CoAP) + `POST …/telemetry` (HTTP bridge) | project ingestion node |

## 10. Security model

- Owner/team console endpoints are bound to the project's team roster: every mutating
  call carries the caller wallet and is checked against the v1.0 §4.4 role matrix.
- Sensitive actions (grants touching Regulator/Public, erasure outside GDPR triggers,
  decommission) carry the timelock delay and are publicly visible while pending.
- Stakeholder endpoints authenticate exclusively via grant-derived keys (§5.2) — there is
  no generic API-key system to misconfigure.
- The EDC never stores plaintext bearer tokens, only `blake3` digests.
- All on-chain writes go through the co-located rope-node over loopback — the V11
  destructive-RPC gate treats co-located callers as internal; remote/public callers
  cannot forge writes through the EDC because the EDC only writes what its
  role-checked endpoints produce.

## 11. What remains open (carried from v1.0 §11, updated)

- Scale-tier thresholds (§4) are now encoded; engineering sign-off can amend constants in
  one place (`rope-edc::types`).
- Console usage pricing, external-data gas metering, cross-border residency legal review,
  and the regulator mandatory-free-tier question remain policy decisions — the engine
  supports every outcome (grants can be free, priced, or time-boxed per jurisdiction).
- QR/NFC tagging workflow: the data model carries the tag field; the mobile scan app is
  the remaining build item (tracked separately, not blocking).
- LoRaWAN ingestion: `rope-iot-gateway` covers MQTT/CoAP/HTTP today; LoRaWAN network
  servers (ChirpStack et al.) integrate today via their MQTT bridge, native LNS support
  tracked separately.
