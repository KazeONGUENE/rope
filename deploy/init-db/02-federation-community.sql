-- =============================================================================
-- Datachain Rope - Federation & Community Generation Schema
-- Based on Federation Generation Schema (2018) and Ecosystemic Autonomous Maintenance
-- =============================================================================

-- =============================================================================
-- FEDERATION GENERATION
-- Federation creation and management following the schema:
-- CREATE -> GENERATE -> Federation -> Banking/Global -> Protocols -> Identity -> Predictability -> Wallet
-- =============================================================================

-- Federation Types (Structured, Unstructured, Autonomous)
CREATE TYPE federation_type AS ENUM (
    'structured',       -- City, Object, Contributors
    'unstructured',     -- Real-Madrid, Fans, Painter, Musicians
    'autonomous'        -- AI, Expert Systems, Bot, Script
);

-- Federation Structure (Monocellular, Multicellular)
CREATE TYPE federation_structure AS ENUM (
    'monocellular',     -- Single-entity federation
    'multicellular'     -- Multi-entity consortium
);

-- Federation Scope
CREATE TYPE federation_scope AS ENUM (
    'global',
    'regional', 
    'local'
);

-- Industry Sectors
CREATE TYPE industry_sector AS ENUM (
    'banking',
    'healthcare',
    'automotive',
    'mobility',
    'hospitality',
    'human_rights',
    'energy',
    'agricultural',
    'public_institution',
    'technology',
    'entertainment',
    'education',
    'retail',
    'logistics',
    'manufacturing'
);

-- Federations table
CREATE TABLE IF NOT EXISTS federations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(255) NOT NULL,
    description TEXT,
    federation_type federation_type NOT NULL DEFAULT 'structured',
    structure federation_structure NOT NULL DEFAULT 'monocellular',
    scope federation_scope NOT NULL DEFAULT 'regional',
    industry industry_sector NOT NULL,
    
    -- Creator and ownership
    creator_id BYTEA NOT NULL REFERENCES accounts(id),
    
    -- Instance configuration from schema
    instance_url VARCHAR(255),           -- Connect to instance configuration web panel
    genesis_entry BYTEA,                 -- Link to genesis entry in Datachain main net
    
    -- Wallet generation (10,000,000 per federation as per schema)
    data_wallets_count BIGINT DEFAULT 10000000,
    data_wallets_generated BIGINT DEFAULT 0,
    
    -- Individual chains (10,000,000 per federation)
    individual_chains_count BIGINT DEFAULT 10000000,
    individual_chains_generated BIGINT DEFAULT 0,
    
    -- Protocol invocations
    native_protocols JSONB DEFAULT '["datachain"]',  -- Native DC, Hyperledger, NXT, EOS, etc.
    external_protocols JSONB DEFAULT '[]',            -- Wanchain, Lisk, Ethereum, Blockchain, etc.
    
    -- Identity & Compliance
    kyc_aml_enabled BOOLEAN DEFAULT TRUE,
    identity_protocols JSONB DEFAULT '["epassport", "iso_iec_24760"]',
    swift_integration BOOLEAN DEFAULT FALSE,
    sepa_integration BOOLEAN DEFAULT FALSE,
    
    -- Predictability & AI
    predictability_enabled BOOLEAN DEFAULT TRUE,
    ai_features JSONB DEFAULT '["adaptability", "matching", "retracement", "contract_mining", "risk_management", "fraud_detection", "scoring"]',
    
    -- Crypto integration
    crypto_currencies JSONB DEFAULT '["dc", "bitcoin", "eth", "eos", "wan"]',
    consensus_type VARCHAR(50) DEFAULT 'PoA',    -- Consensus (PoA) as shown in schema
    
    -- Status
    status VARCHAR(50) DEFAULT 'pending_vote',   -- pending_vote, active, suspended
    vote_count_for INTEGER DEFAULT 0,
    vote_count_against INTEGER DEFAULT 0,
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    activated_at TIMESTAMP WITH TIME ZONE
);

CREATE INDEX idx_federations_creator ON federations(creator_id);
CREATE INDEX idx_federations_type ON federations(federation_type);
CREATE INDEX idx_federations_industry ON federations(industry);
CREATE INDEX idx_federations_status ON federations(status);

-- =============================================================================
-- COMMUNITY GENERATION
-- Community creation following the schema:
-- CREATE -> GENERATE -> Community -> Banking/Global -> Protocols -> KYC/AML -> Predictability -> Wallet
-- =============================================================================

-- Community table (similar to Federation but community-owned)
CREATE TABLE IF NOT EXISTS communities (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(255) NOT NULL,
    description TEXT,
    
    -- Community ownership
    federation_id UUID REFERENCES federations(id),
    creator_id BYTEA NOT NULL REFERENCES accounts(id),
    
    -- Community type from schema annotation:
    -- "Federation creation and other asset description: type, scale, number of entries, 
    -- identity of the grid Federation or Community operator are instanced within the Datachain main net"
    community_type VARCHAR(100),
    scale VARCHAR(50),                   -- small, medium, large, enterprise
    max_entries BIGINT,
    
    -- Instance configuration
    instance_url VARCHAR(255),
    genesis_entry BYTEA,
    
    -- Wallet generation (10,000,000 per community)
    data_wallets_count BIGINT DEFAULT 10000000,
    data_wallets_generated BIGINT DEFAULT 0,
    
    -- Protocol configuration (inherited from federation or custom)
    native_protocols JSONB DEFAULT '["datachain"]',
    external_protocols JSONB DEFAULT '[]',
    
    -- KYC/AML Configuration (from schema)
    kyc_aml_enabled BOOLEAN DEFAULT TRUE,
    transaction_monitoring BOOLEAN DEFAULT TRUE,
    swift_integration BOOLEAN DEFAULT FALSE,
    sepa_integration BOOLEAN DEFAULT FALSE,
    
    -- Predictability features
    predictability_enabled BOOLEAN DEFAULT TRUE,
    ai_features JSONB DEFAULT '["adaptability", "matching", "retracement", "contract_mining", "risk_management", "fraud_detection", "scoring"]',
    
    -- Wallet & Crypto
    wallet_enabled BOOLEAN DEFAULT TRUE,
    consensus_enabled BOOLEAN DEFAULT TRUE,
    web_services_enabled BOOLEAN DEFAULT TRUE,
    crypto_currencies JSONB DEFAULT '["dc", "bitcoin", "eth", "eos", "wan"]',
    
    -- Status
    status VARCHAR(50) DEFAULT 'pending_vote',
    vote_count_for INTEGER DEFAULT 0,
    vote_count_against INTEGER DEFAULT 0,
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    activated_at TIMESTAMP WITH TIME ZONE
);

CREATE INDEX idx_communities_federation ON communities(federation_id);
CREATE INDEX idx_communities_creator ON communities(creator_id);
CREATE INDEX idx_communities_status ON communities(status);

-- =============================================================================
-- PROJECT SUBMISSIONS
-- "Start Building" submissions for community voting
-- =============================================================================

-- Project categories
CREATE TYPE project_category AS ENUM (
    'defi',
    'nft',
    'gaming',
    'social',
    'infrastructure',
    'dao',
    'marketplace',
    'identity',
    'supply_chain',
    'healthcare',
    'iot',
    'ai_ml',
    'oracle',
    'bridge',
    'other'
);

-- Project stage
CREATE TYPE project_stage AS ENUM (
    'idea',
    'prototype',
    'mvp',
    'beta',
    'production'
);

-- Project submissions table
CREATE TABLE IF NOT EXISTS project_submissions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Project information
    name VARCHAR(255) NOT NULL,
    tagline VARCHAR(500),
    description TEXT NOT NULL,
    category project_category NOT NULL,
    stage project_stage NOT NULL DEFAULT 'idea',
    
    -- Submitter
    submitter_id BYTEA NOT NULL REFERENCES accounts(id),
    submitter_name VARCHAR(255),
    submitter_email VARCHAR(255),
    organization_name VARCHAR(255),
    organization_type VARCHAR(100),      -- individual, business, institution
    
    -- Technical specifications
    tech_stack JSONB DEFAULT '[]',       -- ["Rust", "TypeScript", "Solidity"]
    architecture_description TEXT,
    
    -- Functional specifications
    features JSONB DEFAULT '[]',         -- Array of feature objects
    use_cases TEXT,
    target_users TEXT,
    
    -- Protocol integration
    required_protocols JSONB DEFAULT '["datachain"]',
    external_integrations JSONB DEFAULT '[]',
    
    -- AI requirements
    requires_ai_testimony BOOLEAN DEFAULT FALSE,
    ai_agent_requirements TEXT,
    
    -- Resource requirements
    estimated_gas_usage BIGINT,
    storage_requirements TEXT,
    bandwidth_requirements TEXT,
    
    -- Timeline
    estimated_launch_date DATE,
    milestones JSONB DEFAULT '[]',       -- Array of milestone objects
    
    -- Documentation
    whitepaper_url VARCHAR(500),
    documentation_url VARCHAR(500),
    github_url VARCHAR(500),
    website_url VARCHAR(500),
    demo_url VARCHAR(500),
    
    -- Team
    team_members JSONB DEFAULT '[]',     -- Array of team member objects
    
    -- Funding (if applicable)
    funding_requested NUMERIC(78, 0) DEFAULT 0,
    funding_currency VARCHAR(10) DEFAULT 'FAT',
    funding_breakdown TEXT,
    
    -- Voting
    status VARCHAR(50) DEFAULT 'pending_review',  -- pending_review, voting, approved, rejected, building, launched
    voting_starts_at TIMESTAMP WITH TIME ZONE,
    voting_ends_at TIMESTAMP WITH TIME ZONE,
    vote_count_for INTEGER DEFAULT 0,
    vote_count_against INTEGER DEFAULT 0,
    required_votes INTEGER DEFAULT 100,          -- Minimum votes needed
    approval_threshold NUMERIC(5,2) DEFAULT 0.51, -- 51% approval needed
    
    -- Review
    reviewed_by BYTEA REFERENCES accounts(id),
    review_notes TEXT,
    reviewed_at TIMESTAMP WITH TIME ZONE,
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    approved_at TIMESTAMP WITH TIME ZONE,
    launched_at TIMESTAMP WITH TIME ZONE
);

CREATE INDEX idx_projects_submitter ON project_submissions(submitter_id);
CREATE INDEX idx_projects_category ON project_submissions(category);
CREATE INDEX idx_projects_status ON project_submissions(status);
CREATE INDEX idx_projects_stage ON project_submissions(stage);
CREATE INDEX idx_projects_voting ON project_submissions(status, voting_ends_at) WHERE status = 'voting';

-- =============================================================================
-- VOTING SYSTEM
-- DC FAT holders vote on federations, communities, and projects
-- =============================================================================

-- Vote types
CREATE TYPE vote_target_type AS ENUM (
    'federation',
    'community',
    'project',
    'proposal',
    'minting'
);

-- Votes table
CREATE TABLE IF NOT EXISTS votes (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Voter (must hold DC FAT)
    voter_id BYTEA NOT NULL REFERENCES accounts(id),
    voter_stake NUMERIC(78, 0) NOT NULL,     -- Stake at time of voting (voting power)
    
    -- Target
    target_type vote_target_type NOT NULL,
    target_id UUID NOT NULL,
    
    -- Vote
    vote_for BOOLEAN NOT NULL,               -- true = for, false = against
    vote_weight NUMERIC(78, 0) NOT NULL,     -- Weighted by stake
    comment TEXT,
    
    -- Verification
    signature BYTEA NOT NULL,
    transaction_hash BYTEA,
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    
    -- One vote per voter per target
    UNIQUE(voter_id, target_type, target_id)
);

CREATE INDEX idx_votes_target ON votes(target_type, target_id);
CREATE INDEX idx_votes_voter ON votes(voter_id);
CREATE INDEX idx_votes_for ON votes(vote_for);

-- =============================================================================
-- DATAWALLETS
-- Generated wallets for federations/communities (10,000,000 each as per schema)
-- =============================================================================

CREATE TABLE IF NOT EXISTS datawallets (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Ownership
    federation_id UUID REFERENCES federations(id),
    community_id UUID REFERENCES communities(id),
    owner_id BYTEA REFERENCES accounts(id),
    
    -- Wallet addresses
    address BYTEA NOT NULL UNIQUE,
    public_key_ed25519 BYTEA,
    public_key_dilithium BYTEA,          -- Post-quantum
    
    -- Type
    wallet_type VARCHAR(50) DEFAULT 'standard',  -- standard, custodial, multisig
    
    -- Status
    is_activated BOOLEAN DEFAULT FALSE,
    activated_at TIMESTAMP WITH TIME ZONE,
    
    -- Metadata
    label VARCHAR(255),
    metadata JSONB DEFAULT '{}',
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_datawallets_federation ON datawallets(federation_id);
CREATE INDEX idx_datawallets_community ON datawallets(community_id);
CREATE INDEX idx_datawallets_owner ON datawallets(owner_id);
CREATE INDEX idx_datawallets_address ON datawallets(address);

-- =============================================================================
-- PROTOCOL INVOCATIONS
-- Track which protocols each federation/community uses
-- =============================================================================

CREATE TABLE IF NOT EXISTS protocol_invocations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Target
    federation_id UUID REFERENCES federations(id),
    community_id UUID REFERENCES communities(id),
    
    -- Protocol info (from schema)
    protocol_name VARCHAR(100) NOT NULL,
    protocol_type VARCHAR(50) NOT NULL,  -- native, external
    -- Native: Native DC, Hyperledger, NXT, EOS, Wanchain, Lisk, Ethereum, Blockchain, Tangle, Hashgraph, Gnutella, GSM, Bittorrent
    -- External: Depending on configuration
    
    -- Configuration
    config JSONB DEFAULT '{}',
    endpoint_url VARCHAR(500),
    
    -- Status
    is_active BOOLEAN DEFAULT TRUE,
    last_invoked_at TIMESTAMP WITH TIME ZONE,
    invocation_count BIGINT DEFAULT 0,
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_protocol_invocations_federation ON protocol_invocations(federation_id);
CREATE INDEX idx_protocol_invocations_community ON protocol_invocations(community_id);
CREATE INDEX idx_protocol_invocations_protocol ON protocol_invocations(protocol_name);

-- =============================================================================
-- ECOSYSTEMIC AUTONOMOUS MAINTENANCE
-- Supporting infrastructure for the use case diagram
-- =============================================================================

-- Stakeholder types
CREATE TYPE stakeholder_type AS ENUM (
    'municipality',
    'supplier',
    'ai_system',
    'maintenance_dept',
    'department_manager'
);

-- Stakeholders table
CREATE TABLE IF NOT EXISTS stakeholders (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    account_id BYTEA REFERENCES accounts(id),
    
    name VARCHAR(255) NOT NULL,
    stakeholder_type stakeholder_type NOT NULL,
    organization VARCHAR(255),
    
    -- From the diagram: Municipality, Suppliers, AI systems
    role_description TEXT,
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Universe of Assets (from diagram)
CREATE TABLE IF NOT EXISTS asset_universes (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    federation_id UUID REFERENCES federations(id),
    
    name VARCHAR(255) NOT NULL,
    description TEXT,
    
    -- Asset types (.csv, .json as shown in schema)
    supported_formats JSONB DEFAULT '["csv", "json"]',
    
    -- Onboarding
    datawallet_provider_id UUID REFERENCES datawallets(id),
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Assets in universes
CREATE TABLE IF NOT EXISTS universe_assets (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    universe_id UUID NOT NULL REFERENCES asset_universes(id),
    
    asset_identifier VARCHAR(255) NOT NULL,
    asset_type VARCHAR(100),
    asset_data JSONB,
    
    -- Onboarded via Datawallet
    datawallet_id UUID REFERENCES datawallets(id),
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Diagnosis records (from Ecosystemic Autonomous Maintenance)
CREATE TABLE IF NOT EXISTS diagnosis_records (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    asset_id UUID REFERENCES universe_assets(id),
    
    diagnosis_type VARCHAR(100) NOT NULL,  -- wear, failure, etc.
    diagnosis_details JSONB,
    
    ai_agent_id VARCHAR(255),              -- AI agent that performed diagnosis
    confidence_score NUMERIC(5,4),
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Maintenance recommendations (from diagram)
CREATE TABLE IF NOT EXISTS maintenance_recommendations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    diagnosis_id UUID REFERENCES diagnosis_records(id),
    
    recommendation_type VARCHAR(100) NOT NULL,  -- call_for_tender, repair, replace
    description TEXT,
    
    -- Service provider recommendation
    recommended_providers JSONB DEFAULT '[]',
    
    -- AI recommendation
    ai_agent_id VARCHAR(255),
    recommendation_score NUMERIC(5,4),
    
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- =============================================================================
-- TRIGGERS
-- =============================================================================

-- Update timestamp trigger for federations
CREATE TRIGGER trigger_federations_updated_at
    BEFORE UPDATE ON federations
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at();

-- Update timestamp trigger for communities
CREATE TRIGGER trigger_communities_updated_at
    BEFORE UPDATE ON communities
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at();

-- Update timestamp trigger for project_submissions
CREATE TRIGGER trigger_projects_updated_at
    BEFORE UPDATE ON project_submissions
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at();

-- =============================================================================
-- COMMENTS
-- =============================================================================

COMMENT ON TABLE federations IS 'Federation generation following the 2018 schema - Structured/Unstructured/Autonomous federations with protocol invocations';
COMMENT ON TABLE communities IS 'Community generation - Community-owned entities within federations';
COMMENT ON TABLE project_submissions IS 'Project submissions from "Start Building" button - require community vote for approval';
COMMENT ON TABLE votes IS 'Voting system for DC FAT holders to approve federations, communities, and projects';
COMMENT ON TABLE datawallets IS 'Generated wallets (10,000,000 per federation/community as per schema)';
COMMENT ON TABLE protocol_invocations IS 'Protocol integrations - Native DC, Hyperledger, NXT, EOS, Ethereum, etc.';


