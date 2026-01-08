-- =============================================================================
-- Datachain Rope - Database Initialization
-- DC Explorer PostgreSQL Schema
-- =============================================================================

-- Enable extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pg_trgm";  -- For text search

-- =============================================================================
-- CORE TABLES
-- =============================================================================

-- Strings (equivalent to blocks)
CREATE TABLE IF NOT EXISTS strings (
    id BYTEA PRIMARY KEY,                    -- 32-byte StringId
    version SMALLINT NOT NULL DEFAULT 1,
    creator_id BYTEA NOT NULL,               -- 32-byte NodeId
    timestamp BIGINT NOT NULL,
    content_hash BYTEA NOT NULL,
    mutability_class SMALLINT NOT NULL,
    retention_policy JSONB,
    signature BYTEA NOT NULL,
    finality_status SMALLINT NOT NULL DEFAULT 0,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    indexed_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_strings_creator ON strings(creator_id);
CREATE INDEX idx_strings_timestamp ON strings(timestamp DESC);
CREATE INDEX idx_strings_finality ON strings(finality_status);

-- String parents (DAG relationships)
CREATE TABLE IF NOT EXISTS string_parents (
    string_id BYTEA NOT NULL REFERENCES strings(id),
    parent_id BYTEA NOT NULL,
    position SMALLINT NOT NULL,
    PRIMARY KEY (string_id, parent_id)
);

CREATE INDEX idx_string_parents_parent ON string_parents(parent_id);

-- Complements (Reed-Solomon encoded data)
CREATE TABLE IF NOT EXISTS complements (
    string_id BYTEA PRIMARY KEY REFERENCES strings(id),
    parity_shards BYTEA NOT NULL,
    regeneration_hints JSONB,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Testimonies (attestations)
CREATE TABLE IF NOT EXISTS testimonies (
    id BYTEA PRIMARY KEY,
    target_string_id BYTEA NOT NULL REFERENCES strings(id),
    witness_id BYTEA NOT NULL,
    attestation_type SMALLINT NOT NULL,
    timestamp BIGINT NOT NULL,
    signature BYTEA NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_testimonies_target ON testimonies(target_string_id);
CREATE INDEX idx_testimonies_witness ON testimonies(witness_id);
CREATE INDEX idx_testimonies_timestamp ON testimonies(timestamp DESC);

-- Anchors (consensus synchronization points)
CREATE TABLE IF NOT EXISTS anchors (
    id BYTEA PRIMARY KEY,
    epoch BIGINT NOT NULL,
    timestamp BIGINT NOT NULL,
    finalized_strings BYTEA[] NOT NULL,
    merkle_root BYTEA NOT NULL,
    signature BYTEA NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_anchors_epoch ON anchors(epoch DESC);

-- =============================================================================
-- ACCOUNTS & TOKENS
-- =============================================================================

-- Addresses/Accounts
CREATE TABLE IF NOT EXISTS accounts (
    id BYTEA PRIMARY KEY,                    -- 32-byte NodeId (public key hash)
    public_key_ed25519 BYTEA,
    public_key_dilithium BYTEA,
    balance NUMERIC(78, 0) NOT NULL DEFAULT 0,  -- Up to 10^78 (more than enough for 18 decimals)
    nonce BIGINT NOT NULL DEFAULT 0,
    first_seen BIGINT NOT NULL,
    last_active BIGINT NOT NULL,
    is_validator BOOLEAN DEFAULT FALSE,
    stake NUMERIC(78, 0) DEFAULT 0,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_accounts_balance ON accounts(balance DESC);
CREATE INDEX idx_accounts_validator ON accounts(is_validator) WHERE is_validator = TRUE;

-- Transactions
CREATE TABLE IF NOT EXISTS transactions (
    id BYTEA PRIMARY KEY,                    -- Transaction hash
    string_id BYTEA NOT NULL REFERENCES strings(id),
    from_address BYTEA NOT NULL,
    to_address BYTEA,
    value NUMERIC(78, 0) NOT NULL DEFAULT 0,
    data BYTEA,
    nonce BIGINT NOT NULL,
    gas_limit BIGINT NOT NULL,
    gas_price NUMERIC(78, 0) NOT NULL,
    gas_used BIGINT,
    status SMALLINT NOT NULL DEFAULT 0,      -- 0: pending, 1: success, 2: failed
    error_message TEXT,
    timestamp BIGINT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_transactions_string ON transactions(string_id);
CREATE INDEX idx_transactions_from ON transactions(from_address);
CREATE INDEX idx_transactions_to ON transactions(to_address);
CREATE INDEX idx_transactions_timestamp ON transactions(timestamp DESC);

-- Token transfers (DC-20)
CREATE TABLE IF NOT EXISTS token_transfers (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    transaction_id BYTEA NOT NULL REFERENCES transactions(id),
    token_address BYTEA NOT NULL,
    from_address BYTEA NOT NULL,
    to_address BYTEA NOT NULL,
    value NUMERIC(78, 0) NOT NULL,
    log_index SMALLINT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_token_transfers_tx ON token_transfers(transaction_id);
CREATE INDEX idx_token_transfers_token ON token_transfers(token_address);
CREATE INDEX idx_token_transfers_from ON token_transfers(from_address);
CREATE INDEX idx_token_transfers_to ON token_transfers(to_address);

-- =============================================================================
-- VALIDATORS & GOVERNANCE
-- =============================================================================

-- Validators
CREATE TABLE IF NOT EXISTS validators (
    id BYTEA PRIMARY KEY REFERENCES accounts(id),
    name VARCHAR(255),
    description TEXT,
    website VARCHAR(255),
    stake NUMERIC(78, 0) NOT NULL,
    commission_rate NUMERIC(5, 4) NOT NULL DEFAULT 0.1,
    status SMALLINT NOT NULL DEFAULT 0,      -- 0: pending, 1: active, 2: jailed, 3: unbonding
    jailed_until BIGINT,
    reputation_score INTEGER DEFAULT 0,
    strings_created BIGINT DEFAULT 0,
    testimonies_given BIGINT DEFAULT 0,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_validators_stake ON validators(stake DESC);
CREATE INDEX idx_validators_status ON validators(status);

-- Foundation members
CREATE TABLE IF NOT EXISTS foundation_members (
    id BYTEA PRIMARY KEY REFERENCES accounts(id),
    name VARCHAR(255) NOT NULL,
    role VARCHAR(100),
    voting_power SMALLINT NOT NULL DEFAULT 1,
    active BOOLEAN DEFAULT TRUE,
    added_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Minting proposals
CREATE TABLE IF NOT EXISTS minting_proposals (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    proposer_id BYTEA NOT NULL REFERENCES accounts(id),
    amount NUMERIC(78, 0) NOT NULL,
    reason TEXT NOT NULL,
    status SMALLINT NOT NULL DEFAULT 0,      -- 0: pending_ai, 1: pending_governors, 2: pending_foundation, 3: approved, 4: rejected
    ai_approvals JSONB DEFAULT '[]',
    governor_approvals JSONB DEFAULT '[]',
    foundation_approvals JSONB DEFAULT '[]',
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    resolved_at TIMESTAMP WITH TIME ZONE
);

CREATE INDEX idx_minting_proposals_status ON minting_proposals(status);

-- =============================================================================
-- STATISTICS & ANALYTICS
-- =============================================================================

-- Network statistics (updated periodically)
CREATE TABLE IF NOT EXISTS network_stats (
    id SERIAL PRIMARY KEY,
    timestamp BIGINT NOT NULL,
    total_strings BIGINT NOT NULL,
    total_transactions BIGINT NOT NULL,
    total_accounts BIGINT NOT NULL,
    active_validators INTEGER NOT NULL,
    total_stake NUMERIC(78, 0) NOT NULL,
    strings_per_second NUMERIC(10, 2),
    average_finality_ms INTEGER,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE INDEX idx_network_stats_timestamp ON network_stats(timestamp DESC);

-- Daily statistics
CREATE TABLE IF NOT EXISTS daily_stats (
    date DATE PRIMARY KEY,
    strings_created BIGINT DEFAULT 0,
    transactions_count BIGINT DEFAULT 0,
    unique_addresses BIGINT DEFAULT 0,
    volume NUMERIC(78, 0) DEFAULT 0,
    gas_used BIGINT DEFAULT 0,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- =============================================================================
-- FUNCTIONS & TRIGGERS
-- =============================================================================

-- Function to update timestamp
CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Apply to validators
CREATE TRIGGER trigger_validators_updated_at
    BEFORE UPDATE ON validators
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at();

-- =============================================================================
-- INITIAL DATA
-- =============================================================================

-- Insert genesis string (placeholder)
-- This will be replaced with actual genesis data on first run

COMMENT ON TABLE strings IS 'Core String Lattice entries (equivalent to blocks in blockchain)';
COMMENT ON TABLE testimonies IS 'Attestations from validators';
COMMENT ON TABLE accounts IS 'Network accounts with DC FAT balances';
COMMENT ON TABLE transactions IS 'All network transactions';
COMMENT ON TABLE validators IS 'Active and historical validators';

