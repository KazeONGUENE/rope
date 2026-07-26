-- =============================================================================
-- Datachain Rope - AI Testimony Agents Registry
-- M2 (2026-07-25 security audit): rope-explorer's crates/rope-explorer/src/db.rs
-- (`AGENT_QUERY`, `list_agents`, `get_agent`, `get_agent_by_wallet`) queries an
-- `ai_agents` table that no committed SQL migration ever created. Every call
-- to that Postgres path therefore always fails (relation does not exist), and
-- `/api/v1/ai-agents` silently falls back to the hardcoded
-- `canonical_ai_agents()` / `CANONICAL_AGENT_WALLETS` Rust constants in
-- main.rs on every request. That fallback kept the endpoint from ever
-- returning an empty list, but it also meant:
--   1. an orphaned, permanently-failing Postgres round trip on every request
--      when DATABASE_URL is configured (wasted I/O + `tracing::warn!` noise),
--   2. no way to ever manage the AI-agent roster from the database — any
--      future admin tooling that INSERTs/UPDATEs `ai_agents` rows would
--      silently have no effect on `/api/v1/ai-agents` until this migration
--      landed, because the table did not exist to write to.
--
-- This migration creates the table with the exact column set/types that
-- crates/rope-explorer/src/db.rs::parse_agent_row expects, and seeds it with
-- the five canonical always-on agents documented in
-- `.cursor/rules/handover-canonical-agents-live-from-rope-2026-05-05.mdc`
-- (SemanticAgent / OracleAgent / InsuranceAgent / ValidationAgent /
-- ComplianceAgent, wallets 0x...C001-0x...C005), so that once DATABASE_URL is
-- configured the live DB-backed path in `list_ai_agents_live()` actually
-- succeeds end-to-end instead of falling through to the hardcoded fallback.
--
-- Idempotent: safe to re-run (CREATE ... IF NOT EXISTS / ON CONFLICT DO
-- NOTHING). NOTE: like every file in deploy/init-db/, Postgres only executes
-- files under /docker-entrypoint-initdb.d on a container's FIRST boot against
-- an empty data volume (see deploy/docker-compose.yml). On an
-- already-initialized production database, apply this file manually once:
--   psql "$DATABASE_URL" -f deploy/init-db/03-ai-agents.sql
-- =============================================================================

CREATE TABLE IF NOT EXISTS ai_agents (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    agent_type      TEXT NOT NULL,
    wallet_address  TEXT NOT NULL,
    icon            TEXT NOT NULL DEFAULT 'fa-robot',
    icon_class      TEXT NOT NULL DEFAULT 'fa-solid',
    description     TEXT NOT NULL DEFAULT '',
    org             TEXT NOT NULL DEFAULT '',
    tags            TEXT[] NOT NULL DEFAULT '{}',
    services        TEXT[] NOT NULL DEFAULT '{}',
    -- db.rs casts this to text before parsing as f64 client-side (avoids
    -- lossy binary NUMERIC<->f64 driver conversion); keep it NUMERIC here.
    reward_rate_fat NUMERIC(38, 18) NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'active',
    health_url      TEXT,
    created_at      TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

-- get_agent_by_wallet() matches on LOWER(wallet_address) = LOWER($1); a
-- case-sensitive UNIQUE constraint would not stop two rows differing only by
-- address casing from colliding under that lookup, so index (and constrain)
-- the lowercased form directly.
CREATE UNIQUE INDEX IF NOT EXISTS idx_ai_agents_wallet_lower
    ON ai_agents (LOWER(wallet_address));

CREATE INDEX IF NOT EXISTS idx_ai_agents_status ON ai_agents(status);
CREATE INDEX IF NOT EXISTS idx_ai_agents_created_at ON ai_agents(created_at ASC);

COMMENT ON TABLE ai_agents IS
    'Canonical + operator-managed Datachain Rope AI Testimony Agents. Backs '
    '/api/v1/ai-agents (crates/rope-explorer/src/db.rs). Falls back to the '
    'hardcoded canonical_ai_agents()/CANONICAL_AGENT_WALLETS constants in '
    'main.rs when this table is empty, missing, or unreachable.';

-- Seed the five canonical always-on agents (per
-- handover-canonical-agents-live-from-rope-2026-05-05.mdc). reward_rate_fat
-- is intentionally 0 — no governance-approved per-testimony FAT reward rate
-- has been set for these agents; do not fabricate a figure here. Wallet
-- addresses and health URLs match crates/rope-explorer/src/main.rs's
-- CANONICAL_AGENT_WALLETS constant exactly so DB-backed and fallback paths
-- never disagree on identity.
INSERT INTO ai_agents (
    id, name, agent_type, wallet_address, icon, icon_class, description, org,
    tags, services, reward_rate_fat, status, health_url, created_at
) VALUES
    (
        'semantic', 'SemanticAgent', 'Semantic Analysis',
        '0x000000000000000000000000000000000000C001',
        'fa-brain', 'fa-solid',
        'Indexes Datachain Rope strings, tags event_type fields, and exposes semantic search across knots.',
        'Datachain Foundation',
        ARRAY['canonical', 'index-checkpoint', 'search'],
        ARRAY['knot search', 'testimony indexing', 'merkle checkpoint anchoring'],
        0, 'active', 'http://127.0.0.1:9092/v1/health', '2026-05-05T07:30:00Z'
    ),
    (
        'oracle', 'OracleAgent', 'Price Oracle',
        '0x000000000000000000000000000000000000C002',
        'fa-chart-line', 'fa-solid',
        'Publishes DC FAT and stablecoin price testimonies sourced from DCSwap reserves and external feeds (XDCScan, GeckoTerminal).',
        'Datachain Foundation',
        ARRAY['canonical', 'price-attestation', 'oracle'],
        ARRAY['price feed attestation', 'outlier-rejected VWAP reconciliation'],
        0, 'active', NULL, '2026-05-05T07:30:00Z'
    ),
    (
        'insurance', 'InsuranceAgent', 'Risk Underwriting',
        '0x000000000000000000000000000000000000C003',
        'fa-shield-halved', 'fa-solid',
        'Issues parametric-insurance attestations against tokenized RWAs (Tanastok asset shares, NaturaProof biodiversity proofs).',
        'Datachain Foundation',
        ARRAY['canonical', 'insurance-attestation', 'rwa'],
        ARRAY['parametric insurance attestation', 'tokenized-asset feed polling'],
        0, 'active', NULL, '2026-05-05T07:30:00Z'
    ),
    (
        'validation', 'ValidationAgent', 'Knot Validation',
        '0x000000000000000000000000000000000000C004',
        'fa-circle-check', 'fa-solid',
        'Verifies post-quantum signatures (ML-DSA-65 default) on knots and witnesses the cord anchor knot at federation level.',
        'Datachain Foundation',
        ARRAY['canonical', 'signature-validation', 'testimony-consensus'],
        ARRAY['hybrid Ed25519+Dilithium3 signature verification', 'cord anchor witnessing'],
        0, 'active', NULL, '2026-05-05T07:30:00Z'
    ),
    (
        'compliance', 'ComplianceAgent', 'Regulatory Compliance',
        '0x000000000000000000000000000000000000C005',
        'fa-gavel', 'fa-solid',
        'Flags GDPR Art. 17 erasure requests and orchestrates rope_untieKnot tombstone knots; covers MiFID II / DORA reporting.',
        'Datachain Foundation',
        ARRAY['canonical', 'compliance-report', 'gdpr'],
        ARRAY['GDPR Article 17 erasure orchestration', 'MiFID II event batching', 'DORA incident anchoring'],
        0, 'active', 'http://127.0.0.1:9091/v1/health', '2026-05-05T07:30:00Z'
    )
ON CONFLICT (id) DO NOTHING;
