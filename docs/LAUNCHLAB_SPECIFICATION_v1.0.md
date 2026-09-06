# LaunchLab Technical & Functional Specification v1.0

**Document Version:** 1.0.0  
**Date:** 2026-09-06  
**Author:** Datachain Foundation  
**Status:** SPECIFICATION — Ready for Implementation  
**Target Platform:** DCSwap on Datachain Rope (Chain ID: 271828)

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Architecture Overview](#2-architecture-overview)
3. [Data Model & Storage](#3-data-model--storage)
4. [Module 1: Trading Bot Farm](#4-module-1-trading-bot-farm)
5. [Module 2: Market Maker Service](#5-module-2-market-maker-service)
6. [Module 3: Automated Listing Service](#6-module-3-automated-listing-service)
7. [Module 4: Marketing Campaign Manager](#7-module-4-marketing-campaign-manager)
8. [API Specifications](#8-api-specifications)
9. [Smart Contracts](#9-smart-contracts)
10. [Security Considerations](#10-security-considerations)
11. [Deployment Architecture](#11-deployment-architecture)
12. [Migration Path](#12-migration-path)

---

## 1. Executive Summary

### 1.1 Vision

LaunchLab transforms DCSwap from a decentralized exchange into a comprehensive asset launch platform, enabling project owners to create, manage, and promote any tokenized asset—whether cryptocurrency, real-world assets (forests, vehicles, watches), securities, or NFTs—without relying on expensive intermediaries.

### 1.2 Core Objectives

| Objective | Description | Target Metric |
|-----------|-------------|---------------|
| **Democratize Market Making** | Replace $2,500+/month contractors with self-service tools | 90% cost reduction |
| **Automate Listings** | Digitize CMC/CoinGecko submission workflows | <24h from submission to listing |
| **Scale Trading Activity** | Bot farm with 500 wallets, 15+ strategies | 10M+ daily transactions |
| **Enable Marketing** | Shopify-like campaign management | 1-click campaign deployment |

### 1.3 Platform Participants

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           LAUNCHLAB ECOSYSTEM                                    │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐    ┌──────────────┐  │
│  │   PROJECT    │    │    ASSET     │    │  LIQUIDITY   │    │  COMMUNITY   │  │
│  │    OWNER     │    │   HOLDERS    │    │  PROVIDERS   │    │   MEMBERS    │  │
│  └──────┬───────┘    └──────┬───────┘    └──────┬───────┘    └──────┬───────┘  │
│         │                   │                   │                   │          │
│         └───────────────────┴───────────────────┴───────────────────┘          │
│                                    │                                            │
│                         ┌──────────▼──────────┐                                │
│                         │      LAUNCHLAB      │                                │
│                         │   Control Center    │                                │
│                         └──────────┬──────────┘                                │
│                                    │                                            │
│         ┌──────────────────────────┼──────────────────────────┐                │
│         │                          │                          │                │
│  ┌──────▼──────┐  ┌───────────────▼───────────────┐  ┌───────▼───────┐        │
│  │  BOT FARM   │  │      MARKET MAKER             │  │   MARKETING   │        │
│  │  500 Bots   │  │   Spread & Volume Control     │  │   CAMPAIGNS   │        │
│  │  15+ Strats │  │   No Intermediaries           │  │   X/FB/OTC    │        │
│  └─────────────┘  └───────────────────────────────┘  └───────────────┘        │
│                                    │                                            │
│                         ┌──────────▼──────────┐                                │
│                         │   AUTO-LISTING      │                                │
│                         │  CMC/CoinGecko/+    │                                │
│                         └─────────────────────┘                                │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 1.4 Supported Asset Types

| Asset Type | Standard | Compliance | Example |
|------------|----------|------------|---------|
| **Utility Token** | DC-20 (ERC-20) | None | Governance, Access |
| **Security Token** | ERC-3643 (T-REX) | KYC/AML/Accreditation | Equity, Bonds |
| **Real-World Asset** | DCNFT + ERC-3643 | Title Verification | Forest, Vehicle, Watch |
| **NFT Collection** | DC-721 (ERC-721) | Optional KYC | Collectibles |
| **Semi-Fungible** | DC-1155 (ERC-1155) | Configurable | Gaming, Tickets |

---

## 2. Architecture Overview

### 2.1 System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                          LAUNCHLAB SYSTEM ARCHITECTURE                          │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  FRONTEND LAYER                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │  LaunchLab Dashboard (React + TypeScript)                               │   │
│  │  ├── Project Creation Wizard                                            │   │
│  │  ├── Bot Farm Control Panel                                             │   │
│  │  ├── Market Maker Configuration                                         │   │
│  │  ├── Listing Status Tracker                                             │   │
│  │  └── Campaign Management Console                                        │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                         │                                       │
│  API GATEWAY LAYER                      ▼                                       │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │  Nginx Reverse Proxy (SSL Termination)                                  │   │
│  │  Rate Limiting: 1000 req/min per wallet                                 │   │
│  │  Authentication: JWT + Wallet Signature (EIP-712)                       │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                         │                                       │
│  SERVICE LAYER                          ▼                                       │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌─────────────┐              │
│  │ LaunchLab   │ │  Bot Farm   │ │   Market    │ │  Campaign   │              │
│  │ Core API    │ │  Orchestrator│ │   Maker    │ │  Engine     │              │
│  │ (Rust)      │ │  (Rust)     │ │  (Rust)     │ │  (TS)       │              │
│  │ Port: 3010  │ │  Port: 3011 │ │  Port: 3012 │ │  Port: 3013 │              │
│  └──────┬──────┘ └──────┬──────┘ └──────┬──────┘ └──────┬──────┘              │
│         │               │               │               │                       │
│  ┌──────┴───────────────┴───────────────┴───────────────┴──────┐              │
│  │                    MESSAGE BUS (Redis Streams)              │              │
│  │         Topics: bot.commands, mm.signals, campaign.events   │              │
│  └─────────────────────────────┬───────────────────────────────┘              │
│                                │                                               │
│  DATA LAYER                    ▼                                               │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌─────────────┐              │
│  │  PostgreSQL │ │   Redis     │ │    IPFS     │ │  Datachain  │              │
│  │  (Neon)     │ │  (Cache +   │ │  (Media +   │ │   Rope      │              │
│  │  Relational │ │   Queues)   │ │   Metadata) │ │   (Chain)   │              │
│  └─────────────┘ └─────────────┘ └─────────────┘ └─────────────┘              │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 2.2 Technology Stack

| Layer | Technology | Version | Purpose |
|-------|------------|---------|---------|
| **Frontend** | React + Vite + TypeScript | 18.x / 5.x / 5.x | Dashboard UI |
| **API Gateway** | Nginx | 1.25.x | SSL, Rate Limiting, Load Balancing |
| **Core Services** | Rust + Axum | 1.75+ / 0.7.x | High-performance API & Engine |
| **Campaign Service** | Node.js + TypeScript | 20.x LTS | Social API Integration |
| **Database** | PostgreSQL (Neon) | 16.x | Relational Data |
| **Cache/Queue** | Redis | 7.x | Caching, Message Bus |
| **Storage** | IPFS (Kubo) | 0.27.x | Decentralized Media Storage |
| **Blockchain** | Datachain Rope | 271828 | Smart Contracts, Settlement |

### 2.3 Integration Points

| System | Protocol | Endpoint | Purpose |
|--------|----------|----------|---------|
| **Datachain Rope RPC** | JSON-RPC | `https://erpc.datachain.network` | On-chain operations |
| **DCSwap Router** | EVM | `0x55e660B8ee61208381298382f8c3DEb3B8f7621b` | Swaps, Liquidity |
| **DCSwap Factory** | EVM | `0x17d2ACc47f20d93eedA29dB6252eF22d3D7699B7` | Pool Creation |
| **T-REX Registry** | EVM | `0xB28E38b344A7238C9777D74209F966D1873D26e0` | KYC/Compliance |
| **String Registry** | JSON-RPC | `rope_appendToString` | Entity History |
| **CoinMarketCap** | REST | `https://api.coinmarketcap.com` | Listing Submission |
| **CoinGecko** | REST | `https://api.coingecko.com` | Listing Submission |
| **X.com (Twitter)** | OAuth 2.0 | `https://api.twitter.com/2` | Social Campaigns |
| **Meta (Facebook)** | Graph API | `https://graph.facebook.com/v18.0` | Social Campaigns |

---

## 3. Data Model & Storage

### 3.1 Project Owner Identity (Wallet-Based NoSQL)

Project owner data is stored as a **LaunchLab Identity NFT** (LLI-NFT) in the owner's wallet. This NFT contains a continuously updated JSON document stored on IPFS, providing a pure NoSQL design where the owner controls their data.

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts-upgradeable/token/ERC721/ERC721Upgradeable.sol";
import "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";

/**
 * @title LaunchLabIdentity
 * @notice Non-transferable identity NFT storing project owner metadata
 * @dev One identity per wallet, metadata stored on IPFS
 */
contract LaunchLabIdentity is ERC721Upgradeable, OwnableUpgradeable, UUPSUpgradeable {
    
    // Mapping from tokenId to IPFS CID
    mapping(uint256 => string) private _metadataCIDs;
    
    // Mapping from wallet to tokenId (one identity per wallet)
    mapping(address => uint256) public walletToIdentity;
    
    // Counter for token IDs
    uint256 private _tokenIdCounter;
    
    // Events
    event IdentityCreated(address indexed owner, uint256 indexed tokenId, string cid);
    event IdentityUpdated(uint256 indexed tokenId, string oldCid, string newCid);
    
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }
    
    function initialize() public initializer {
        __ERC721_init("LaunchLab Identity", "LLI");
        __Ownable_init(msg.sender);
        __UUPSUpgradeable_init();
    }
    
    /**
     * @notice Mint a new identity NFT for the caller
     * @param initialCid IPFS CID of the initial metadata document
     */
    function createIdentity(string calldata initialCid) external returns (uint256) {
        require(walletToIdentity[msg.sender] == 0, "Identity already exists");
        require(bytes(initialCid).length > 0, "CID cannot be empty");
        
        _tokenIdCounter++;
        uint256 newTokenId = _tokenIdCounter;
        
        _safeMint(msg.sender, newTokenId);
        _metadataCIDs[newTokenId] = initialCid;
        walletToIdentity[msg.sender] = newTokenId;
        
        emit IdentityCreated(msg.sender, newTokenId, initialCid);
        return newTokenId;
    }
    
    /**
     * @notice Update the metadata CID for an identity
     * @param newCid New IPFS CID containing updated metadata
     */
    function updateMetadata(string calldata newCid) external {
        uint256 tokenId = walletToIdentity[msg.sender];
        require(tokenId != 0, "No identity found");
        require(ownerOf(tokenId) == msg.sender, "Not identity owner");
        
        string memory oldCid = _metadataCIDs[tokenId];
        _metadataCIDs[tokenId] = newCid;
        
        emit IdentityUpdated(tokenId, oldCid, newCid);
    }
    
    /**
     * @notice Get the current metadata CID for an identity
     */
    function getMetadataCID(uint256 tokenId) external view returns (string memory) {
        require(_ownerOf(tokenId) != address(0), "Identity does not exist");
        return _metadataCIDs[tokenId];
    }
    
    /**
     * @notice Override to make tokens non-transferable (soulbound)
     */
    function _update(address to, uint256 tokenId, address auth) internal override returns (address) {
        address from = _ownerOf(tokenId);
        // Allow minting (from == address(0)) but prevent transfers
        require(from == address(0), "Identity is non-transferable");
        return super._update(to, tokenId, auth);
    }
    
    function _authorizeUpgrade(address newImplementation) internal override onlyOwner {}
    
    function tokenURI(uint256 tokenId) public view override returns (string memory) {
        require(_ownerOf(tokenId) != address(0), "Token does not exist");
        return string(abi.encodePacked("ipfs://", _metadataCIDs[tokenId]));
    }
}
```

### 3.2 Identity Metadata Schema (IPFS JSON Document)

```typescript
/**
 * LaunchLab Identity Metadata Schema
 * Stored on IPFS, referenced by NFT
 */
interface LaunchLabIdentityMetadata {
  // Schema version for forward compatibility
  schemaVersion: "1.0.0";
  
  // Identity basics
  identity: {
    id: string;                        // UUID v7
    walletAddress: string;             // 0x... checksum address
    createdAt: string;                 // ISO 8601
    updatedAt: string;                 // ISO 8601
    displayName: string;               // Project owner name
    avatarCID?: string;                // IPFS CID of avatar
    bio?: string;
    website?: string;
    socialLinks?: {
      twitter?: string;
      telegram?: string;
      discord?: string;
      github?: string;
    };
  };
  
  // KYC/Compliance status (if applicable)
  compliance?: {
    kycVerified: boolean;
    kycProvider?: string;              // e.g., "onchainid", "sumsub"
    kycTimestamp?: string;
    accreditedInvestor?: boolean;
    jurisdictions?: string[];          // ISO 3166-1 alpha-2
  };
  
  // Projects owned by this identity
  projects: LaunchLabProject[];
  
  // Bot farm configuration
  botFarm?: BotFarmConfig;
  
  // Market maker configurations
  marketMaker?: MarketMakerConfig[];
  
  // Campaign history
  campaigns?: CampaignConfig[];
  
  // Listing submissions
  listings?: ListingSubmission[];
  
  // Statistics
  stats: {
    totalProjects: number;
    totalTVL: string;                  // USD value as string
    totalVolume24h: string;
    botsActive: number;
    campaignsActive: number;
  };
  
  // Previous metadata CIDs (for history)
  previousVersions: string[];
}

interface LaunchLabProject {
  id: string;                          // UUID v7
  name: string;
  description: string;
  logoCID: string;                     // IPFS CID
  bannerCID?: string;
  category: "token" | "rwa" | "nft" | "security" | "other";
  
  // Asset configuration
  asset: {
    type: "DC20" | "ERC3643" | "DCNFT" | "DC721" | "DC1155";
    contractAddress: string;
    deploymentTx: string;
    decimals?: number;
    totalSupply?: string;
    symbol: string;
    name: string;
  };
  
  // Pool information
  pools: {
    address: string;
    token0: string;
    token1: string;
    tvl: string;
    volume24h: string;
    apy?: string;
  }[];
  
  // Real-world asset details (if applicable)
  rwaDetails?: {
    assetType: string;                 // "forest", "vehicle", "watch", etc.
    location?: string;
    valuationUSD: string;
    valuationDate: string;
    certificateCID?: string;           // Legal document on IPFS
    verificationStatus: "pending" | "verified" | "disputed";
  };
  
  // Status
  status: "draft" | "active" | "paused" | "deprecated";
  createdAt: string;
  updatedAt: string;
}
```

### 3.3 PostgreSQL Schema (Operational Data)

```sql
-- =============================================================================
-- LaunchLab PostgreSQL Schema
-- Operational data that doesn't need to be on-chain
-- =============================================================================

-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pg_trgm";
CREATE EXTENSION IF NOT EXISTS "timescaledb" CASCADE;

-- =============================================================================
-- PROJECT OWNER CACHE (mirrors on-chain identity)
-- =============================================================================

CREATE TABLE IF NOT EXISTS launchlab_identities (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    wallet_address BYTEA NOT NULL UNIQUE,
    nft_token_id BIGINT,
    current_cid TEXT,
    display_name TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    last_sync_at TIMESTAMPTZ
);

CREATE INDEX idx_identities_wallet ON launchlab_identities(wallet_address);

-- =============================================================================
-- BOT FARM TABLES
-- =============================================================================

-- Bot wallet pool
CREATE TABLE IF NOT EXISTS bot_wallets (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL,
    wallet_index INTEGER NOT NULL,
    address BYTEA NOT NULL,
    encrypted_private_key BYTEA NOT NULL,  -- Encrypted with project owner's key
    balance_fat NUMERIC(78, 0) DEFAULT 0,
    balance_usdc NUMERIC(78, 0) DEFAULT 0,
    balance_usdt NUMERIC(78, 0) DEFAULT 0,
    balance_eurod NUMERIC(78, 0) DEFAULT 0,
    nonce BIGINT DEFAULT 0,
    strategy_assignment TEXT,
    profile_type TEXT,
    is_active BOOLEAN DEFAULT FALSE,
    last_trade_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(project_id, wallet_index)
);

CREATE INDEX idx_bot_wallets_project ON bot_wallets(project_id);
CREATE INDEX idx_bot_wallets_strategy ON bot_wallets(strategy_assignment);
CREATE INDEX idx_bot_wallets_active ON bot_wallets(is_active) WHERE is_active = TRUE;

-- Bot strategies configuration
CREATE TABLE IF NOT EXISTS bot_strategies (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL,
    strategy_type TEXT NOT NULL,  -- see enum below
    name TEXT NOT NULL,
    config JSONB NOT NULL,
    wallet_count INTEGER NOT NULL,
    wallet_start_index INTEGER NOT NULL,
    is_active BOOLEAN DEFAULT TRUE,
    priority INTEGER DEFAULT 50,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- Valid strategy types
COMMENT ON COLUMN bot_strategies.strategy_type IS 
'Valid types: market_maker, cross_pair_arb, stable_trader, retail_sim, 
whale, scalper, momentum, lp_manager, dca, volume_generator, 
price_support, organic_growth, wash_trading_defense, 
time_weighted_accumulator, volatility_trader';

CREATE INDEX idx_strategies_project ON bot_strategies(project_id);

-- Bot execution log (timescaledb hypertable for efficient time-series)
CREATE TABLE IF NOT EXISTS bot_executions (
    id UUID DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL,
    strategy_id UUID NOT NULL,
    wallet_id UUID NOT NULL,
    execution_time TIMESTAMPTZ NOT NULL,
    action TEXT NOT NULL,
    token_in TEXT NOT NULL,
    token_out TEXT NOT NULL,
    amount_in NUMERIC(78, 0) NOT NULL,
    amount_out NUMERIC(78, 0) NOT NULL,
    price NUMERIC(38, 18),
    gas_used BIGINT,
    tx_hash BYTEA,
    status TEXT NOT NULL,  -- 'pending', 'success', 'failed', 'reverted'
    error_message TEXT,
    PRIMARY KEY (id, execution_time)
);

SELECT create_hypertable('bot_executions', 'execution_time', 
    chunk_time_interval => INTERVAL '1 day',
    if_not_exists => TRUE
);

CREATE INDEX idx_executions_project_time ON bot_executions(project_id, execution_time DESC);

-- =============================================================================
-- MARKET MAKER TABLES
-- =============================================================================

CREATE TABLE IF NOT EXISTS market_maker_configs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL,
    pool_address BYTEA NOT NULL,
    is_active BOOLEAN DEFAULT TRUE,
    
    -- Spread configuration
    target_spread_bps INTEGER NOT NULL DEFAULT 100,  -- 1% = 100 bps
    min_spread_bps INTEGER NOT NULL DEFAULT 25,
    max_spread_bps INTEGER NOT NULL DEFAULT 500,
    
    -- Volume configuration
    target_daily_volume_usd NUMERIC(38, 2),
    min_trade_size_usd NUMERIC(38, 2) DEFAULT 10,
    max_trade_size_usd NUMERIC(38, 2) DEFAULT 10000,
    
    -- Price stability
    price_deviation_tolerance_bps INTEGER DEFAULT 500,  -- 5%
    rebalance_threshold_bps INTEGER DEFAULT 200,
    
    -- Correlation settings
    follow_market_leader TEXT,  -- 'BTC', 'ETH', 'SP500', or NULL
    correlation_strength NUMERIC(3, 2) DEFAULT 0.50,
    
    -- Time settings
    operating_hours JSONB,  -- {"start": "00:00", "end": "24:00", "timezone": "UTC"}
    trade_frequency_seconds INTEGER DEFAULT 30,
    
    -- Risk limits
    max_inventory_imbalance_pct NUMERIC(5, 2) DEFAULT 30.0,
    daily_loss_limit_usd NUMERIC(38, 2),
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_mm_configs_project ON market_maker_configs(project_id);
CREATE INDEX idx_mm_configs_pool ON market_maker_configs(pool_address);

-- Market maker state
CREATE TABLE IF NOT EXISTS market_maker_state (
    config_id UUID PRIMARY KEY REFERENCES market_maker_configs(id),
    current_bid NUMERIC(38, 18),
    current_ask NUMERIC(38, 18),
    inventory_token0 NUMERIC(78, 0) DEFAULT 0,
    inventory_token1 NUMERIC(78, 0) DEFAULT 0,
    realized_pnl_usd NUMERIC(38, 2) DEFAULT 0,
    unrealized_pnl_usd NUMERIC(38, 2) DEFAULT 0,
    volume_today_usd NUMERIC(38, 2) DEFAULT 0,
    trades_today INTEGER DEFAULT 0,
    last_trade_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- =============================================================================
-- LISTING SUBMISSION TABLES
-- =============================================================================

CREATE TABLE IF NOT EXISTS listing_submissions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL,
    platform TEXT NOT NULL,  -- 'coinmarketcap', 'coingecko', 'defillama', etc.
    status TEXT NOT NULL DEFAULT 'draft',  -- 'draft', 'pending', 'submitted', 'approved', 'rejected', 'listed'
    
    -- Submission data (platform-specific)
    submission_data JSONB NOT NULL,
    
    -- Tracking
    submitted_at TIMESTAMPTZ,
    response_at TIMESTAMPTZ,
    response_data JSONB,
    listing_url TEXT,
    
    -- Automation
    auto_fill_enabled BOOLEAN DEFAULT TRUE,
    validation_errors JSONB,
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_listings_project ON listing_submissions(project_id);
CREATE INDEX idx_listings_platform ON listing_submissions(platform);
CREATE INDEX idx_listings_status ON listing_submissions(status);

-- Listing platform credentials (encrypted)
CREATE TABLE IF NOT EXISTS listing_credentials (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    identity_id UUID NOT NULL REFERENCES launchlab_identities(id),
    platform TEXT NOT NULL,
    encrypted_api_key BYTEA,
    encrypted_api_secret BYTEA,
    is_valid BOOLEAN DEFAULT TRUE,
    last_validated_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(identity_id, platform)
);

-- =============================================================================
-- CAMPAIGN TABLES
-- =============================================================================

CREATE TABLE IF NOT EXISTS campaigns (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    project_id UUID NOT NULL,
    name TEXT NOT NULL,
    type TEXT NOT NULL,  -- 'social', 'airdrop', 'bounty', 'otc', 'referral'
    status TEXT NOT NULL DEFAULT 'draft',
    
    -- Budget
    budget_token TEXT NOT NULL,
    budget_amount NUMERIC(78, 0) NOT NULL,
    spent_amount NUMERIC(78, 0) DEFAULT 0,
    
    -- Timing
    start_at TIMESTAMPTZ,
    end_at TIMESTAMPTZ,
    
    -- Configuration
    config JSONB NOT NULL,
    
    -- Metrics
    metrics JSONB DEFAULT '{}',
    
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_campaigns_project ON campaigns(project_id);
CREATE INDEX idx_campaigns_status ON campaigns(status);
CREATE INDEX idx_campaigns_type ON campaigns(type);

-- Social post queue
CREATE TABLE IF NOT EXISTS social_posts (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    campaign_id UUID NOT NULL REFERENCES campaigns(id),
    platform TEXT NOT NULL,  -- 'twitter', 'facebook', 'instagram', 'telegram'
    scheduled_at TIMESTAMPTZ,
    content TEXT NOT NULL,
    media_cids TEXT[],
    status TEXT NOT NULL DEFAULT 'draft',
    platform_post_id TEXT,
    engagement_metrics JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT NOW(),
    posted_at TIMESTAMPTZ
);

CREATE INDEX idx_posts_campaign ON social_posts(campaign_id);
CREATE INDEX idx_posts_scheduled ON social_posts(scheduled_at) WHERE status = 'scheduled';

-- Airdrop claims
CREATE TABLE IF NOT EXISTS airdrop_claims (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    campaign_id UUID NOT NULL REFERENCES campaigns(id),
    recipient_address BYTEA NOT NULL,
    amount NUMERIC(78, 0) NOT NULL,
    claim_proof BYTEA,  -- Merkle proof
    is_claimed BOOLEAN DEFAULT FALSE,
    claim_tx_hash BYTEA,
    claimed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_claims_campaign ON airdrop_claims(campaign_id);
CREATE INDEX idx_claims_recipient ON airdrop_claims(recipient_address);
CREATE INDEX idx_claims_unclaimed ON airdrop_claims(campaign_id, is_claimed) WHERE is_claimed = FALSE;

-- OTC deals
CREATE TABLE IF NOT EXISTS otc_deals (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v7(),
    campaign_id UUID NOT NULL REFERENCES campaigns(id),
    buyer_address BYTEA NOT NULL,
    seller_address BYTEA NOT NULL,
    token_address BYTEA NOT NULL,
    amount NUMERIC(78, 0) NOT NULL,
    price_per_token NUMERIC(38, 18) NOT NULL,
    payment_currency TEXT NOT NULL,  -- 'USDC', 'USDT', 'FAT', 'fiat'
    vesting_schedule JSONB,
    status TEXT NOT NULL DEFAULT 'proposed',
    escrow_contract BYTEA,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_otc_campaign ON otc_deals(campaign_id);
CREATE INDEX idx_otc_status ON otc_deals(status);
```

---

## 4. Module 1: Trading Bot Farm

### 4.1 Overview

The Trading Bot Farm extends the existing 9-strategy, 62-wallet system to support **500 wallets** with **15+ customizable strategies**, giving project owners fine-grained control over trading behavior simulation.

### 4.2 Wallet Management Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                          BOT WALLET HIERARCHY                                   │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  PROJECT OWNER WALLET                                                          │
│  └── LaunchLab Identity NFT                                                    │
│      └── Bot Master Key (HD Wallet Root)                                       │
│          │                                                                      │
│          ├── Strategy Group 0: Market Makers (50 wallets)                      │
│          │   ├── Wallet 0-9:   Tight Spread MM (±0.1%)                         │
│          │   ├── Wallet 10-19: Wide Spread MM (±0.5%)                          │
│          │   ├── Wallet 20-29: Adaptive Spread MM                              │
│          │   ├── Wallet 30-39: Cross-Pool Arbitrage                            │
│          │   └── Wallet 40-49: Reserve/Backup                                  │
│          │                                                                      │
│          ├── Strategy Group 1: Volume Generation (100 wallets)                 │
│          │   ├── Wallet 50-79:  Retail Simulation (varied sizes)               │
│          │   ├── Wallet 80-99:  Scalper (high frequency, small)                │
│          │   └── Wallet 100-149: Organic Growth (random patterns)              │
│          │                                                                      │
│          ├── Strategy Group 2: Price Support (50 wallets)                      │
│          │   ├── Wallet 150-169: DCA Accumulators                              │
│          │   ├── Wallet 170-189: Support Level Defenders                       │
│          │   └── Wallet 190-199: Dip Buyers                                    │
│          │                                                                      │
│          ├── Strategy Group 3: Liquidity Management (50 wallets)               │
│          │   ├── Wallet 200-219: LP Position Managers                          │
│          │   ├── Wallet 220-239: Rebalancers                                   │
│          │   └── Wallet 240-249: Fee Harvesters                                │
│          │                                                                      │
│          ├── Strategy Group 4: Special Operations (100 wallets)                │
│          │   ├── Wallet 250-279: Whale Simulation                              │
│          │   ├── Wallet 280-319: Momentum Traders                              │
│          │   ├── Wallet 320-349: Mean Reversion                                │
│          │   └── Wallet 350-399: Volatility Traders                            │
│          │                                                                      │
│          └── Strategy Group 5: Reserve Pool (100 wallets)                      │
│              └── Wallet 400-499: Unassigned/Custom                             │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 4.3 Strategy Definitions

```rust
/// Bot strategy types with full configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BotStrategy {
    /// Classic market making with configurable spread
    MarketMaker(MarketMakerParams),
    
    /// Arbitrage between different pools/pairs
    CrossPairArbitrage(ArbitrageParams),
    
    /// Stablecoin pair trading around peg
    StableTrader(StableTraderParams),
    
    /// Realistic retail trader simulation
    RetailSimulation(RetailSimParams),
    
    /// Large infrequent trades
    Whale(WhaleParams),
    
    /// High-frequency small trades
    Scalper(ScalperParams),
    
    /// Trend-following strategy
    Momentum(MomentumParams),
    
    /// Liquidity provision management
    LPManager(LPManagerParams),
    
    /// Dollar-cost averaging accumulation
    DCA(DCAParams),
    
    /// Pure volume generation for metrics
    VolumeGenerator(VolumeGenParams),
    
    /// Support specific price levels
    PriceSupport(PriceSupportParams),
    
    /// Natural-looking transaction patterns
    OrganicGrowth(OrganicGrowthParams),
    
    /// Counter wash trading signals (for legitimacy)
    WashTradingDefense(WashDefenseParams),
    
    /// Time-weighted execution (TWAP-style)
    TimeWeightedAccumulator(TWAPParams),
    
    /// Volatility exploitation
    VolatilityTrader(VolatilityParams),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketMakerParams {
    /// Target spread in basis points (100 = 1%)
    pub target_spread_bps: u16,
    
    /// Minimum spread (during high activity)
    pub min_spread_bps: u16,
    
    /// Maximum spread (during low activity)
    pub max_spread_bps: u16,
    
    /// Order size range in USD
    pub min_order_size_usd: f64,
    pub max_order_size_usd: f64,
    
    /// Maximum inventory imbalance (% of portfolio)
    pub max_inventory_imbalance_pct: f64,
    
    /// Rebalance when imbalance exceeds this threshold
    pub rebalance_threshold_pct: f64,
    
    /// Quote refresh interval in milliseconds
    pub quote_refresh_ms: u64,
    
    /// Skip quoting if price moved more than this in last interval
    pub volatility_pause_threshold_bps: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetailSimParams {
    /// Profile distribution
    pub profiles: Vec<RetailProfile>,
    
    /// Time-of-day activity weights (24 values, 0.0-1.0)
    pub hourly_activity_weights: [f64; 24],
    
    /// Day-of-week activity weights (7 values, 0.0-1.0)
    pub daily_activity_weights: [f64; 7],
    
    /// Chance to follow recent price movement
    pub fomo_probability: f64,
    
    /// Chance to panic sell on drops
    pub panic_sell_probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetailProfile {
    /// Profile name for identification
    pub name: String,
    
    /// Weight in selection (higher = more common)
    pub weight: f64,
    
    /// Trade frequency (trades per day on average)
    pub trades_per_day: f64,
    
    /// Trade size distribution
    pub min_trade_usd: f64,
    pub max_trade_usd: f64,
    pub mean_trade_usd: f64,
    
    /// Holding period preferences
    pub avg_hold_time_hours: f64,
    
    /// Preference for buy vs sell (0.5 = neutral)
    pub buy_bias: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleParams {
    /// Minimum trade size in USD
    pub min_trade_usd: f64,
    
    /// Maximum trade size in USD  
    pub max_trade_usd: f64,
    
    /// Average trades per day
    pub trades_per_day: f64,
    
    /// Split large orders into chunks?
    pub enable_iceberg: bool,
    
    /// Iceberg chunk size as % of total
    pub iceberg_chunk_pct: f64,
    
    /// Time between iceberg chunks (seconds)
    pub iceberg_interval_secs: u64,
    
    /// Slippage tolerance in basis points
    pub slippage_tolerance_bps: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumParams {
    /// Lookback period for trend detection (minutes)
    pub lookback_minutes: u32,
    
    /// Minimum price change to trigger entry (bps)
    pub entry_threshold_bps: u16,
    
    /// Take profit level (bps)
    pub take_profit_bps: u16,
    
    /// Stop loss level (bps)
    pub stop_loss_bps: u16,
    
    /// Maximum position hold time (minutes)
    pub max_hold_minutes: u32,
    
    /// Position size as % of available balance
    pub position_size_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DCAParams {
    /// Token to accumulate
    pub target_token: String,
    
    /// Investment per interval in USD
    pub amount_per_interval_usd: f64,
    
    /// Interval between purchases
    pub interval: DCAInterval,
    
    /// Randomize timing within interval (±%)
    pub timing_jitter_pct: f64,
    
    /// Increase purchase size on dips
    pub dip_buying_enabled: bool,
    
    /// Dip threshold to increase size (% drop from recent high)
    pub dip_threshold_pct: f64,
    
    /// Multiplier for dip purchases
    pub dip_size_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DCAInterval {
    Hourly,
    Every4Hours,
    Every8Hours,
    Daily,
    TwiceWeekly,
    Weekly,
    BiWeekly,
    Monthly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeGenParams {
    /// Target daily volume in USD
    pub target_daily_volume_usd: f64,
    
    /// Minimum trade size
    pub min_trade_usd: f64,
    
    /// Maximum trade size
    pub max_trade_usd: f64,
    
    /// Target number of trades per day
    pub target_trades_per_day: u32,
    
    /// Volume distribution throughout day
    pub volume_profile: VolumeProfile,
    
    /// Self-trading tolerance (0 = pure through-market)
    pub self_trade_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VolumeProfile {
    /// Constant throughout day
    Flat,
    /// Higher during market hours (9-5 UTC)
    MarketHours,
    /// Higher during US market hours
    USMarketHours,
    /// Higher during Asian market hours
    AsianMarketHours,
    /// Custom 24-hour weights
    Custom([f64; 24]),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganicGrowthParams {
    /// Target growth rate (% per day)
    pub daily_growth_rate_pct: f64,
    
    /// Number of unique "users" to simulate
    pub unique_user_count: u32,
    
    /// New user acquisition rate per day
    pub new_users_per_day: f64,
    
    /// User retention curve parameters
    pub retention_day1: f64,
    pub retention_day7: f64,
    pub retention_day30: f64,
    
    /// Transaction pattern randomization
    pub pattern_entropy: f64,  // 0.0 = predictable, 1.0 = random
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceSupportParams {
    /// Price levels to defend (as % below current price)
    pub support_levels: Vec<SupportLevel>,
    
    /// Total budget allocated for support
    pub total_budget_usd: f64,
    
    /// Emergency support trigger (% drop in 1 hour)
    pub emergency_trigger_pct: f64,
    
    /// Emergency support budget multiplier
    pub emergency_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportLevel {
    /// Distance below reference price (%)
    pub distance_pct: f64,
    
    /// Budget weight (relative to other levels)
    pub budget_weight: f64,
    
    /// Order size at this level
    pub order_size_usd: f64,
    
    /// Aggressiveness (0 = passive, 1 = aggressive)
    pub aggressiveness: f64,
}
```

### 4.4 Bot Orchestrator Engine

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use ethers::prelude::*;

/// Central orchestrator managing all bot strategies
pub struct BotOrchestrator {
    /// Project configuration
    config: Arc<RwLock<ProjectConfig>>,
    
    /// Wallet pool
    wallets: Arc<WalletPool>,
    
    /// Active strategy executors
    executors: Vec<Arc<dyn StrategyExecutor>>,
    
    /// Command channel
    command_rx: mpsc::Receiver<BotCommand>,
    
    /// Event broadcaster
    event_tx: broadcast::Sender<BotEvent>,
    
    /// Chain connection
    provider: Arc<Provider<Http>>,
    
    /// Router contract
    router: DCSwapRouter<Provider<Http>>,
    
    /// Execution state
    state: Arc<RwLock<OrchestratorState>>,
}

impl BotOrchestrator {
    pub async fn new(
        config: ProjectConfig,
        provider: Arc<Provider<Http>>,
        command_rx: mpsc::Receiver<BotCommand>,
    ) -> Result<Self, BotError> {
        let router_address: Address = config.router_address.parse()?;
        let router = DCSwapRouter::new(router_address, provider.clone());
        
        let wallets = WalletPool::new(
            &config.master_seed,
            config.wallet_count,
            provider.clone(),
        ).await?;
        
        let (event_tx, _) = broadcast::channel(10000);
        
        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            wallets: Arc::new(wallets),
            executors: Vec::new(),
            command_rx,
            event_tx,
            provider,
            router,
            state: Arc::new(RwLock::new(OrchestratorState::default())),
        })
    }
    
    /// Main orchestration loop
    pub async fn run(&mut self) -> Result<(), BotError> {
        info!("Bot orchestrator starting");
        
        // Initialize all configured strategies
        self.initialize_strategies().await?;
        
        // Start wallet funding check
        self.check_and_fund_wallets().await?;
        
        // Main event loop
        loop {
            tokio::select! {
                Some(cmd) = self.command_rx.recv() => {
                    self.handle_command(cmd).await?;
                }
                _ = self.tick() => {
                    // Regular tick processing
                }
            }
        }
    }
    
    async fn initialize_strategies(&mut self) -> Result<(), BotError> {
        let config = self.config.read().await;
        
        for strategy_config in &config.strategies {
            let executor: Arc<dyn StrategyExecutor> = match &strategy_config.strategy {
                BotStrategy::MarketMaker(params) => {
                    Arc::new(MarketMakerExecutor::new(
                        strategy_config.clone(),
                        params.clone(),
                        self.wallets.clone(),
                        self.provider.clone(),
                    ).await?)
                }
                BotStrategy::RetailSimulation(params) => {
                    Arc::new(RetailSimExecutor::new(
                        strategy_config.clone(),
                        params.clone(),
                        self.wallets.clone(),
                        self.provider.clone(),
                    ).await?)
                }
                BotStrategy::Whale(params) => {
                    Arc::new(WhaleExecutor::new(
                        strategy_config.clone(),
                        params.clone(),
                        self.wallets.clone(),
                        self.provider.clone(),
                    ).await?)
                }
                // ... other strategies
                _ => continue,
            };
            
            self.executors.push(executor);
        }
        
        Ok(())
    }
    
    async fn handle_command(&mut self, cmd: BotCommand) -> Result<(), BotError> {
        match cmd {
            BotCommand::Start { strategy_id } => {
                if let Some(executor) = self.find_executor(&strategy_id) {
                    executor.start().await?;
                }
            }
            BotCommand::Stop { strategy_id } => {
                if let Some(executor) = self.find_executor(&strategy_id) {
                    executor.stop().await?;
                }
            }
            BotCommand::UpdateConfig { strategy_id, config } => {
                if let Some(executor) = self.find_executor(&strategy_id) {
                    executor.update_config(config).await?;
                }
            }
            BotCommand::EmergencyStop => {
                for executor in &self.executors {
                    executor.emergency_stop().await?;
                }
            }
            BotCommand::Rebalance { strategy_id } => {
                if let Some(executor) = self.find_executor(&strategy_id) {
                    executor.rebalance().await?;
                }
            }
            BotCommand::FundWallets { amounts } => {
                self.fund_wallets(amounts).await?;
            }
            BotCommand::WithdrawAll { destination } => {
                self.withdraw_all(destination).await?;
            }
        }
        
        Ok(())
    }
    
    async fn tick(&self) -> Result<(), BotError> {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        interval.tick().await;
        
        // Update market data
        let market_data = self.fetch_market_data().await?;
        
        // Let each executor process
        for executor in &self.executors {
            if executor.is_active().await {
                if let Err(e) = executor.tick(&market_data).await {
                    error!("Strategy {} tick error: {}", executor.id(), e);
                    self.event_tx.send(BotEvent::Error {
                        strategy_id: executor.id().to_string(),
                        error: e.to_string(),
                    }).ok();
                }
            }
        }
        
        Ok(())
    }
}

/// Strategy executor trait
#[async_trait]
pub trait StrategyExecutor: Send + Sync {
    fn id(&self) -> &str;
    fn strategy_type(&self) -> &str;
    
    async fn start(&self) -> Result<(), BotError>;
    async fn stop(&self) -> Result<(), BotError>;
    async fn emergency_stop(&self) -> Result<(), BotError>;
    
    async fn is_active(&self) -> bool;
    async fn tick(&self, market_data: &MarketData) -> Result<(), BotError>;
    async fn rebalance(&self) -> Result<(), BotError>;
    async fn update_config(&self, config: serde_json::Value) -> Result<(), BotError>;
    
    async fn get_stats(&self) -> StrategyStats;
}
```

### 4.5 Bot Farm API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/botfarm/wallets` | GET | List all bot wallets with balances |
| `/v1/botfarm/wallets` | POST | Create new wallets (up to 500 total) |
| `/v1/botfarm/wallets/{id}/fund` | POST | Fund a specific wallet |
| `/v1/botfarm/wallets/bulk-fund` | POST | Fund multiple wallets |
| `/v1/botfarm/strategies` | GET | List all configured strategies |
| `/v1/botfarm/strategies` | POST | Create new strategy |
| `/v1/botfarm/strategies/{id}` | PUT | Update strategy configuration |
| `/v1/botfarm/strategies/{id}` | DELETE | Remove strategy |
| `/v1/botfarm/strategies/{id}/start` | POST | Start strategy execution |
| `/v1/botfarm/strategies/{id}/stop` | POST | Stop strategy execution |
| `/v1/botfarm/strategies/{id}/stats` | GET | Get strategy performance stats |
| `/v1/botfarm/executions` | GET | List execution history |
| `/v1/botfarm/stats` | GET | Overall bot farm statistics |
| `/v1/botfarm/emergency-stop` | POST | Emergency stop all strategies |

---

## 5. Module 2: Market Maker Service

### 5.1 Overview

The Market Maker Service provides project owners with institutional-grade market making capabilities without third-party contractors. It maintains spread, generates volume, and correlates asset behavior with broader market trends.

### 5.2 Core Market Making Engine

```rust
use std::collections::VecDeque;

/// Adaptive market maker with spread and inventory management
pub struct AdaptiveMarketMaker {
    config: MarketMakerConfig,
    state: MarketMakerState,
    price_history: VecDeque<PricePoint>,
    order_book_simulator: OrderBookSimulator,
    inventory_manager: InventoryManager,
    correlation_engine: CorrelationEngine,
}

impl AdaptiveMarketMaker {
    /// Calculate optimal bid/ask prices
    pub fn calculate_quotes(&self, market_data: &MarketData) -> (Decimal, Decimal) {
        let mid_price = market_data.mid_price;
        
        // Base spread from config
        let base_spread_bps = Decimal::from(self.config.target_spread_bps);
        
        // Adjust spread based on volatility
        let volatility_adjustment = self.calculate_volatility_adjustment(market_data);
        
        // Adjust spread based on inventory imbalance
        let inventory_adjustment = self.inventory_manager.calculate_spread_adjustment();
        
        // Adjust spread based on market correlation
        let correlation_adjustment = self.correlation_engine.calculate_spread_adjustment();
        
        // Final spread calculation
        let effective_spread_bps = (base_spread_bps 
            * volatility_adjustment 
            * inventory_adjustment 
            * correlation_adjustment)
            .max(Decimal::from(self.config.min_spread_bps))
            .min(Decimal::from(self.config.max_spread_bps));
        
        let half_spread = mid_price * effective_spread_bps / Decimal::from(10000) / Decimal::TWO;
        
        // Apply inventory skew (push price away from accumulated side)
        let inventory_skew = self.inventory_manager.calculate_price_skew(mid_price);
        
        let bid = mid_price - half_spread + inventory_skew;
        let ask = mid_price + half_spread + inventory_skew;
        
        (bid, ask)
    }
    
    /// Calculate optimal order size
    pub fn calculate_order_size(&self, side: Side, quote_price: Decimal) -> Decimal {
        let base_size = self.config.base_order_size_usd / quote_price;
        
        // Reduce size when inventory is imbalanced
        let inventory_factor = self.inventory_manager.calculate_size_factor(side);
        
        // Reduce size during high volatility
        let volatility_factor = self.calculate_volatility_size_factor();
        
        (base_size * inventory_factor * volatility_factor)
            .max(self.config.min_order_size_usd / quote_price)
            .min(self.config.max_order_size_usd / quote_price)
    }
    
    fn calculate_volatility_adjustment(&self, market_data: &MarketData) -> Decimal {
        // Higher volatility = wider spread
        let recent_volatility = self.calculate_recent_volatility();
        let baseline_volatility = self.calculate_baseline_volatility();
        
        if recent_volatility > baseline_volatility * Decimal::TWO {
            Decimal::from_str("1.5").unwrap() // Widen spread 50% during high vol
        } else if recent_volatility > baseline_volatility {
            Decimal::ONE + (recent_volatility - baseline_volatility) / baseline_volatility / Decimal::TWO
        } else {
            Decimal::ONE
        }
    }
}

/// Correlation engine for market-following behavior
pub struct CorrelationEngine {
    leader: MarketLeader,
    correlation_strength: Decimal,
    price_feed: Arc<dyn PriceFeed>,
    last_leader_price: Option<Decimal>,
    last_leader_update: Option<Instant>,
}

#[derive(Debug, Clone)]
pub enum MarketLeader {
    Bitcoin,
    Ethereum,
    SP500,
    Gold,
    Custom(String),
    None,
}

impl CorrelationEngine {
    /// Calculate price adjustment based on leader movement
    pub async fn calculate_correlation_adjustment(&mut self) -> Decimal {
        if matches!(self.leader, MarketLeader::None) {
            return Decimal::ONE;
        }
        
        let current_leader_price = self.fetch_leader_price().await;
        
        if let (Some(last_price), Some(current_price)) = 
            (self.last_leader_price, current_leader_price) 
        {
            let leader_change_pct = (current_price - last_price) / last_price;
            
            // Apply correlation strength to determine how much to follow
            let target_change = leader_change_pct * self.correlation_strength;
            
            // Update state
            self.last_leader_price = current_leader_price;
            self.last_leader_update = Some(Instant::now());
            
            // Return multiplier (1.0 + change)
            Decimal::ONE + target_change
        } else {
            self.last_leader_price = current_leader_price;
            self.last_leader_update = Some(Instant::now());
            Decimal::ONE
        }
    }
    
    async fn fetch_leader_price(&self) -> Option<Decimal> {
        match &self.leader {
            MarketLeader::Bitcoin => self.price_feed.get_price("BTC").await.ok(),
            MarketLeader::Ethereum => self.price_feed.get_price("ETH").await.ok(),
            MarketLeader::SP500 => self.price_feed.get_price("SPX").await.ok(),
            MarketLeader::Gold => self.price_feed.get_price("XAU").await.ok(),
            MarketLeader::Custom(symbol) => self.price_feed.get_price(symbol).await.ok(),
            MarketLeader::None => None,
        }
    }
}

/// Inventory management for MM
pub struct InventoryManager {
    /// Current token0 inventory
    inventory_token0: Decimal,
    
    /// Current token1 inventory
    inventory_token1: Decimal,
    
    /// Target inventory ratio (0.5 = balanced)
    target_ratio: Decimal,
    
    /// Maximum imbalance tolerance
    max_imbalance_pct: Decimal,
    
    /// Rebalance threshold
    rebalance_threshold_pct: Decimal,
}

impl InventoryManager {
    /// Calculate how much to skew price to rebalance inventory
    pub fn calculate_price_skew(&self, mid_price: Decimal) -> Decimal {
        let current_ratio = self.current_inventory_ratio();
        let imbalance = current_ratio - self.target_ratio;
        
        // If we have too much token0, lower ask to sell more
        // If we have too much token1, raise bid to buy more
        let skew_factor = imbalance * Decimal::from_str("0.001").unwrap(); // 0.1% per 1% imbalance
        
        mid_price * skew_factor
    }
    
    /// Calculate spread adjustment based on inventory
    pub fn calculate_spread_adjustment(&self) -> Decimal {
        let imbalance = (self.current_inventory_ratio() - self.target_ratio).abs();
        
        if imbalance > self.max_imbalance_pct {
            // Widen spread significantly when too imbalanced
            Decimal::from_str("2.0").unwrap()
        } else if imbalance > self.rebalance_threshold_pct {
            // Gradually widen spread
            Decimal::ONE + imbalance / self.max_imbalance_pct
        } else {
            Decimal::ONE
        }
    }
    
    /// Calculate size factor for orders
    pub fn calculate_size_factor(&self, side: Side) -> Decimal {
        let imbalance = self.current_inventory_ratio() - self.target_ratio;
        
        match side {
            Side::Buy if imbalance > Decimal::ZERO => {
                // We have too much token0, reduce buys
                (Decimal::ONE - imbalance * Decimal::TWO).max(Decimal::from_str("0.1").unwrap())
            }
            Side::Sell if imbalance < Decimal::ZERO => {
                // We have too little token0, reduce sells
                (Decimal::ONE + imbalance * Decimal::TWO).max(Decimal::from_str("0.1").unwrap())
            }
            _ => Decimal::ONE,
        }
    }
    
    fn current_inventory_ratio(&self) -> Decimal {
        let total_value = self.inventory_token0 + self.inventory_token1;
        if total_value.is_zero() {
            return self.target_ratio;
        }
        self.inventory_token0 / total_value
    }
}
```

### 5.3 Volume Generation with Market Consistency

```rust
/// Volume generator that maintains market consistency
pub struct ConsistentVolumeGenerator {
    config: VolumeGenConfig,
    correlation_engine: Arc<CorrelationEngine>,
    volume_tracker: VolumeTracker,
    pattern_analyzer: PatternAnalyzer,
}

impl ConsistentVolumeGenerator {
    /// Generate trades that look organic and follow market patterns
    pub async fn generate_volume(&mut self) -> Vec<TradeIntent> {
        let mut trades = Vec::new();
        
        // Get current market conditions
        let market_mood = self.analyze_market_mood().await;
        let time_factor = self.get_time_of_day_factor();
        let correlation_factor = self.correlation_engine.calculate_correlation_adjustment().await;
        
        // Determine volume for this period
        let target_volume = self.calculate_period_target(time_factor);
        let remaining_volume = target_volume - self.volume_tracker.period_volume();
        
        if remaining_volume <= Decimal::ZERO {
            return trades;
        }
        
        // Generate trades with varying sizes
        let trade_count = self.determine_trade_count(remaining_volume);
        let trade_sizes = self.generate_trade_size_distribution(remaining_volume, trade_count);
        
        for size in trade_sizes {
            let side = self.determine_trade_side(market_mood, correlation_factor);
            let timing_delay = self.generate_timing_delay();
            
            trades.push(TradeIntent {
                side,
                size_usd: size,
                delay_ms: timing_delay,
                priority: TradePriority::Normal,
                slippage_tolerance_bps: self.config.slippage_tolerance_bps,
            });
        }
        
        // Add some correlated trades if market is moving
        if market_mood.is_trending() {
            trades.extend(self.generate_correlated_trades(market_mood, correlation_factor));
        }
        
        trades
    }
    
    fn determine_trade_side(&self, mood: MarketMood, correlation: Decimal) -> Side {
        let base_probability = match mood {
            MarketMood::Bullish => 0.65,
            MarketMood::Bearish => 0.35,
            MarketMood::Neutral => 0.50,
        };
        
        // Adjust based on correlation engine
        let correlation_adjustment = (correlation - Decimal::ONE).to_f64().unwrap_or(0.0) * 0.2;
        let final_buy_probability = (base_probability + correlation_adjustment).clamp(0.2, 0.8);
        
        if rand::random::<f64>() < final_buy_probability {
            Side::Buy
        } else {
            Side::Sell
        }
    }
    
    fn generate_trade_size_distribution(
        &self, 
        total_volume: Decimal, 
        count: usize
    ) -> Vec<Decimal> {
        // Use log-normal distribution for realistic trade sizes
        let mean = (total_volume / Decimal::from(count)).to_f64().unwrap();
        let std_dev = mean * 0.5;
        
        let mut sizes: Vec<Decimal> = (0..count)
            .map(|_| {
                let log_normal = (rand::random::<f64>().ln() * std_dev + mean.ln()).exp();
                Decimal::from_f64(log_normal).unwrap()
                    .max(self.config.min_trade_usd)
                    .min(self.config.max_trade_usd)
            })
            .collect();
        
        // Normalize to hit target volume
        let sum: Decimal = sizes.iter().sum();
        if !sum.is_zero() {
            let factor = total_volume / sum;
            sizes = sizes.into_iter().map(|s| s * factor).collect();
        }
        
        sizes
    }
}
```

### 5.4 Market Maker API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/marketmaker/configs` | GET | List all MM configurations |
| `/v1/marketmaker/configs` | POST | Create new MM configuration |
| `/v1/marketmaker/configs/{id}` | GET | Get specific MM config |
| `/v1/marketmaker/configs/{id}` | PUT | Update MM configuration |
| `/v1/marketmaker/configs/{id}` | DELETE | Delete MM configuration |
| `/v1/marketmaker/configs/{id}/start` | POST | Start market making |
| `/v1/marketmaker/configs/{id}/stop` | POST | Stop market making |
| `/v1/marketmaker/configs/{id}/state` | GET | Current MM state |
| `/v1/marketmaker/configs/{id}/pnl` | GET | Profit/loss report |
| `/v1/marketmaker/correlation/leaders` | GET | Available correlation leaders |
| `/v1/marketmaker/quotes/{pool}` | GET | Current quotes for pool |

---

## 6. Module 3: Automated Listing Service

### 6.1 Overview

The Automated Listing Service digitizes and automates the submission process for major cryptocurrency data aggregators (CoinMarketCap, CoinGecko, DeFiLlama, etc.), eliminating the need for manual form filling and expensive listing services.

### 6.2 Supported Platforms

| Platform | API Type | Requirements | Automation Level |
|----------|----------|--------------|------------------|
| **CoinMarketCap** | Form + API | Contract address, socials, volume | Full |
| **CoinGecko** | Form + API | Contract address, liquidity proof | Full |
| **DeFiLlama** | GitHub PR | TVL adapter | Semi-auto |
| **DEXTools** | API | Contract, pair address | Full |
| **GeckoTerminal** | Auto-discovery | Verified contract | Automatic |
| **Dexscreener** | Auto-discovery | Pool activity | Automatic |

### 6.3 Listing Data Model

```typescript
/**
 * Universal listing submission data structure
 * Covers requirements for CMC, CoinGecko, and similar platforms
 */
interface UniversalListingData {
  // === BASIC INFO ===
  token: {
    name: string;                      // e.g., "Datachain FAT"
    symbol: string;                    // e.g., "FAT"
    description: string;               // 250-500 chars
    shortDescription: string;          // 50-100 chars
    category: TokenCategory;
    tags: string[];                    // ["defi", "layer1", "evm"]
  };
  
  // === CONTRACT INFO ===
  contracts: {
    primary: {
      chain: string;                   // "datachain-rope"
      chainId: number;                 // 271828
      address: string;                 // "0x..."
      decimals: number;
      explorerUrl: string;             // "https://dcscan.io/token/0x..."
    };
    bridges?: {
      chain: string;
      address: string;
      bridgeContract: string;
    }[];
  };
  
  // === SUPPLY INFO ===
  supply: {
    totalSupply: string;               // "10000000000"
    maxSupply?: string;                // null if unlimited
    circulatingSupply: string;
    circulatingSupplyCalculation: string;  // Explanation of calculation
    supplyApiEndpoint?: string;        // API for live supply
  };
  
  // === MARKET INFO ===
  market: {
    launchDate: string;                // ISO 8601
    launchPrice?: string;              // USD
    icoPrice?: string;
    icoDate?: string;
    exchangeListings: {
      exchange: string;
      pair: string;
      url: string;
      type: "cex" | "dex";
    }[];
  };
  
  // === LINKS ===
  links: {
    website: string;
    whitepaper?: string;
    github?: string;
    twitter?: string;
    telegram?: string;
    discord?: string;
    medium?: string;
    reddit?: string;
    facebook?: string;
    youtube?: string;
    linkedin?: string;
    announcement?: string;             // BitcoinTalk, etc.
    messageBoard?: string;
    explorer: string;
    sourceCode?: string;
    technicalDoc?: string;
    chat?: string[];
  };
  
  // === TEAM INFO ===
  team: {
    isAnonymous: boolean;
    members?: {
      name: string;
      role: string;
      linkedin?: string;
      twitter?: string;
      photo?: string;                  // IPFS CID or URL
    }[];
    company?: {
      name: string;
      country: string;
      registrationNumber?: string;
      address?: string;
    };
  };
  
  // === MEDIA ===
  media: {
    logo: {
      png256: string;                  // IPFS CID or URL (256x256 PNG)
      png64: string;                   // 64x64
      svg?: string;                    // Vector version
    };
    banner?: string;                   // 1200x630 for social
    screenshots?: string[];
    videos?: string[];
  };
  
  // === COMPLIANCE ===
  compliance: {
    auditReports?: {
      auditor: string;
      date: string;
      reportUrl: string;
    }[];
    securityRating?: string;
    kycVerified: boolean;
    legalOpinion?: string;             // URL to legal documentation
  };
  
  // === METRICS (auto-populated) ===
  metrics: {
    holders: number;
    transactions24h: number;
    volume24h: string;
    tvl?: string;
    marketCap?: string;
    fullyDilutedValuation?: string;
  };
}

type TokenCategory = 
  | "currency"
  | "platform"
  | "defi"
  | "nft"
  | "gaming"
  | "metaverse"
  | "infrastructure"
  | "data"
  | "storage"
  | "identity"
  | "oracle"
  | "privacy"
  | "scaling"
  | "interoperability"
  | "governance"
  | "stablecoin"
  | "wrapped"
  | "meme"
  | "fan-token"
  | "real-world-asset"
  | "security-token";
```

### 6.4 Platform-Specific Adapters

```typescript
/**
 * CoinMarketCap listing adapter
 */
class CoinMarketCapAdapter implements ListingAdapter {
  private readonly CMC_FORM_URL = "https://coinmarketcap.com/token-request/";
  private readonly CMC_API_BASE = "https://api.coinmarketcap.com";
  
  /**
   * Transform universal data to CMC format
   */
  async prepare(data: UniversalListingData): Promise<CMCSubmissionData> {
    // Validate all required fields
    this.validateRequiredFields(data);
    
    return {
      // Project Info
      project_name: data.token.name,
      project_ticker: data.token.symbol,
      project_description: data.token.description,
      project_short_description: data.token.shortDescription,
      
      // Contract Info
      contract_platform: this.mapChainToCMC(data.contracts.primary.chain),
      contract_address: data.contracts.primary.address,
      decimal_points: data.contracts.primary.decimals,
      
      // Supply
      total_supply: data.supply.totalSupply,
      max_supply: data.supply.maxSupply,
      circulating_supply: data.supply.circulatingSupply,
      supply_endpoint: data.supply.supplyApiEndpoint,
      
      // Market
      launch_date: data.market.launchDate,
      
      // Links
      website: data.links.website,
      whitepaper: data.links.whitepaper,
      explorer: data.links.explorer,
      source_code: data.links.sourceCode,
      technical_doc: data.links.technicalDoc,
      twitter: data.links.twitter,
      telegram: data.links.telegram,
      discord: data.links.discord,
      reddit: data.links.reddit,
      medium: data.links.medium,
      
      // Media
      logo_256: data.media.logo.png256,
      logo_64: data.media.logo.png64,
      
      // Team
      is_team_anonymous: data.team.isAnonymous,
      team_members: data.team.members?.map(m => ({
        name: m.name,
        title: m.role,
        linkedin: m.linkedin,
      })),
      
      // Exchanges (need at least 2 for CMC)
      exchanges: data.market.exchangeListings.map(e => ({
        exchange_name: e.exchange,
        trading_pair: e.pair,
        trading_url: e.url,
      })),
      
      // Audit
      audit_links: data.compliance.auditReports?.map(a => a.reportUrl),
    };
  }
  
  /**
   * Submit to CoinMarketCap
   */
  async submit(data: CMCSubmissionData): Promise<SubmissionResult> {
    // CMC uses a multi-step form process
    const session = await this.createSession();
    
    try {
      // Step 1: Basic Info
      await this.submitStep(session, 'basic', {
        project_name: data.project_name,
        project_ticker: data.project_ticker,
        contract_platform: data.contract_platform,
        contract_address: data.contract_address,
      });
      
      // Step 2: Supply Info
      await this.submitStep(session, 'supply', {
        total_supply: data.total_supply,
        circulating_supply: data.circulating_supply,
        supply_endpoint: data.supply_endpoint,
      });
      
      // Step 3: Links & Socials
      await this.submitStep(session, 'links', {
        website: data.website,
        explorer: data.explorer,
        twitter: data.twitter,
        telegram: data.telegram,
      });
      
      // Step 4: Media
      await this.submitStep(session, 'media', {
        logo_256: data.logo_256,
      });
      
      // Step 5: Team (if not anonymous)
      if (!data.is_team_anonymous) {
        await this.submitStep(session, 'team', {
          team_members: data.team_members,
        });
      }
      
      // Step 6: Exchanges
      await this.submitStep(session, 'exchanges', {
        exchanges: data.exchanges,
      });
      
      // Final submission
      const result = await this.finalizeSubmission(session);
      
      return {
        platform: 'coinmarketcap',
        status: 'submitted',
        submissionId: result.submission_id,
        submittedAt: new Date().toISOString(),
        estimatedReviewTime: '7-14 days',
        trackingUrl: `https://coinmarketcap.com/request-status/${result.submission_id}`,
      };
    } catch (error) {
      return {
        platform: 'coinmarketcap',
        status: 'failed',
        error: error.message,
        validationErrors: this.parseValidationErrors(error),
      };
    }
  }
  
  private mapChainToCMC(chain: string): string {
    const mapping: Record<string, string> = {
      'datachain-rope': 'Datachain Rope',
      'ethereum': 'Ethereum',
      'bsc': 'BNB Smart Chain (BEP20)',
      'polygon': 'Polygon',
      'arbitrum': 'Arbitrum',
      // Add more chains
    };
    return mapping[chain] || chain;
  }
}

/**
 * CoinGecko listing adapter
 */
class CoinGeckoAdapter implements ListingAdapter {
  private readonly COINGECKO_FORM_URL = "https://www.coingecko.com/en/coins/request";
  
  async prepare(data: UniversalListingData): Promise<CoinGeckoSubmissionData> {
    // CoinGecko specific requirements
    this.validateCoinGeckoRequirements(data);
    
    return {
      name: data.token.name,
      symbol: data.token.symbol,
      
      // Contract (CoinGecko requires specific format)
      asset_platform_id: this.mapChainToGecko(data.contracts.primary.chain),
      contract_address: data.contracts.primary.address.toLowerCase(),
      
      // Description
      description: {
        en: data.token.description,
      },
      
      // Links
      links: {
        homepage: [data.links.website],
        blockchain_site: [data.links.explorer],
        official_forum_url: data.links.announcement ? [data.links.announcement] : [],
        chat_url: data.links.discord ? [data.links.discord] : [],
        announcement_url: data.links.medium ? [data.links.medium] : [],
        twitter_screen_name: this.extractTwitterHandle(data.links.twitter),
        telegram_channel_identifier: this.extractTelegramHandle(data.links.telegram),
        subreddit_url: data.links.reddit,
        repos_url: {
          github: data.links.github ? [data.links.github] : [],
        },
      },
      
      // Image
      image: {
        thumb: data.media.logo.png64,
        small: data.media.logo.png256,
        large: data.media.logo.png256,
      },
      
      // Categories
      categories: this.mapCategories(data.token.category, data.token.tags),
    };
  }
  
  async submit(data: CoinGeckoSubmissionData): Promise<SubmissionResult> {
    // CoinGecko submission logic
    // ...
  }
  
  private validateCoinGeckoRequirements(data: UniversalListingData): void {
    // CoinGecko specific validations
    if (!data.links.website) {
      throw new ValidationError('CoinGecko requires a website');
    }
    if (!data.links.explorer) {
      throw new ValidationError('CoinGecko requires an explorer link');
    }
    // At least 1 CEX or significant DEX volume
    const hasQualifyingExchange = data.market.exchangeListings.some(e => 
      e.type === 'cex' || this.isSignificantDex(e.exchange)
    );
    if (!hasQualifyingExchange) {
      throw new ValidationError('CoinGecko requires listing on a qualifying exchange');
    }
  }
}
```

### 6.5 Auto-Fill Engine

```typescript
/**
 * Auto-fill engine that populates listing data from on-chain and API sources
 */
class ListingAutoFillEngine {
  constructor(
    private readonly provider: ethers.Provider,
    private readonly indexerApi: IndexerApiClient,
    private readonly ipfsClient: IPFSClient,
    private readonly socialVerifier: SocialVerifier,
  ) {}
  
  /**
   * Auto-populate listing data from project configuration
   */
  async autoFill(projectId: string): Promise<Partial<UniversalListingData>> {
    const project = await this.fetchProjectData(projectId);
    const onChainData = await this.fetchOnChainData(project.asset.contractAddress);
    const metrics = await this.fetchMetrics(project.asset.contractAddress);
    const socialProfiles = await this.verifySocialLinks(project);
    
    return {
      token: {
        name: onChainData.name,
        symbol: onChainData.symbol,
        description: project.description,
        shortDescription: this.generateShortDescription(project.description),
        category: this.inferCategory(project),
        tags: this.inferTags(project),
      },
      contracts: {
        primary: {
          chain: 'datachain-rope',
          chainId: 271828,
          address: project.asset.contractAddress,
          decimals: onChainData.decimals,
          explorerUrl: `https://dcscan.io/token/${project.asset.contractAddress}`,
        },
      },
      supply: {
        totalSupply: onChainData.totalSupply.toString(),
        maxSupply: onChainData.maxSupply?.toString(),
        circulatingSupply: await this.calculateCirculatingSupply(project),
        circulatingSupplyCalculation: this.generateSupplyCalculation(project),
        supplyApiEndpoint: `https://dcswap.net/v1/tokens/${project.asset.contractAddress}/supply`,
      },
      market: {
        launchDate: project.createdAt,
        exchangeListings: await this.fetchExchangeListings(project),
      },
      links: {
        website: project.website || socialProfiles.website,
        explorer: `https://dcscan.io/token/${project.asset.contractAddress}`,
        twitter: socialProfiles.twitter,
        telegram: socialProfiles.telegram,
        discord: socialProfiles.discord,
      },
      media: {
        logo: await this.processLogoImages(project.logoCID),
      },
      metrics: {
        holders: metrics.holderCount,
        transactions24h: metrics.tx24h,
        volume24h: metrics.volume24h,
        tvl: metrics.tvl,
      },
    };
  }
  
  private async fetchOnChainData(address: string) {
    const contract = new ethers.Contract(address, ERC20_ABI, this.provider);
    
    const [name, symbol, decimals, totalSupply] = await Promise.all([
      contract.name(),
      contract.symbol(),
      contract.decimals(),
      contract.totalSupply(),
    ]);
    
    // Try to get max supply if available
    let maxSupply = null;
    try {
      maxSupply = await contract.maxSupply?.();
    } catch {}
    
    return { name, symbol, decimals, totalSupply, maxSupply };
  }
  
  private async calculateCirculatingSupply(project: LaunchLabProject): Promise<string> {
    // Fetch total supply
    const contract = new ethers.Contract(
      project.asset.contractAddress, 
      ERC20_ABI, 
      this.provider
    );
    const totalSupply = await contract.totalSupply();
    
    // Subtract known locked/burned addresses
    const excludedBalances = await this.fetchExcludedBalances(
      project.asset.contractAddress
    );
    
    const circulating = totalSupply.sub(excludedBalances);
    return circulating.toString();
  }
  
  private async processLogoImages(logoCID: string): Promise<{
    png256: string;
    png64: string;
    svg?: string;
  }> {
    // Fetch original from IPFS
    const originalBuffer = await this.ipfsClient.cat(logoCID);
    
    // Process and resize
    const png256 = await sharp(originalBuffer)
      .resize(256, 256, { fit: 'contain', background: { r: 0, g: 0, b: 0, alpha: 0 } })
      .png()
      .toBuffer();
    
    const png64 = await sharp(originalBuffer)
      .resize(64, 64, { fit: 'contain', background: { r: 0, g: 0, b: 0, alpha: 0 } })
      .png()
      .toBuffer();
    
    // Upload processed versions to IPFS
    const [cid256, cid64] = await Promise.all([
      this.ipfsClient.add(png256),
      this.ipfsClient.add(png64),
    ]);
    
    return {
      png256: `https://ipfs.datachain.network/ipfs/${cid256}`,
      png64: `https://ipfs.datachain.network/ipfs/${cid64}`,
    };
  }
}
```

### 6.6 Listing API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/listings/platforms` | GET | List supported platforms and requirements |
| `/v1/listings/prepare` | POST | Prepare listing data (auto-fill) |
| `/v1/listings/validate` | POST | Validate listing data for platform |
| `/v1/listings/submit` | POST | Submit listing to platform |
| `/v1/listings/{id}` | GET | Get submission status |
| `/v1/listings/{id}/retry` | POST | Retry failed submission |
| `/v1/listings/credentials` | POST | Store platform API credentials |
| `/v1/listings/credentials/{platform}` | DELETE | Remove platform credentials |

---

## 7. Module 4: Marketing Campaign Manager

### 7.1 Overview

The Marketing Campaign Manager provides Shopify-like tools for project owners to create and manage multi-channel marketing campaigns including social media, airdrops, bounties, OTC deals, and referral programs.

### 7.2 Campaign Types

```typescript
/**
 * Campaign type definitions
 */
enum CampaignType {
  // Social media campaigns
  SOCIAL_TWITTER = 'social_twitter',
  SOCIAL_FACEBOOK = 'social_facebook',
  SOCIAL_TELEGRAM = 'social_telegram',
  SOCIAL_DISCORD = 'social_discord',
  
  // Distribution campaigns
  AIRDROP = 'airdrop',
  BOUNTY = 'bounty',
  REFERRAL = 'referral',
  
  // Trading campaigns
  OTC = 'otc',
  LIQUIDITY_MINING = 'liquidity_mining',
  TRADING_COMPETITION = 'trading_competition',
  
  // Content campaigns
  CONTENT_CREATOR = 'content_creator',
  AMBASSADOR = 'ambassador',
}

/**
 * Base campaign configuration
 */
interface BaseCampaignConfig {
  id: string;
  projectId: string;
  type: CampaignType;
  name: string;
  description: string;
  status: 'draft' | 'scheduled' | 'active' | 'paused' | 'completed' | 'cancelled';
  
  // Budget
  budget: {
    tokenAddress: string;
    tokenSymbol: string;
    totalAmount: bigint;
    spentAmount: bigint;
    reservedAmount: bigint;
  };
  
  // Schedule
  schedule: {
    startAt: Date;
    endAt: Date;
    timezone: string;
  };
  
  // Targeting
  targeting?: {
    geoTargets?: string[];          // ISO country codes
    minHolderBalance?: bigint;
    excludeAddresses?: string[];
    requireKyc?: boolean;
  };
  
  // Metrics
  metrics: CampaignMetrics;
  
  createdAt: Date;
  updatedAt: Date;
}

interface CampaignMetrics {
  impressions: number;
  clicks: number;
  conversions: number;
  participants: number;
  tokensDistributed: bigint;
  estimatedReach: number;
  engagementRate: number;
  costPerAcquisition: number;
}
```

### 7.3 Social Media Integration

```typescript
/**
 * Social media campaign manager
 */
class SocialCampaignManager {
  constructor(
    private readonly twitterClient: TwitterApiClient,
    private readonly metaClient: MetaGraphApiClient,
    private readonly telegramClient: TelegramBotClient,
    private readonly scheduler: CampaignScheduler,
    private readonly analytics: AnalyticsService,
  ) {}
  
  /**
   * Create a multi-platform social campaign
   */
  async createCampaign(config: SocialCampaignConfig): Promise<SocialCampaign> {
    // Validate all platform credentials
    await this.validatePlatformCredentials(config.platforms);
    
    // Create campaign record
    const campaign = await this.db.campaigns.create({
      ...config,
      status: 'draft',
    });
    
    // Generate content calendar
    const contentCalendar = await this.generateContentCalendar(config);
    
    // Schedule posts
    for (const post of contentCalendar) {
      await this.scheduler.schedulePost(campaign.id, post);
    }
    
    return campaign;
  }
  
  /**
   * Generate optimized content calendar
   */
  private async generateContentCalendar(
    config: SocialCampaignConfig
  ): Promise<ScheduledPost[]> {
    const posts: ScheduledPost[] = [];
    const duration = config.schedule.endAt.getTime() - config.schedule.startAt.getTime();
    const postCount = config.postFrequency * (duration / (24 * 60 * 60 * 1000));
    
    for (let i = 0; i < postCount; i++) {
      // Determine optimal posting time
      const postTime = this.calculateOptimalPostTime(
        config.schedule.startAt,
        config.schedule.endAt,
        config.platforms,
        i / postCount,
      );
      
      // Generate content for each platform
      for (const platform of config.platforms) {
        posts.push({
          campaignId: '', // Set after campaign creation
          platform,
          scheduledAt: postTime,
          content: await this.generatePlatformContent(platform, config, i),
          mediaUrls: config.mediaAssets?.[i % config.mediaAssets.length],
          status: 'scheduled',
        });
      }
    }
    
    return posts;
  }
  
  /**
   * Calculate optimal posting time based on platform analytics
   */
  private calculateOptimalPostTime(
    startDate: Date,
    endDate: Date,
    platforms: Platform[],
    progressRatio: number,
  ): Date {
    // Platform-specific optimal hours (UTC)
    const optimalHours: Record<Platform, number[]> = {
      twitter: [13, 14, 15, 16, 17],      // 1-5 PM UTC
      facebook: [12, 13, 14, 15],          // 12-3 PM UTC
      telegram: [9, 10, 11, 14, 15, 16],   // Morning + afternoon
      discord: [18, 19, 20, 21],           // Evening UTC
    };
    
    // Weight by platform presence
    const avgOptimalHour = platforms.reduce((sum, p) => {
      const hours = optimalHours[p];
      return sum + hours[Math.floor(Math.random() * hours.length)];
    }, 0) / platforms.length;
    
    // Calculate date based on progress
    const targetTimestamp = startDate.getTime() + 
      (endDate.getTime() - startDate.getTime()) * progressRatio;
    
    const targetDate = new Date(targetTimestamp);
    targetDate.setUTCHours(Math.round(avgOptimalHour), Math.floor(Math.random() * 60));
    
    return targetDate;
  }
  
  /**
   * Post to Twitter/X
   */
  async postToTwitter(post: ScheduledPost): Promise<TwitterPostResult> {
    const mediaIds: string[] = [];
    
    // Upload media if present
    if (post.mediaUrls?.length) {
      for (const url of post.mediaUrls) {
        const mediaBuffer = await this.fetchMedia(url);
        const mediaId = await this.twitterClient.v1.uploadMedia(mediaBuffer, {
          mimeType: this.getMimeType(url),
        });
        mediaIds.push(mediaId);
      }
    }
    
    // Create tweet
    const tweet = await this.twitterClient.v2.tweet({
      text: post.content,
      media: mediaIds.length ? { media_ids: mediaIds } : undefined,
    });
    
    return {
      platformPostId: tweet.data.id,
      postedAt: new Date(),
      url: `https://twitter.com/i/status/${tweet.data.id}`,
    };
  }
  
  /**
   * Post to Facebook/Instagram
   */
  async postToMeta(post: ScheduledPost, pageId: string): Promise<MetaPostResult> {
    // Upload media to Facebook
    let mediaId: string | undefined;
    if (post.mediaUrls?.[0]) {
      const mediaResponse = await this.metaClient.post(`/${pageId}/photos`, {
        url: post.mediaUrls[0],
        published: false,
      });
      mediaId = mediaResponse.id;
    }
    
    // Create post
    const postResponse = await this.metaClient.post(`/${pageId}/feed`, {
      message: post.content,
      attached_media: mediaId ? [{ media_fbid: mediaId }] : undefined,
    });
    
    return {
      platformPostId: postResponse.id,
      postedAt: new Date(),
      url: `https://facebook.com/${postResponse.id}`,
    };
  }
}

/**
 * Twitter campaign configuration
 */
interface TwitterCampaignConfig extends SocialCampaignConfig {
  // Tweet settings
  includeHashtags: boolean;
  hashtagList: string[];
  includeMentions: string[];
  
  // Thread settings
  enableThreads: boolean;
  maxThreadLength: number;
  
  // Engagement automation
  autoReply: boolean;
  autoReplyTemplate?: string;
  
  // Analytics tracking
  trackLinks: boolean;
  utmSource: string;
  utmMedium: string;
  utmCampaign: string;
}
```

### 7.4 Airdrop Campaign System

```typescript
/**
 * Merkle-based airdrop distribution system
 */
class AirdropCampaignManager {
  constructor(
    private readonly provider: ethers.Provider,
    private readonly signer: ethers.Signer,
    private readonly ipfsClient: IPFSClient,
  ) {}
  
  /**
   * Create airdrop campaign with Merkle tree
   */
  async createAirdrop(config: AirdropConfig): Promise<AirdropCampaign> {
    // Validate budget
    await this.validateBudget(config);
    
    // Build recipient list
    const recipients = await this.buildRecipientList(config);
    
    // Build Merkle tree
    const { root, tree, claims } = this.buildMerkleTree(recipients);
    
    // Upload proof data to IPFS
    const proofsCid = await this.uploadProofs(claims);
    
    // Deploy airdrop contract
    const contractAddress = await this.deployAirdropContract({
      token: config.tokenAddress,
      merkleRoot: root,
      startTime: config.schedule.startAt,
      endTime: config.schedule.endAt,
      totalAmount: config.budget.totalAmount,
    });
    
    // Fund the contract
    await this.fundContract(contractAddress, config.tokenAddress, config.budget.totalAmount);
    
    return {
      id: crypto.randomUUID(),
      config,
      contractAddress,
      merkleRoot: root,
      proofsCid,
      recipients: recipients.length,
      totalAmount: config.budget.totalAmount,
      status: 'active',
    };
  }
  
  /**
   * Build recipient list based on eligibility criteria
   */
  private async buildRecipientList(config: AirdropConfig): Promise<AirdropRecipient[]> {
    const recipients: AirdropRecipient[] = [];
    
    switch (config.eligibility.type) {
      case 'holders':
        // Get all token holders
        const holders = await this.fetchTokenHolders(config.eligibility.tokenAddress);
        for (const holder of holders) {
          if (this.meetsEligibility(holder, config.eligibility)) {
            recipients.push({
              address: holder.address,
              amount: this.calculateAllocation(holder, config.distribution),
            });
          }
        }
        break;
        
      case 'whitelist':
        // Use provided whitelist
        for (const entry of config.eligibility.whitelist!) {
          recipients.push({
            address: entry.address,
            amount: entry.amount || config.distribution.baseAmount,
          });
        }
        break;
        
      case 'activity':
        // Based on trading/interaction activity
        const activeUsers = await this.fetchActiveUsers(config.eligibility.criteria);
        for (const user of activeUsers) {
          recipients.push({
            address: user.address,
            amount: this.calculateActivityBonus(user, config.distribution),
          });
        }
        break;
    }
    
    // Apply caps and filters
    return this.applyDistributionRules(recipients, config.distribution);
  }
  
  /**
   * Build Merkle tree for airdrop claims
   */
  private buildMerkleTree(recipients: AirdropRecipient[]): {
    root: string;
    tree: MerkleTree;
    claims: MerkleClaim[];
  } {
    // Create leaf nodes
    const leaves = recipients.map(r => 
      ethers.solidityPackedKeccak256(
        ['address', 'uint256'],
        [r.address, r.amount]
      )
    );
    
    // Build tree
    const tree = new MerkleTree(leaves, keccak256, { sortPairs: true });
    const root = tree.getHexRoot();
    
    // Generate proofs for each recipient
    const claims = recipients.map((r, i) => ({
      address: r.address,
      amount: r.amount.toString(),
      proof: tree.getHexProof(leaves[i]),
    }));
    
    return { root, tree, claims };
  }
}

/**
 * Airdrop smart contract (Solidity)
 */
const AIRDROP_CONTRACT = `
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/cryptography/MerkleProof.sol";
import "@openzeppelin/contracts/access/Ownable.sol";

contract LaunchLabAirdrop is Ownable {
    using SafeERC20 for IERC20;
    
    IERC20 public immutable token;
    bytes32 public immutable merkleRoot;
    uint256 public immutable startTime;
    uint256 public immutable endTime;
    
    mapping(address => bool) public claimed;
    uint256 public totalClaimed;
    
    event Claimed(address indexed account, uint256 amount);
    event Recovered(address token, uint256 amount);
    
    constructor(
        address _token,
        bytes32 _merkleRoot,
        uint256 _startTime,
        uint256 _endTime
    ) Ownable(msg.sender) {
        token = IERC20(_token);
        merkleRoot = _merkleRoot;
        startTime = _startTime;
        endTime = _endTime;
    }
    
    function claim(uint256 amount, bytes32[] calldata proof) external {
        require(block.timestamp >= startTime, "Airdrop not started");
        require(block.timestamp <= endTime, "Airdrop ended");
        require(!claimed[msg.sender], "Already claimed");
        
        bytes32 leaf = keccak256(abi.encodePacked(msg.sender, amount));
        require(MerkleProof.verify(proof, merkleRoot, leaf), "Invalid proof");
        
        claimed[msg.sender] = true;
        totalClaimed += amount;
        
        token.safeTransfer(msg.sender, amount);
        emit Claimed(msg.sender, amount);
    }
    
    function recoverTokens(address _token) external onlyOwner {
        require(block.timestamp > endTime, "Airdrop not ended");
        
        uint256 balance = IERC20(_token).balanceOf(address(this));
        IERC20(_token).safeTransfer(owner(), balance);
        emit Recovered(_token, balance);
    }
}
`;
```

### 7.5 OTC Deal Manager

```typescript
/**
 * OTC (Over-The-Counter) deal management
 */
class OTCDealManager {
  constructor(
    private readonly escrowFactory: EscrowFactory,
    private readonly kycVerifier: KYCVerifier,
    private readonly priceFeed: PriceFeed,
  ) {}
  
  /**
   * Create OTC deal with escrow
   */
  async createDeal(config: OTCDealConfig): Promise<OTCDeal> {
    // Verify KYC for both parties if required
    if (config.requireKyc) {
      await this.verifyPartyKyc(config.seller);
      await this.verifyPartyKyc(config.buyer);
    }
    
    // Calculate deal value
    const dealValue = this.calculateDealValue(config);
    
    // Deploy escrow contract
    const escrow = await this.escrowFactory.deploy({
      seller: config.seller,
      buyer: config.buyer,
      token: config.tokenAddress,
      amount: config.amount,
      price: config.pricePerToken,
      paymentCurrency: config.paymentCurrency,
      vestingSchedule: config.vestingSchedule,
      deadline: config.deadline,
    });
    
    return {
      id: crypto.randomUUID(),
      config,
      escrowAddress: escrow.address,
      status: 'pending_deposit',
      dealValue,
      createdAt: new Date(),
    };
  }
  
  /**
   * Calculate vesting release schedule
   */
  private buildVestingSchedule(config: VestingConfig): VestingRelease[] {
    const releases: VestingRelease[] = [];
    const totalAmount = config.totalAmount;
    
    // Initial unlock (TGE)
    if (config.tgeUnlockPercent > 0) {
      releases.push({
        date: config.startDate,
        amount: totalAmount * BigInt(config.tgeUnlockPercent) / 100n,
        type: 'tge',
      });
    }
    
    // Cliff period
    let cliffEnd = config.startDate;
    if (config.cliffMonths > 0) {
      cliffEnd = new Date(config.startDate);
      cliffEnd.setMonth(cliffEnd.getMonth() + config.cliffMonths);
      
      releases.push({
        date: cliffEnd,
        amount: totalAmount * BigInt(config.cliffUnlockPercent || 0) / 100n,
        type: 'cliff',
      });
    }
    
    // Linear vesting
    const remainingAmount = totalAmount - releases.reduce((s, r) => s + r.amount, 0n);
    const vestingPeriods = config.vestingMonths;
    const amountPerPeriod = remainingAmount / BigInt(vestingPeriods);
    
    for (let i = 1; i <= vestingPeriods; i++) {
      const releaseDate = new Date(cliffEnd);
      releaseDate.setMonth(releaseDate.getMonth() + i);
      
      releases.push({
        date: releaseDate,
        amount: amountPerPeriod,
        type: 'vesting',
      });
    }
    
    return releases;
  }
}

/**
 * Vesting escrow contract
 */
const VESTING_ESCROW_CONTRACT = `
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";

contract VestingEscrow is ReentrancyGuard {
    using SafeERC20 for IERC20;
    
    struct VestingSchedule {
        uint256 totalAmount;
        uint256 releasedAmount;
        uint256 startTime;
        uint256 cliffDuration;
        uint256 vestingDuration;
        uint256 tgePercent;
    }
    
    IERC20 public immutable token;
    address public immutable beneficiary;
    VestingSchedule public schedule;
    
    event Released(uint256 amount);
    
    constructor(
        address _token,
        address _beneficiary,
        uint256 _totalAmount,
        uint256 _startTime,
        uint256 _cliffDuration,
        uint256 _vestingDuration,
        uint256 _tgePercent
    ) {
        token = IERC20(_token);
        beneficiary = _beneficiary;
        schedule = VestingSchedule({
            totalAmount: _totalAmount,
            releasedAmount: 0,
            startTime: _startTime,
            cliffDuration: _cliffDuration,
            vestingDuration: _vestingDuration,
            tgePercent: _tgePercent
        });
    }
    
    function release() external nonReentrant {
        uint256 releasable = getReleasableAmount();
        require(releasable > 0, "Nothing to release");
        
        schedule.releasedAmount += releasable;
        token.safeTransfer(beneficiary, releasable);
        
        emit Released(releasable);
    }
    
    function getReleasableAmount() public view returns (uint256) {
        return getVestedAmount() - schedule.releasedAmount;
    }
    
    function getVestedAmount() public view returns (uint256) {
        if (block.timestamp < schedule.startTime) {
            return 0;
        }
        
        // TGE unlock
        uint256 tgeAmount = (schedule.totalAmount * schedule.tgePercent) / 100;
        
        if (block.timestamp < schedule.startTime + schedule.cliffDuration) {
            return tgeAmount;
        }
        
        uint256 vestingAmount = schedule.totalAmount - tgeAmount;
        uint256 timeFromCliff = block.timestamp - schedule.startTime - schedule.cliffDuration;
        
        if (timeFromCliff >= schedule.vestingDuration) {
            return schedule.totalAmount;
        }
        
        return tgeAmount + (vestingAmount * timeFromCliff) / schedule.vestingDuration;
    }
}
`;
```

### 7.6 Campaign API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/campaigns` | GET | List all campaigns |
| `/v1/campaigns` | POST | Create new campaign |
| `/v1/campaigns/{id}` | GET | Get campaign details |
| `/v1/campaigns/{id}` | PUT | Update campaign |
| `/v1/campaigns/{id}` | DELETE | Cancel campaign |
| `/v1/campaigns/{id}/start` | POST | Start campaign |
| `/v1/campaigns/{id}/pause` | POST | Pause campaign |
| `/v1/campaigns/{id}/metrics` | GET | Get campaign metrics |
| `/v1/campaigns/{id}/posts` | GET | List scheduled posts |
| `/v1/campaigns/{id}/posts` | POST | Add scheduled post |
| `/v1/campaigns/{id}/claims` | GET | List airdrop claims |
| `/v1/campaigns/{id}/claims/{address}` | GET | Get claim proof |
| `/v1/campaigns/otc` | POST | Create OTC deal |
| `/v1/campaigns/otc/{id}/accept` | POST | Accept OTC deal |
| `/v1/campaigns/otc/{id}/deposit` | POST | Deposit to escrow |
| `/v1/campaigns/social/connect/{platform}` | POST | Connect social account |

---

## 8. API Specifications

### 8.1 Authentication

All API endpoints require authentication via:

1. **Wallet Signature (EIP-712)** - Primary method
2. **JWT Token** - For session-based access
3. **API Key** - For server-to-server communication

```typescript
// EIP-712 Domain
const DOMAIN = {
  name: 'LaunchLab',
  version: '1',
  chainId: 271828,
  verifyingContract: '0x...', // LaunchLab Registry
};

// Authentication message
const AUTH_TYPES = {
  Authentication: [
    { name: 'wallet', type: 'address' },
    { name: 'nonce', type: 'uint256' },
    { name: 'expiry', type: 'uint256' },
    { name: 'action', type: 'string' },
  ],
};
```

### 8.2 Rate Limits

| Endpoint Category | Rate Limit | Window |
|-------------------|------------|--------|
| Public Read | 100 req/min | Per IP |
| Authenticated Read | 1000 req/min | Per wallet |
| Write Operations | 60 req/min | Per wallet |
| Bot Commands | 300 req/min | Per project |
| Social Posting | 50 req/hour | Per platform |

### 8.3 OpenAPI Specification

Full OpenAPI 3.1 specification available at `/v1/openapi.yaml`

---

## 9. Smart Contracts

### 9.1 Contract Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                       LAUNCHLAB CONTRACT ARCHITECTURE                           │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  PROXY LAYER (UUPS Upgradeable)                                                │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                       LaunchLabProxy                                    │   │
│  │                    (ERC-1967 Proxy)                                     │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                │                                               │
│  IMPLEMENTATION LAYER          ▼                                               │
│  ┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐                  │
│  │ LaunchLabCore   │ │  IdentityNFT    │ │  ProjectFactory │                  │
│  │ - Registry      │ │  - Soulbound    │ │  - Token Deploy │                  │
│  │ - Access Ctrl   │ │  - Metadata     │ │  - Pool Create  │                  │
│  │ - Fee Handler   │ │  - Updates      │ │  - RWA Mint     │                  │
│  └─────────────────┘ └─────────────────┘ └─────────────────┘                  │
│                                                                                 │
│  ┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐                  │
│  │  CampaignMgr    │ │  AirdropFactory │ │  EscrowFactory  │                  │
│  │  - Create       │ │  - Merkle       │ │  - OTC Deals    │                  │
│  │  - Fund         │ │  - Claims       │ │  - Vesting      │                  │
│  │  - Distribute   │ │  - Recovery     │ │  - Milestones   │                  │
│  └─────────────────┘ └─────────────────┘ └─────────────────┘                  │
│                                                                                 │
│  INTEGRATION LAYER                                                              │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │  DCSwapRouter │ DCSwapFactory │ T-REX Registry │ IPFS Gateway          │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 9.2 Deployed Addresses (Mainnet)

| Contract | Address | Type |
|----------|---------|------|
| LaunchLabCore | TBD | UUPS Proxy |
| LaunchLabIdentity | TBD | UUPS Proxy |
| ProjectFactory | TBD | UUPS Proxy |
| AirdropFactory | TBD | Beacon |
| EscrowFactory | TBD | Beacon |
| CampaignManager | TBD | UUPS Proxy |

---

## 10. Security Considerations

### 10.1 Bot Wallet Security

- **HD Wallet Derivation**: All bot wallets derived from project master seed
- **Key Encryption**: Private keys encrypted with project owner's public key
- **Access Control**: Only project owner can export or transfer funds
- **Rate Limiting**: Maximum transactions per wallet per block

### 10.2 Fund Security

- **Escrow Contracts**: All campaign funds locked in audited escrow
- **Time Locks**: Minimum 24-hour delay for large withdrawals
- **Multi-sig Option**: Optional multi-signature for fund access
- **Emergency Pause**: Admin can pause suspicious activities

### 10.3 API Security

- **Signature Verification**: All write operations require wallet signature
- **Nonce Tracking**: Prevent replay attacks
- **Rate Limiting**: Protect against DoS
- **Input Validation**: Strict parameter validation

---

## 11. Deployment Architecture

### 11.1 Production Infrastructure

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                       LAUNCHLAB DEPLOYMENT TOPOLOGY                             │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  LOAD BALANCER (Cloudflare)                                                    │
│  └── launchlab.dcswap.net                                                      │
│                                                                                 │
│  VPS: 92.243.26.114 (DCSwap Primary)                                           │
│  ├── nginx (443) ─────────────────────────────────────────────────────────────┤│
│  │   ├── /v1/launchlab/* → LaunchLab Core API (3010)                          ││
│  │   ├── /v1/botfarm/*   → Bot Farm Orchestrator (3011)                       ││
│  │   ├── /v1/mm/*        → Market Maker Service (3012)                        ││
│  │   └── /v1/campaigns/* → Campaign Engine (3013)                             ││
│  │                                                                             ││
│  ├── LaunchLab Core API (Rust, port 3010)                                     ││
│  ├── Bot Farm Orchestrator (Rust, port 3011)                                  ││
│  ├── Market Maker Service (Rust, port 3012)                                   ││
│  ├── Campaign Engine (Node.js, port 3013)                                     ││
│  │                                                                             ││
│  ├── Redis (6379) - Message bus, caching                                      ││
│  ├── IPFS (5001) - Media storage                                              ││
│  └── PostgreSQL (5432) - via Neon                                             ││
│                                                                                 │
│  EXTERNAL SERVICES                                                              │
│  ├── Neon PostgreSQL - neon.tech (managed)                                     │
│  ├── Datachain Rope RPC - erpc.datachain.network                               │
│  ├── IPFS Gateway - ipfs.datachain.network                                     │
│  ├── Twitter API - api.twitter.com                                             │
│  ├── Meta Graph API - graph.facebook.com                                       │
│  └── CoinMarketCap API - api.coinmarketcap.com                                 │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 11.2 Service Configuration

```toml
# launchlab.toml

[server]
host = "0.0.0.0"
port = 3010

[database]
url = "${NEON_DATABASE_URL}"
max_connections = 50
min_connections = 5

[redis]
url = "redis://localhost:6379"
pool_size = 20

[chain]
rpc_url = "https://erpc.datachain.network"
chain_id = 271828
router_address = "0x55e660B8ee61208381298382f8c3DEb3B8f7621b"
factory_address = "0x17d2ACc47f20d93eedA29dB6252eF22d3D7699B7"

[ipfs]
api_url = "http://localhost:5001"
gateway_url = "https://ipfs.datachain.network"

[botfarm]
max_wallets_per_project = 500
max_strategies_per_project = 20
tick_interval_ms = 100
gas_price_buffer_pct = 20

[marketmaker]
default_tick_ms = 1000
max_spread_bps = 1000
min_spread_bps = 10

[listings]
cmc_api_key = "${CMC_API_KEY}"
coingecko_api_key = "${COINGECKO_API_KEY}"

[campaigns]
twitter_client_id = "${TWITTER_CLIENT_ID}"
twitter_client_secret = "${TWITTER_CLIENT_SECRET}"
meta_app_id = "${META_APP_ID}"
meta_app_secret = "${META_APP_SECRET}"
```

---

## 12. Migration Path

### 12.1 Phase 1: Core Infrastructure (Weeks 1-3)

- [ ] Deploy LaunchLabIdentity NFT contract
- [ ] Deploy ProjectFactory contract
- [ ] Implement Core API service
- [ ] Set up PostgreSQL schema
- [ ] Configure Redis message bus
- [ ] Deploy initial frontend dashboard

### 12.2 Phase 2: Bot Farm Expansion (Weeks 4-6)

- [ ] Migrate existing 62-wallet, 9-strategy bot
- [ ] Implement 6 new strategy types
- [ ] Scale wallet pool to 500
- [ ] Add real-time monitoring dashboard
- [ ] Implement emergency stop mechanisms

### 12.3 Phase 3: Market Maker Service (Weeks 7-9)

- [ ] Build adaptive market maker engine
- [ ] Implement correlation engine
- [ ] Add inventory management
- [ ] Create MM configuration UI
- [ ] Deploy volume generation system

### 12.4 Phase 4: Listing Automation (Weeks 10-12)

- [ ] Build CoinMarketCap adapter
- [ ] Build CoinGecko adapter
- [ ] Implement auto-fill engine
- [ ] Create listing status tracker
- [ ] Add validation and error handling

### 12.5 Phase 5: Campaign Manager (Weeks 13-16)

- [ ] Integrate Twitter/X API
- [ ] Integrate Meta Graph API
- [ ] Build airdrop system with Merkle proofs
- [ ] Implement OTC escrow contracts
- [ ] Create campaign analytics dashboard

### 12.6 Phase 6: Testing & Launch (Weeks 17-18)

- [ ] Security audit of all contracts
- [ ] Load testing of services
- [ ] Beta testing with selected projects
- [ ] Documentation and tutorials
- [ ] Production launch

---

## Appendix A: Glossary

| Term | Definition |
|------|------------|
| **LLI-NFT** | LaunchLab Identity NFT - Soulbound token storing project owner metadata |
| **Bot Farm** | Automated trading wallet infrastructure |
| **Market Maker** | Service maintaining bid/ask spreads on trading pairs |
| **Merkle Airdrop** | Gas-efficient token distribution using Merkle proofs |
| **OTC** | Over-The-Counter - Private, negotiated trades |
| **Vesting** | Time-locked token release schedule |
| **TVL** | Total Value Locked - Measure of liquidity |
| **VWAP** | Volume-Weighted Average Price |

---

## Appendix B: References

1. DCSwap Handover Documentation (2026-03-01)
2. Datachain Rope Whitepaper
3. ERC-3643 (T-REX) Standard
4. CoinMarketCap Listing Guide
5. CoinGecko Listing Requirements
6. Twitter API v2 Documentation
7. Meta Graph API Documentation

---

**Document Control**

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0.0 | 2026-09-06 | Datachain Foundation | Initial specification |

---

*This specification is subject to change based on implementation feedback and evolving requirements.*
