-- =============================================================================
-- LaunchLab Database Schema
-- PostgreSQL Schema for LaunchLab Platform
-- =============================================================================

-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pg_trgm";

-- UUID v7 function (time-based, sortable)
CREATE OR REPLACE FUNCTION uuid_generate_v7()
RETURNS UUID AS $$
DECLARE
    timestamp_ms BIGINT;
    uuid_bytes BYTEA;
BEGIN
    timestamp_ms := (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT;
    uuid_bytes := decode(
        lpad(to_hex(timestamp_ms), 12, '0') ||
        lpad(to_hex((random() * 65535)::INT), 4, '0') ||
        '8' || lpad(to_hex((random() * 4095)::INT), 3, '0') ||
        lpad(to_hex((random() * 1099511627775)::BIGINT), 12, '0'),
        'hex'
    );
    RETURN encode(uuid_bytes, 'hex')::UUID;
END;
$$ LANGUAGE plpgsql;

-- =============================================================================
-- PROJECT OWNER IDENTITIES
-- =============================================================================

CREATE TABLE IF NOT EXISTS launchlab_identities (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    wallet_address BYTEA NOT NULL UNIQUE,
    nft_token_id BIGINT,
    current_metadata_cid TEXT,
    display_name TEXT,
    avatar_cid TEXT,
    bio TEXT,
    website TEXT,
    twitter_handle TEXT,
    telegram_handle TEXT,
    discord_handle TEXT,
    kyc_verified BOOLEAN DEFAULT FALSE,
    kyc_provider TEXT,
    kyc_verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    last_sync_at TIMESTAMPTZ
);

CREATE INDEX idx_identities_wallet ON launchlab_identities(wallet_address);
CREATE INDEX idx_identities_display_name ON launchlab_identities USING gin(display_name gin_trgm_ops);

-- =============================================================================
-- PROJECTS
-- =============================================================================

CREATE TABLE IF NOT EXISTS launchlab_projects (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    identity_id UUID NOT NULL REFERENCES launchlab_identities(id),
    name TEXT NOT NULL,
    symbol TEXT NOT NULL,
    description TEXT,
    short_description TEXT,
    logo_cid TEXT,
    banner_cid TEXT,
    category TEXT NOT NULL,  -- 'token', 'rwa', 'nft', 'security'
    
    -- Asset configuration
    asset_type TEXT NOT NULL,  -- 'DC20', 'ERC3643', 'DCNFT', 'DC721', 'DC1155'
    contract_address BYTEA,
    implementation_address BYTEA,
    deployment_tx BYTEA,
    decimals SMALLINT DEFAULT 18,
    total_supply NUMERIC(78, 0),
    
    -- RWA specific
    rwa_asset_type TEXT,  -- 'forest', 'vehicle', 'watch', etc.
    rwa_location TEXT,
    rwa_valuation_usd NUMERIC(38, 2),
    rwa_valuation_date TIMESTAMPTZ,
    rwa_certificate_cid TEXT,
    rwa_verification_status TEXT DEFAULT 'pending',  -- 'pending', 'verified', 'disputed'
    
    -- Metadata
    metadata_cid TEXT,
    website TEXT,
    whitepaper_url TEXT,
    github_url TEXT,
    
    -- Status
    status TEXT NOT NULL DEFAULT 'draft',  -- 'draft', 'active', 'paused', 'deprecated'
    launched_at TIMESTAMPTZ,
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_projects_identity ON launchlab_projects(identity_id);
CREATE INDEX idx_projects_contract ON launchlab_projects(contract_address);
CREATE INDEX idx_projects_status ON launchlab_projects(status);
CREATE INDEX idx_projects_category ON launchlab_projects(category);
CREATE INDEX idx_projects_name ON launchlab_projects USING gin(name gin_trgm_ops);

-- =============================================================================
-- LIQUIDITY POOLS
-- =============================================================================

CREATE TABLE IF NOT EXISTS launchlab_pools (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL REFERENCES launchlab_projects(id),
    pool_address BYTEA NOT NULL UNIQUE,
    token0_address BYTEA NOT NULL,
    token1_address BYTEA NOT NULL,
    token0_symbol TEXT NOT NULL,
    token1_symbol TEXT NOT NULL,
    is_zero_fee BOOLEAN DEFAULT FALSE,
    
    -- Current state
    reserve0 NUMERIC(78, 0) DEFAULT 0,
    reserve1 NUMERIC(78, 0) DEFAULT 0,
    tvl_usd NUMERIC(38, 2) DEFAULT 0,
    volume_24h_usd NUMERIC(38, 2) DEFAULT 0,
    volume_7d_usd NUMERIC(38, 2) DEFAULT 0,
    apy NUMERIC(10, 4),
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_pools_project ON launchlab_pools(project_id);
CREATE INDEX idx_pools_address ON launchlab_pools(pool_address);

-- =============================================================================
-- BOT FARM TABLES
-- =============================================================================

-- Bot wallet pool
CREATE TABLE IF NOT EXISTS launchlab_bot_wallets (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL REFERENCES launchlab_projects(id),
    wallet_index INTEGER NOT NULL,
    address BYTEA NOT NULL,
    encrypted_private_key BYTEA NOT NULL,
    encryption_key_id TEXT NOT NULL,  -- Reference to KMS key
    
    -- Balances (cached)
    balance_native NUMERIC(78, 0) DEFAULT 0,
    balance_token0 NUMERIC(78, 0) DEFAULT 0,
    balance_token1 NUMERIC(78, 0) DEFAULT 0,
    
    nonce BIGINT DEFAULT 0,
    strategy_assignment TEXT,
    profile_type TEXT,
    is_active BOOLEAN DEFAULT FALSE,
    
    -- Stats
    total_trades INTEGER DEFAULT 0,
    total_volume_usd NUMERIC(38, 2) DEFAULT 0,
    last_trade_at TIMESTAMPTZ,
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    
    UNIQUE(project_id, wallet_index)
);

CREATE INDEX idx_bot_wallets_project ON launchlab_bot_wallets(project_id);
CREATE INDEX idx_bot_wallets_strategy ON launchlab_bot_wallets(strategy_assignment);
CREATE INDEX idx_bot_wallets_active ON launchlab_bot_wallets(is_active) WHERE is_active = TRUE;

-- Bot strategies
CREATE TABLE IF NOT EXISTS launchlab_bot_strategies (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL REFERENCES launchlab_projects(id),
    
    strategy_type TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    
    -- Configuration
    config JSONB NOT NULL,
    
    -- Wallet allocation
    wallet_count INTEGER NOT NULL,
    wallet_start_index INTEGER NOT NULL,
    wallet_end_index INTEGER NOT NULL,
    
    -- State
    is_active BOOLEAN DEFAULT FALSE,
    priority INTEGER DEFAULT 50,
    
    -- Stats
    total_executions BIGINT DEFAULT 0,
    successful_executions BIGINT DEFAULT 0,
    total_volume_usd NUMERIC(38, 2) DEFAULT 0,
    pnl_usd NUMERIC(38, 2) DEFAULT 0,
    
    last_execution_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- Valid strategy types check
ALTER TABLE launchlab_bot_strategies
ADD CONSTRAINT valid_strategy_type CHECK (
    strategy_type IN (
        'market_maker', 'cross_pair_arb', 'stable_trader', 'retail_sim',
        'whale', 'scalper', 'momentum', 'lp_manager', 'dca',
        'volume_generator', 'price_support', 'organic_growth',
        'wash_trading_defense', 'time_weighted_accumulator', 'volatility_trader'
    )
);

CREATE INDEX idx_strategies_project ON launchlab_bot_strategies(project_id);
CREATE INDEX idx_strategies_type ON launchlab_bot_strategies(strategy_type);
CREATE INDEX idx_strategies_active ON launchlab_bot_strategies(is_active) WHERE is_active = TRUE;

-- Bot execution log (partitioned by time)
CREATE TABLE IF NOT EXISTS launchlab_bot_executions (
    id UUID DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL,
    strategy_id UUID NOT NULL,
    wallet_id UUID NOT NULL,
    execution_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    action TEXT NOT NULL,  -- 'buy', 'sell', 'add_liquidity', 'remove_liquidity'
    pool_address BYTEA NOT NULL,
    token_in BYTEA NOT NULL,
    token_out BYTEA NOT NULL,
    amount_in NUMERIC(78, 0) NOT NULL,
    amount_out NUMERIC(78, 0) NOT NULL,
    price NUMERIC(38, 18),
    price_impact_bps INTEGER,
    
    gas_used BIGINT,
    gas_price NUMERIC(78, 0),
    tx_hash BYTEA,
    block_number BIGINT,
    
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'submitted', 'success', 'failed', 'reverted'
    error_message TEXT,
    
    PRIMARY KEY (id, execution_time)
) PARTITION BY RANGE (execution_time);

-- Create partitions for the next 12 months
DO $$
DECLARE
    start_date DATE := DATE_TRUNC('month', CURRENT_DATE);
    end_date DATE;
    partition_name TEXT;
BEGIN
    FOR i IN 0..11 LOOP
        end_date := start_date + INTERVAL '1 month';
        partition_name := 'launchlab_bot_executions_' || TO_CHAR(start_date, 'YYYY_MM');
        
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF launchlab_bot_executions
             FOR VALUES FROM (%L) TO (%L)',
            partition_name, start_date, end_date
        );
        
        start_date := end_date;
    END LOOP;
END $$;

CREATE INDEX idx_executions_project_time ON launchlab_bot_executions(project_id, execution_time DESC);
CREATE INDEX idx_executions_strategy ON launchlab_bot_executions(strategy_id, execution_time DESC);
CREATE INDEX idx_executions_status ON launchlab_bot_executions(status) WHERE status != 'success';

-- =============================================================================
-- MARKET MAKER TABLES
-- =============================================================================

CREATE TABLE IF NOT EXISTS launchlab_market_maker_configs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL REFERENCES launchlab_projects(id),
    pool_address BYTEA NOT NULL,
    
    is_active BOOLEAN DEFAULT FALSE,
    
    -- Spread configuration
    target_spread_bps INTEGER NOT NULL DEFAULT 100,
    min_spread_bps INTEGER NOT NULL DEFAULT 25,
    max_spread_bps INTEGER NOT NULL DEFAULT 500,
    
    -- Volume configuration
    target_daily_volume_usd NUMERIC(38, 2),
    min_trade_size_usd NUMERIC(38, 2) DEFAULT 10,
    max_trade_size_usd NUMERIC(38, 2) DEFAULT 10000,
    
    -- Price stability
    price_deviation_tolerance_bps INTEGER DEFAULT 500,
    rebalance_threshold_bps INTEGER DEFAULT 200,
    
    -- Correlation settings
    follow_market_leader TEXT,  -- 'BTC', 'ETH', 'SP500', 'GOLD', NULL
    correlation_strength NUMERIC(3, 2) DEFAULT 0.50,
    
    -- Time settings
    operating_hours_start TIME DEFAULT '00:00',
    operating_hours_end TIME DEFAULT '24:00',
    operating_timezone TEXT DEFAULT 'UTC',
    trade_frequency_seconds INTEGER DEFAULT 30,
    
    -- Risk limits
    max_inventory_imbalance_pct NUMERIC(5, 2) DEFAULT 30.0,
    daily_loss_limit_usd NUMERIC(38, 2),
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    
    UNIQUE(project_id, pool_address)
);

CREATE INDEX idx_mm_configs_project ON launchlab_market_maker_configs(project_id);
CREATE INDEX idx_mm_configs_active ON launchlab_market_maker_configs(is_active) WHERE is_active = TRUE;

-- Market maker real-time state
CREATE TABLE IF NOT EXISTS launchlab_market_maker_state (
    config_id UUID PRIMARY KEY REFERENCES launchlab_market_maker_configs(id),
    
    current_bid NUMERIC(38, 18),
    current_ask NUMERIC(38, 18),
    current_spread_bps INTEGER,
    
    inventory_token0 NUMERIC(78, 0) DEFAULT 0,
    inventory_token1 NUMERIC(78, 0) DEFAULT 0,
    inventory_ratio NUMERIC(5, 4),
    
    realized_pnl_usd NUMERIC(38, 2) DEFAULT 0,
    unrealized_pnl_usd NUMERIC(38, 2) DEFAULT 0,
    volume_today_usd NUMERIC(38, 2) DEFAULT 0,
    trades_today INTEGER DEFAULT 0,
    
    last_trade_at TIMESTAMPTZ,
    last_rebalance_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- =============================================================================
-- LISTING SUBMISSION TABLES
-- =============================================================================

CREATE TABLE IF NOT EXISTS launchlab_listing_submissions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL REFERENCES launchlab_projects(id),
    
    platform TEXT NOT NULL,  -- 'coinmarketcap', 'coingecko', 'defillama', 'dextools'
    status TEXT NOT NULL DEFAULT 'draft',  -- 'draft', 'validating', 'ready', 'submitted', 'pending_review', 'approved', 'rejected', 'listed'
    
    -- Submission data
    submission_data JSONB NOT NULL,
    auto_filled_data JSONB,
    manual_overrides JSONB,
    
    -- Validation
    validation_errors JSONB,
    validation_warnings JSONB,
    last_validated_at TIMESTAMPTZ,
    
    -- Tracking
    submitted_at TIMESTAMPTZ,
    submission_id TEXT,  -- Platform's submission ID
    response_at TIMESTAMPTZ,
    response_data JSONB,
    
    -- Result
    listing_url TEXT,
    listed_at TIMESTAMPTZ,
    rejection_reason TEXT,
    
    -- Retry tracking
    retry_count INTEGER DEFAULT 0,
    last_retry_at TIMESTAMPTZ,
    next_retry_at TIMESTAMPTZ,
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_listings_project ON launchlab_listing_submissions(project_id);
CREATE INDEX idx_listings_platform ON launchlab_listing_submissions(platform);
CREATE INDEX idx_listings_status ON launchlab_listing_submissions(status);

-- Platform credentials (encrypted)
CREATE TABLE IF NOT EXISTS launchlab_listing_credentials (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    identity_id UUID NOT NULL REFERENCES launchlab_identities(id),
    platform TEXT NOT NULL,
    encrypted_api_key BYTEA,
    encrypted_api_secret BYTEA,
    encryption_key_id TEXT,
    oauth_token_encrypted BYTEA,
    oauth_refresh_token_encrypted BYTEA,
    oauth_expires_at TIMESTAMPTZ,
    is_valid BOOLEAN DEFAULT TRUE,
    last_validated_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    
    UNIQUE(identity_id, platform)
);

-- =============================================================================
-- CAMPAIGN TABLES
-- =============================================================================

CREATE TABLE IF NOT EXISTS launchlab_campaigns (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL REFERENCES launchlab_projects(id),
    
    name TEXT NOT NULL,
    description TEXT,
    type TEXT NOT NULL,  -- 'social', 'airdrop', 'bounty', 'otc', 'referral', 'liquidity_mining', 'trading_competition'
    status TEXT NOT NULL DEFAULT 'draft',  -- 'draft', 'scheduled', 'active', 'paused', 'completed', 'cancelled'
    
    -- Budget
    budget_token_address BYTEA,
    budget_token_symbol TEXT,
    budget_amount NUMERIC(78, 0) NOT NULL,
    spent_amount NUMERIC(78, 0) DEFAULT 0,
    reserved_amount NUMERIC(78, 0) DEFAULT 0,
    
    -- Schedule
    start_at TIMESTAMPTZ,
    end_at TIMESTAMPTZ,
    timezone TEXT DEFAULT 'UTC',
    
    -- Targeting
    targeting JSONB,  -- geo, min_balance, exclude_addresses, require_kyc
    
    -- Configuration (type-specific)
    config JSONB NOT NULL,
    
    -- Metrics
    metrics JSONB DEFAULT '{}',
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_campaigns_project ON launchlab_campaigns(project_id);
CREATE INDEX idx_campaigns_type ON launchlab_campaigns(type);
CREATE INDEX idx_campaigns_status ON launchlab_campaigns(status);
CREATE INDEX idx_campaigns_active ON launchlab_campaigns(status, start_at, end_at) 
    WHERE status IN ('scheduled', 'active');

-- Social posts
CREATE TABLE IF NOT EXISTS launchlab_social_posts (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    campaign_id UUID NOT NULL REFERENCES launchlab_campaigns(id),
    
    platform TEXT NOT NULL,  -- 'twitter', 'facebook', 'instagram', 'telegram', 'discord'
    status TEXT NOT NULL DEFAULT 'draft',  -- 'draft', 'scheduled', 'posting', 'posted', 'failed'
    
    scheduled_at TIMESTAMPTZ,
    content TEXT NOT NULL,
    media_cids TEXT[],
    hashtags TEXT[],
    mentions TEXT[],
    
    -- Platform response
    platform_post_id TEXT,
    platform_url TEXT,
    posted_at TIMESTAMPTZ,
    error_message TEXT,
    
    -- Engagement metrics
    impressions INTEGER DEFAULT 0,
    likes INTEGER DEFAULT 0,
    comments INTEGER DEFAULT 0,
    shares INTEGER DEFAULT 0,
    clicks INTEGER DEFAULT 0,
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_posts_campaign ON launchlab_social_posts(campaign_id);
CREATE INDEX idx_posts_scheduled ON launchlab_social_posts(scheduled_at) 
    WHERE status = 'scheduled';
CREATE INDEX idx_posts_platform ON launchlab_social_posts(platform);

-- Airdrop campaigns
CREATE TABLE IF NOT EXISTS launchlab_airdrop_campaigns (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    campaign_id UUID NOT NULL REFERENCES launchlab_campaigns(id) UNIQUE,
    
    contract_address BYTEA,
    merkle_root BYTEA,
    proofs_cid TEXT,  -- IPFS CID with all proofs
    
    total_recipients INTEGER DEFAULT 0,
    claimed_count INTEGER DEFAULT 0,
    
    claim_delay_seconds INTEGER DEFAULT 0,
    
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Airdrop claims
CREATE TABLE IF NOT EXISTS launchlab_airdrop_claims (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    airdrop_id UUID NOT NULL REFERENCES launchlab_airdrop_campaigns(id),
    
    recipient_address BYTEA NOT NULL,
    amount NUMERIC(78, 0) NOT NULL,
    merkle_proof BYTEA[],
    
    initiated_at TIMESTAMPTZ,
    is_claimed BOOLEAN DEFAULT FALSE,
    claimed_at TIMESTAMPTZ,
    claim_tx_hash BYTEA,
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    
    UNIQUE(airdrop_id, recipient_address)
);

CREATE INDEX idx_claims_airdrop ON launchlab_airdrop_claims(airdrop_id);
CREATE INDEX idx_claims_recipient ON launchlab_airdrop_claims(recipient_address);
CREATE INDEX idx_claims_unclaimed ON launchlab_airdrop_claims(airdrop_id) 
    WHERE is_claimed = FALSE;

-- OTC deals
CREATE TABLE IF NOT EXISTS launchlab_otc_deals (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    campaign_id UUID REFERENCES launchlab_campaigns(id),
    project_id UUID NOT NULL REFERENCES launchlab_projects(id),
    
    seller_address BYTEA NOT NULL,
    buyer_address BYTEA NOT NULL,
    
    token_address BYTEA NOT NULL,
    amount NUMERIC(78, 0) NOT NULL,
    price_per_token NUMERIC(38, 18) NOT NULL,
    total_value_usd NUMERIC(38, 2),
    
    payment_currency TEXT NOT NULL,  -- 'USDC', 'USDT', 'FAT', 'fiat'
    payment_details JSONB,
    
    -- Vesting
    vesting_enabled BOOLEAN DEFAULT FALSE,
    vesting_tge_bps INTEGER,
    vesting_cliff_seconds INTEGER,
    vesting_duration_seconds INTEGER,
    
    -- Escrow
    escrow_contract BYTEA,
    escrow_funded BOOLEAN DEFAULT FALSE,
    
    status TEXT NOT NULL DEFAULT 'proposed',  -- 'proposed', 'accepted', 'funded', 'executing', 'completed', 'cancelled', 'disputed'
    
    proposed_at TIMESTAMPTZ DEFAULT NOW(),
    accepted_at TIMESTAMPTZ,
    funded_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_otc_project ON launchlab_otc_deals(project_id);
CREATE INDEX idx_otc_seller ON launchlab_otc_deals(seller_address);
CREATE INDEX idx_otc_buyer ON launchlab_otc_deals(buyer_address);
CREATE INDEX idx_otc_status ON launchlab_otc_deals(status);

-- =============================================================================
-- AUDIT TRAIL
-- =============================================================================

CREATE TABLE IF NOT EXISTS launchlab_audit_log (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    identity_id UUID REFERENCES launchlab_identities(id),
    project_id UUID REFERENCES launchlab_projects(id),
    
    action TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id UUID,
    
    old_values JSONB,
    new_values JSONB,
    
    ip_address INET,
    user_agent TEXT,
    
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_audit_identity ON launchlab_audit_log(identity_id, created_at DESC);
CREATE INDEX idx_audit_project ON launchlab_audit_log(project_id, created_at DESC);
CREATE INDEX idx_audit_action ON launchlab_audit_log(action);

-- =============================================================================
-- FUNCTIONS & TRIGGERS
-- =============================================================================

-- Update timestamp trigger
CREATE OR REPLACE FUNCTION launchlab_update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Apply to all relevant tables
DO $$
DECLARE
    t TEXT;
BEGIN
    FOR t IN 
        SELECT table_name 
        FROM information_schema.columns 
        WHERE table_schema = 'public' 
        AND column_name = 'updated_at'
        AND table_name LIKE 'launchlab_%'
    LOOP
        EXECUTE format(
            'DROP TRIGGER IF EXISTS trigger_%I_updated_at ON %I',
            t, t
        );
        EXECUTE format(
            'CREATE TRIGGER trigger_%I_updated_at
             BEFORE UPDATE ON %I
             FOR EACH ROW
             EXECUTE FUNCTION launchlab_update_updated_at()',
            t, t
        );
    END LOOP;
END $$;

-- =============================================================================
-- VIEWS
-- =============================================================================

-- Project overview with stats
CREATE OR REPLACE VIEW launchlab_project_overview AS
SELECT 
    p.id,
    p.name,
    p.symbol,
    p.category,
    p.status,
    p.asset_type,
    encode(p.contract_address, 'hex') as contract_address,
    i.display_name as owner_name,
    encode(i.wallet_address, 'hex') as owner_address,
    COALESCE(pool_stats.tvl_usd, 0) as total_tvl_usd,
    COALESCE(pool_stats.volume_24h_usd, 0) as total_volume_24h_usd,
    COALESCE(bot_stats.active_bots, 0) as active_bots,
    COALESCE(bot_stats.active_strategies, 0) as active_strategies,
    COALESCE(campaign_stats.active_campaigns, 0) as active_campaigns,
    p.created_at,
    p.launched_at
FROM launchlab_projects p
JOIN launchlab_identities i ON p.identity_id = i.id
LEFT JOIN LATERAL (
    SELECT 
        SUM(tvl_usd) as tvl_usd,
        SUM(volume_24h_usd) as volume_24h_usd
    FROM launchlab_pools 
    WHERE project_id = p.id
) pool_stats ON TRUE
LEFT JOIN LATERAL (
    SELECT 
        COUNT(*) FILTER (WHERE is_active) as active_bots
    FROM launchlab_bot_wallets 
    WHERE project_id = p.id
) bot_stats ON TRUE
LEFT JOIN LATERAL (
    SELECT 
        COUNT(*) FILTER (WHERE is_active) as active_strategies
    FROM launchlab_bot_strategies 
    WHERE project_id = p.id
) strat_stats ON TRUE
LEFT JOIN LATERAL (
    SELECT 
        COUNT(*) FILTER (WHERE status = 'active') as active_campaigns
    FROM launchlab_campaigns 
    WHERE project_id = p.id
) campaign_stats ON TRUE;

-- =============================================================================
-- COMMENTS
-- =============================================================================

COMMENT ON TABLE launchlab_identities IS 'Project owner identities, synced from on-chain NFT';
COMMENT ON TABLE launchlab_projects IS 'Projects created through LaunchLab';
COMMENT ON TABLE launchlab_pools IS 'Liquidity pools associated with projects';
COMMENT ON TABLE launchlab_bot_wallets IS 'Bot wallet pool for trading automation';
COMMENT ON TABLE launchlab_bot_strategies IS 'Strategy configurations for bot farm';
COMMENT ON TABLE launchlab_bot_executions IS 'Trade execution history (partitioned by time)';
COMMENT ON TABLE launchlab_market_maker_configs IS 'Market maker configurations per pool';
COMMENT ON TABLE launchlab_market_maker_state IS 'Real-time market maker state';
COMMENT ON TABLE launchlab_listing_submissions IS 'CMC/CoinGecko listing submissions';
COMMENT ON TABLE launchlab_campaigns IS 'Marketing campaigns (social, airdrop, OTC, etc.)';
COMMENT ON TABLE launchlab_social_posts IS 'Scheduled social media posts';
COMMENT ON TABLE launchlab_airdrop_campaigns IS 'Airdrop campaign details';
COMMENT ON TABLE launchlab_airdrop_claims IS 'Individual airdrop claims';
COMMENT ON TABLE launchlab_otc_deals IS 'OTC deal records';
COMMENT ON TABLE launchlab_audit_log IS 'Audit trail for all changes';
