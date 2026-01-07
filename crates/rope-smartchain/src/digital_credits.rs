//! # Digital Credits - Native Currency System
//! 
//! Datachain Rope's native currency system for minting, transferring,
//! and managing digital assets within the Smartchain.
//! 
//! ## Key Features
//! 
//! - **DC FAT**: DATACHAIN Future Access Token - the native currency
//! - **Custom Tokens**: Create custom digital assets
//! - **AI-Validated Operations**: All minting requires testimony validation
//! - **Quantum-Resistant**: Secured by OES cryptography
//! - **Bridge-Ready**: Can be wrapped/bridged to external chains
//! 
//! ## Token Types
//! 
//! 1. **DC FAT**: Native utility/gas token (DATACHAIN Future Access Token)
//! 2. **Custom Credits**: User-defined tokens (fungible)
//! 3. **NFTs**: Non-fungible digital assets
//! 4. **Wrapped Tokens**: Bridged from external chains
//! 
//! ## DC FAT Tokenomics (Layer 0 Scale)
//! 
//! - Symbol: FAT
//! - Name: DATACHAIN Future Access Token
//! - Decimals: 18
//! - Genesis Supply: 10,000,000,000 (10 billion) at launch
//! - Max Supply: **UNLIMITED** (like Solana, Ethereum, Polkadot)
//! - Annual Inflation Cap: 500 million FAT/year (~5% initial, decreasing %)
//! - Inflation: Controlled via AI Testimony validation + Governance
//! 
//! ## Minting Process (12 Approvals Required for DC FAT)
//! 
//! ```text
//! Mint Request → AI Testimony (5) → Random Governors (5) → Foundation (2) → Execute
//!                  │                   │                      │
//!                  │                   │                      └─ 2 Foundation wallets
//!                  │                   └─ 5 random active validators
//!                  └─ 5 AI Testimony Agents validate request
//! ```
//! 
//! **Security**: DC FAT minting MUST go through governance approval.
//! Direct minting is ONLY allowed for custom tokens created by their owners.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use parking_lot::RwLock;

/// Token identifier (32 bytes)
pub type TokenId = [u8; 32];

/// Account balance type (u128 for high precision)
pub type Balance = u128;

/// The native DC FAT (DATACHAIN Future Access Token) ID
pub const DC_FAT_TOKEN_ID: TokenId = [0u8; 32];

/// Alias for backward compatibility
pub const NATIVE_TOKEN_ID: TokenId = DC_FAT_TOKEN_ID;

/// Token definition
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Token {
    /// Unique token identifier
    pub id: TokenId,
    
    /// Token symbol (e.g., "ROPE", "CREDIT")
    pub symbol: String,
    
    /// Full name
    pub name: String,
    
    /// Decimal places (e.g., 18 for ETH-like)
    pub decimals: u8,
    
    /// Token type
    pub token_type: TokenType,
    
    /// Total supply (current)
    pub total_supply: Balance,
    
    /// Maximum supply (None = unlimited)
    pub max_supply: Option<Balance>,
    
    /// Creator/owner node ID
    pub creator: [u8; 32],
    
    /// Creation timestamp
    pub created_at: i64,
    
    /// Token metadata
    pub metadata: TokenMetadata,
    
    /// Minting rules
    pub minting_rules: MintingRules,
    
    /// Is token active?
    pub is_active: bool,
}

/// Token types
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TokenType {
    /// Native ROPE token
    Native,
    /// Fungible custom token
    Fungible,
    /// Non-fungible token (each unit is unique)
    NonFungible { collection_id: TokenId },
    /// Wrapped token from external chain
    Wrapped { 
        source_chain: String, 
        source_contract: String 
    },
    /// Stablecoin pegged to external value
    Stablecoin { 
        peg: String,  // e.g., "USD", "EUR"
        collateral_ratio_bps: u32, // Basis points (10000 = 100%)
    },
}

/// Token metadata
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenMetadata {
    /// Description
    pub description: String,
    /// Logo URI
    pub logo_uri: Option<String>,
    /// Website
    pub website: Option<String>,
    /// Additional attributes
    pub attributes: HashMap<String, String>,
}

impl Default for TokenMetadata {
    fn default() -> Self {
        Self {
            description: String::new(),
            logo_uri: None,
            website: None,
            attributes: HashMap::new(),
        }
    }
}

/// Rules governing minting
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MintingRules {
    /// Who can mint
    pub authorized_minters: Vec<[u8; 32]>,
    
    /// Requires governance approval (12 approvals: 5 AI + 5 governors + 2 foundation)
    pub requires_governance: bool,
    
    /// Requires AI testimony validation
    pub requires_testimony: bool,
    
    /// Minimum testimony agents required (5 for DC FAT)
    pub min_testimony_agents: u32,
    
    /// Minimum random governors required (5 for DC FAT)
    pub min_random_governors: u32,
    
    /// Minimum foundation members required (2 for DC FAT)
    pub min_foundation_members: u32,
    
    /// Minting rate limit (per block/anchor)
    pub rate_limit: Option<RateLimit>,
    
    /// Is minting currently enabled
    pub minting_enabled: bool,
    
    /// Amount minted this period (for rate limiting)
    pub minted_this_period: u128,
    
    /// Period start timestamp
    pub period_start: i64,
}

impl Default for MintingRules {
    fn default() -> Self {
        Self {
            authorized_minters: Vec::new(),
            requires_governance: true,
            requires_testimony: true,
            min_testimony_agents: 5,
            min_random_governors: 5,
            min_foundation_members: 2,
            rate_limit: None,
            minting_enabled: true,
            minted_this_period: 0,
            period_start: chrono::Utc::now().timestamp(),
        }
    }
}

impl MintingRules {
    /// Create minting rules for custom tokens (less strict)
    pub fn custom_token(creator: [u8; 32]) -> Self {
        Self {
            authorized_minters: vec![creator],
            requires_governance: false,
            requires_testimony: false,
            min_testimony_agents: 0,
            min_random_governors: 0,
            min_foundation_members: 0,
            rate_limit: None,
            minting_enabled: true,
            minted_this_period: 0,
            period_start: chrono::Utc::now().timestamp(),
        }
    }
    
    /// Create minting rules for DC FAT (strictest - 12 approvals required)
    pub fn dc_fat() -> Self {
        Self {
            authorized_minters: Vec::new(), // Governance controlled
            requires_governance: true,
            requires_testimony: true,
            min_testimony_agents: 5, // 5 AI agents
            min_random_governors: 5, // 5 random validators
            min_foundation_members: 2, // 2 foundation members
            rate_limit: Some(RateLimit {
                amount_per_period: 500_000_000 * 10u128.pow(18), // 500M FAT per year
                period_seconds: 31_536_000, // 1 year
            }),
            minting_enabled: true,
            minted_this_period: 0,
            period_start: chrono::Utc::now().timestamp(),
        }
    }
    
    /// Get total approvals required
    pub fn total_approvals_required(&self) -> u32 {
        self.min_testimony_agents + self.min_random_governors + self.min_foundation_members
    }
}

/// Rate limit for minting
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateLimit {
    pub amount_per_period: Balance,
    pub period_seconds: u64,
}

/// Account with token balances
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    /// Account owner (NodeId)
    pub owner: [u8; 32],
    
    /// Token balances
    pub balances: HashMap<TokenId, Balance>,
    
    /// Frozen balances (locked)
    pub frozen: HashMap<TokenId, Balance>,
    
    /// Allowances (delegated spending)
    pub allowances: HashMap<TokenId, HashMap<[u8; 32], Balance>>,
    
    /// Account nonce (for replay protection)
    pub nonce: u64,
    
    /// Created at
    pub created_at: i64,
}

impl Account {
    pub fn new(owner: [u8; 32]) -> Self {
        Self {
            owner,
            balances: HashMap::new(),
            frozen: HashMap::new(),
            allowances: HashMap::new(),
            nonce: 0,
            created_at: chrono::Utc::now().timestamp(),
        }
    }
    
    /// Get available balance (total - frozen)
    pub fn available_balance(&self, token_id: &TokenId) -> Balance {
        let total = self.balances.get(token_id).copied().unwrap_or(0);
        let frozen = self.frozen.get(token_id).copied().unwrap_or(0);
        total.saturating_sub(frozen)
    }
    
    /// Check if account has sufficient balance
    pub fn has_balance(&self, token_id: &TokenId, amount: Balance) -> bool {
        self.available_balance(token_id) >= amount
    }
}

/// Token operation request
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenOperation {
    /// Operation ID
    pub id: [u8; 32],
    
    /// Operation type
    pub operation: OperationType,
    
    /// Token ID
    pub token_id: TokenId,
    
    /// Initiator
    pub initiator: [u8; 32],
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Nonce
    pub nonce: u64,
    
    /// Signature
    pub signature: Vec<u8>,
}

/// Token operation types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OperationType {
    /// Create a new token
    CreateToken {
        symbol: String,
        name: String,
        decimals: u8,
        token_type: TokenType,
        initial_supply: Balance,
        max_supply: Option<Balance>,
        metadata: TokenMetadata,
        minting_rules: MintingRules,
    },
    
    /// Mint new tokens
    Mint {
        to: [u8; 32],
        amount: Balance,
        reason: String,
    },
    
    /// Burn tokens
    Burn {
        from: [u8; 32],
        amount: Balance,
        reason: String,
    },
    
    /// Transfer tokens
    Transfer {
        from: [u8; 32],
        to: [u8; 32],
        amount: Balance,
        memo: Option<String>,
    },
    
    /// Approve spending allowance
    Approve {
        spender: [u8; 32],
        amount: Balance,
    },
    
    /// Transfer using allowance
    TransferFrom {
        from: [u8; 32],
        to: [u8; 32],
        amount: Balance,
    },
    
    /// Freeze tokens
    Freeze {
        account: [u8; 32],
        amount: Balance,
        reason: String,
    },
    
    /// Unfreeze tokens
    Unfreeze {
        account: [u8; 32],
        amount: Balance,
    },
    
    /// Update token metadata
    UpdateMetadata {
        metadata: TokenMetadata,
    },
    
    /// Pause/unpause token
    SetActive {
        is_active: bool,
    },
}

/// Result of a token operation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationResult {
    /// Operation ID
    pub operation_id: [u8; 32],
    
    /// Success status
    pub success: bool,
    
    /// New balance (if applicable)
    pub new_balance: Option<Balance>,
    
    /// String ID in lattice (proof)
    pub string_id: Option<[u8; 32]>,
    
    /// Error message
    pub error: Option<String>,
    
    /// Timestamp
    pub timestamp: i64,
}

/// Digital Credits Ledger
pub struct CreditsLedger {
    /// All registered tokens
    tokens: RwLock<HashMap<TokenId, Token>>,
    
    /// Account balances
    accounts: RwLock<HashMap<[u8; 32], Account>>,
    
    /// Operation history (for audit)
    history: RwLock<Vec<OperationResult>>,
    
    /// Total value locked
    total_value_locked: RwLock<HashMap<TokenId, Balance>>,
}

impl CreditsLedger {
    /// Create new ledger with native DC FAT token
    pub fn new() -> Self {
        let mut tokens = HashMap::new();
        
        // Create native DC FAT (DATACHAIN Future Access Token)
        // Tokenomics designed for Layer 0/Layer 1 scale
        // Model: Unlimited supply with controlled annual inflation (like Solana)
        // Initial inflation: ~5% of circulating supply per year, decreasing over time
        let dc_fat_token = Token {
            id: DC_FAT_TOKEN_ID,
            symbol: "FAT".to_string(),
            name: "DATACHAIN Future Access Token".to_string(),
            decimals: 18,
            token_type: TokenType::Native,
            total_supply: 10_000_000_000 * 10u128.pow(18), // 10 billion at genesis
            max_supply: None, // UNLIMITED - like Solana, Ethereum, Polkadot
            creator: [0u8; 32], // Genesis creator
            created_at: chrono::Utc::now().timestamp(),
            metadata: TokenMetadata {
                description: "DC FAT - The native currency of Datachain Rope Smartchain. \
                    Future Access Token powers the decentralized data management infrastructure, \
                    enabling AI-validated transactions, smart contract execution, and cross-protocol \
                    interoperability. Unlimited supply with controlled annual inflation.".to_string(),
                logo_uri: Some("https://datachain.one/assets/dc-fat-logo.svg".to_string()),
                website: Some("https://datachain.one".to_string()),
                attributes: {
                    let mut attrs = HashMap::new();
                    attrs.insert("ticker".to_string(), "FAT".to_string());
                    attrs.insert("type".to_string(), "utility".to_string());
                    attrs.insert("network".to_string(), "Datachain Rope".to_string());
                    attrs.insert("standard".to_string(), "DC-20".to_string());
                    attrs.insert("supply_model".to_string(), "inflationary".to_string());
                    attrs.insert("genesis_supply".to_string(), "10000000000".to_string());
                    attrs.insert("annual_cap".to_string(), "500000000".to_string());
                    attrs.insert("initial_inflation".to_string(), "5%".to_string());
                    attrs
                },
            },
            minting_rules: MintingRules::dc_fat(), // 12 approvals required (5 AI + 5 governors + 2 foundation)
            is_active: true,
        };
        
        tokens.insert(DC_FAT_TOKEN_ID, dc_fat_token);
        
        Self {
            tokens: RwLock::new(tokens),
            accounts: RwLock::new(HashMap::new()),
            history: RwLock::new(Vec::new()),
            total_value_locked: RwLock::new(HashMap::new()),
        }
    }
    
    /// Create a new custom token
    pub fn create_token(
        &self,
        creator: [u8; 32],
        symbol: String,
        name: String,
        decimals: u8,
        token_type: TokenType,
        initial_supply: Balance,
        max_supply: Option<Balance>,
        metadata: TokenMetadata,
        minting_rules: MintingRules,
    ) -> Result<TokenId, LedgerError> {
        // Validate
        if symbol.len() > 10 {
            return Err(LedgerError::InvalidSymbol);
        }
        
        if let Some(max) = max_supply {
            if initial_supply > max {
                return Err(LedgerError::ExceedsMaxSupply);
            }
        }
        
        // Generate token ID
        let mut id_input = creator.to_vec();
        id_input.extend_from_slice(symbol.as_bytes());
        id_input.extend_from_slice(&chrono::Utc::now().timestamp().to_le_bytes());
        let token_id: TokenId = *blake3::hash(&id_input).as_bytes();
        
        // Check if already exists
        if self.tokens.read().contains_key(&token_id) {
            return Err(LedgerError::TokenExists);
        }
        
        let token = Token {
            id: token_id,
            symbol,
            name,
            decimals,
            token_type,
            total_supply: initial_supply,
            max_supply,
            creator,
            created_at: chrono::Utc::now().timestamp(),
            metadata,
            minting_rules,
            is_active: true,
        };
        
        self.tokens.write().insert(token_id, token);
        
        // Credit initial supply to creator
        if initial_supply > 0 {
            self.credit_account(&creator, &token_id, initial_supply)?;
        }
        
        Ok(token_id)
    }
    
    /// Mint new tokens (requires validation)
    /// 
    /// # Security
    /// 
    /// For DC FAT (native token), this function MUST only be called after
    /// governance approval (12 approvals: 5 AI + 5 governors + 2 foundation).
    /// 
    /// For custom tokens, minting is controlled by authorized_minters.
    /// 
    /// # Arguments
    /// 
    /// * `token_id` - Token to mint
    /// * `to` - Recipient address
    /// * `amount` - Amount to mint
    /// * `minter` - Who is executing the mint
    /// * `governance_approved` - Whether governance has approved (required for DC FAT)
    pub fn mint(
        &self,
        token_id: &TokenId,
        to: &[u8; 32],
        amount: Balance,
        minter: &[u8; 32],
        governance_approved: bool,
    ) -> Result<OperationResult, LedgerError> {
        let mut tokens = self.tokens.write();
        
        let token = tokens.get_mut(token_id)
            .ok_or(LedgerError::TokenNotFound)?;
        
        if !token.is_active {
            return Err(LedgerError::TokenInactive);
        }
        
        if !token.minting_rules.minting_enabled {
            return Err(LedgerError::MintingDisabled);
        }
        
        // CRITICAL: DC FAT requires governance approval (12 approvals)
        if token.minting_rules.requires_governance && !governance_approved {
            return Err(LedgerError::GovernanceRequired);
        }
        
        // Check max supply
        if let Some(max) = token.max_supply {
            if token.total_supply + amount > max {
                return Err(LedgerError::ExceedsMaxSupply);
            }
        }
        
        // Check rate limit for DC FAT
        if let Some(ref rate_limit) = token.minting_rules.rate_limit {
            let now = chrono::Utc::now().timestamp();
            let period_elapsed = now - token.minting_rules.period_start;
            
            // Reset period if expired
            if period_elapsed >= rate_limit.period_seconds as i64 {
                token.minting_rules.minted_this_period = 0;
                token.minting_rules.period_start = now;
            }
            
            // Check if minting would exceed rate limit
            if token.minting_rules.minted_this_period + amount > rate_limit.amount_per_period {
                return Err(LedgerError::RateLimitExceeded);
            }
            
            // Update minted amount
            token.minting_rules.minted_this_period += amount;
        }
        
        // Check authorization for custom tokens
        if !token.minting_rules.authorized_minters.is_empty() 
            && !token.minting_rules.authorized_minters.contains(minter) {
            return Err(LedgerError::Unauthorized);
        }
        
        // Update total supply
        token.total_supply += amount;
        
        drop(tokens); // Release lock before crediting
        
        // Credit to account
        self.credit_account(to, token_id, amount)?;
        
        let operation_id = *blake3::hash(&[
            to.as_slice(),
            token_id.as_slice(),
            &amount.to_le_bytes(),
        ].concat()).as_bytes();
        
        let result = OperationResult {
            operation_id,
            success: true,
            new_balance: Some(self.balance_of(to, token_id)),
            string_id: None, // Would be set when recorded in lattice
            error: None,
            timestamp: chrono::Utc::now().timestamp(),
        };
        
        self.history.write().push(result.clone());
        
        Ok(result)
    }
    
    /// Mint tokens without governance check (internal use only)
    /// 
    /// # Safety
    /// 
    /// This bypasses governance checks. Only use for:
    /// - Initial distribution during genesis
    /// - Custom tokens where creator has minting rights
    /// 
    /// NEVER use for DC FAT minting after genesis.
    fn mint_internal(
        &self,
        token_id: &TokenId,
        to: &[u8; 32],
        amount: Balance,
        minter: &[u8; 32],
    ) -> Result<OperationResult, LedgerError> {
        self.mint(token_id, to, amount, minter, true) // Skip governance for internal
    }
    
    /// Burn tokens
    pub fn burn(
        &self,
        token_id: &TokenId,
        from: &[u8; 32],
        amount: Balance,
    ) -> Result<OperationResult, LedgerError> {
        // Debit from account
        self.debit_account(from, token_id, amount)?;
        
        // Update total supply
        let mut tokens = self.tokens.write();
        if let Some(token) = tokens.get_mut(token_id) {
            token.total_supply = token.total_supply.saturating_sub(amount);
        }
        
        let operation_id = *blake3::hash(&[
            from.as_slice(),
            token_id.as_slice(),
            &amount.to_le_bytes(),
            b"burn",
        ].concat()).as_bytes();
        
        let result = OperationResult {
            operation_id,
            success: true,
            new_balance: Some(self.balance_of(from, token_id)),
            string_id: None,
            error: None,
            timestamp: chrono::Utc::now().timestamp(),
        };
        
        self.history.write().push(result.clone());
        
        Ok(result)
    }
    
    /// Transfer tokens
    pub fn transfer(
        &self,
        token_id: &TokenId,
        from: &[u8; 32],
        to: &[u8; 32],
        amount: Balance,
    ) -> Result<OperationResult, LedgerError> {
        // Check token is active
        let tokens = self.tokens.read();
        let token = tokens.get(token_id).ok_or(LedgerError::TokenNotFound)?;
        if !token.is_active {
            return Err(LedgerError::TokenInactive);
        }
        drop(tokens);
        
        // Debit from sender
        self.debit_account(from, token_id, amount)?;
        
        // Credit to receiver
        self.credit_account(to, token_id, amount)?;
        
        let operation_id = *blake3::hash(&[
            from.as_slice(),
            to.as_slice(),
            token_id.as_slice(),
            &amount.to_le_bytes(),
        ].concat()).as_bytes();
        
        let result = OperationResult {
            operation_id,
            success: true,
            new_balance: Some(self.balance_of(from, token_id)),
            string_id: None,
            error: None,
            timestamp: chrono::Utc::now().timestamp(),
        };
        
        self.history.write().push(result.clone());
        
        Ok(result)
    }
    
    /// Freeze tokens in an account
    pub fn freeze(
        &self,
        token_id: &TokenId,
        account: &[u8; 32],
        amount: Balance,
    ) -> Result<(), LedgerError> {
        let mut accounts = self.accounts.write();
        let acc = accounts.get_mut(account).ok_or(LedgerError::AccountNotFound)?;
        
        let available = acc.available_balance(token_id);
        if available < amount {
            return Err(LedgerError::InsufficientBalance);
        }
        
        *acc.frozen.entry(*token_id).or_insert(0) += amount;
        
        Ok(())
    }
    
    /// Unfreeze tokens
    pub fn unfreeze(
        &self,
        token_id: &TokenId,
        account: &[u8; 32],
        amount: Balance,
    ) -> Result<(), LedgerError> {
        let mut accounts = self.accounts.write();
        let acc = accounts.get_mut(account).ok_or(LedgerError::AccountNotFound)?;
        
        let frozen = acc.frozen.get(token_id).copied().unwrap_or(0);
        if frozen < amount {
            return Err(LedgerError::InsufficientFrozen);
        }
        
        *acc.frozen.get_mut(token_id).unwrap() -= amount;
        
        Ok(())
    }
    
    /// Get balance of an account
    pub fn balance_of(&self, account: &[u8; 32], token_id: &TokenId) -> Balance {
        self.accounts.read()
            .get(account)
            .map(|acc| acc.balances.get(token_id).copied().unwrap_or(0))
            .unwrap_or(0)
    }
    
    /// Get token info
    pub fn get_token(&self, token_id: &TokenId) -> Option<Token> {
        self.tokens.read().get(token_id).cloned()
    }
    
    /// List all tokens
    pub fn list_tokens(&self) -> Vec<Token> {
        self.tokens.read().values().cloned().collect()
    }
    
    /// Get total supply of a token
    pub fn total_supply(&self, token_id: &TokenId) -> Balance {
        self.tokens.read()
            .get(token_id)
            .map(|t| t.total_supply)
            .unwrap_or(0)
    }
    
    // === Internal helpers ===
    
    fn credit_account(
        &self,
        account: &[u8; 32],
        token_id: &TokenId,
        amount: Balance,
    ) -> Result<(), LedgerError> {
        let mut accounts = self.accounts.write();
        let acc = accounts.entry(*account).or_insert_with(|| Account::new(*account));
        *acc.balances.entry(*token_id).or_insert(0) += amount;
        Ok(())
    }
    
    fn debit_account(
        &self,
        account: &[u8; 32],
        token_id: &TokenId,
        amount: Balance,
    ) -> Result<(), LedgerError> {
        let mut accounts = self.accounts.write();
        let acc = accounts.get_mut(account).ok_or(LedgerError::AccountNotFound)?;
        
        if acc.available_balance(token_id) < amount {
            return Err(LedgerError::InsufficientBalance);
        }
        
        *acc.balances.get_mut(token_id).unwrap() -= amount;
        Ok(())
    }
}

impl Default for CreditsLedger {
    fn default() -> Self {
        Self::new()
    }
}

/// Ledger errors
#[derive(Clone, Debug, PartialEq)]
pub enum LedgerError {
    TokenNotFound,
    TokenExists,
    TokenInactive,
    AccountNotFound,
    InsufficientBalance,
    InsufficientFrozen,
    InsufficientAllowance,
    ExceedsMaxSupply,
    MintingDisabled,
    Unauthorized,
    InvalidSymbol,
    InvalidAmount,
    RateLimitExceeded,
    /// DC FAT minting requires governance approval (12 approvals)
    GovernanceRequired,
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::TokenNotFound => write!(f, "Token not found"),
            LedgerError::TokenExists => write!(f, "Token already exists"),
            LedgerError::TokenInactive => write!(f, "Token is inactive"),
            LedgerError::AccountNotFound => write!(f, "Account not found"),
            LedgerError::InsufficientBalance => write!(f, "Insufficient balance"),
            LedgerError::InsufficientFrozen => write!(f, "Insufficient frozen balance"),
            LedgerError::InsufficientAllowance => write!(f, "Insufficient allowance"),
            LedgerError::ExceedsMaxSupply => write!(f, "Exceeds maximum supply"),
            LedgerError::MintingDisabled => write!(f, "Minting is disabled"),
            LedgerError::Unauthorized => write!(f, "Unauthorized operation"),
            LedgerError::InvalidSymbol => write!(f, "Invalid token symbol"),
            LedgerError::InvalidAmount => write!(f, "Invalid amount"),
            LedgerError::RateLimitExceeded => write!(f, "Rate limit exceeded"),
            LedgerError::GovernanceRequired => write!(f, "DC FAT minting requires governance approval (12 approvals: 5 AI + 5 governors + 2 foundation)"),
        }
    }
}

impl std::error::Error for LedgerError {}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_native_token_exists() {
        let ledger = CreditsLedger::new();
        let dc_fat = ledger.get_token(&DC_FAT_TOKEN_ID);
        
        assert!(dc_fat.is_some());
        let dc_fat = dc_fat.unwrap();
        assert_eq!(dc_fat.symbol, "FAT");
        assert_eq!(dc_fat.name, "DATACHAIN Future Access Token");
        assert_eq!(dc_fat.decimals, 18);
        assert!(matches!(dc_fat.token_type, TokenType::Native));
        // 10 billion genesis supply
        assert_eq!(dc_fat.total_supply, 10_000_000_000 * 10u128.pow(18));
        // Unlimited max supply with controlled annual inflation (like Solana)
        assert_eq!(dc_fat.max_supply, None);
        // Check rate limit exists (500M per year cap)
        assert!(dc_fat.minting_rules.rate_limit.is_some());
    }
    
    #[test]
    fn test_create_custom_token() {
        let ledger = CreditsLedger::new();
        let creator = [1u8; 32];
        
        let token_id = ledger.create_token(
            creator,
            "TEST".to_string(),
            "Test Token".to_string(),
            8,
            TokenType::Fungible,
            1_000_000,
            Some(10_000_000),
            TokenMetadata::default(),
            MintingRules::default(),
        ).unwrap();
        
        let token = ledger.get_token(&token_id).unwrap();
        assert_eq!(token.symbol, "TEST");
        assert_eq!(token.total_supply, 1_000_000);
        
        // Creator should have initial supply
        assert_eq!(ledger.balance_of(&creator, &token_id), 1_000_000);
    }
    
    #[test]
    fn test_mint_tokens() {
        let ledger = CreditsLedger::new();
        let creator = [1u8; 32];
        let recipient = [2u8; 32];
        
        // Create token with creator as authorized minter (custom token, no governance required)
        let rules = MintingRules::custom_token(creator);
        
        let token_id = ledger.create_token(
            creator,
            "MINT".to_string(),
            "Mintable Token".to_string(),
            18,
            TokenType::Fungible,
            0,
            Some(1_000_000),
            TokenMetadata::default(),
            rules,
        ).unwrap();
        
        // Mint to recipient (no governance needed for custom tokens)
        let result = ledger.mint(&token_id, &recipient, 500_000, &creator, false).unwrap();
        
        assert!(result.success);
        assert_eq!(ledger.balance_of(&recipient, &token_id), 500_000);
        assert_eq!(ledger.total_supply(&token_id), 500_000);
    }
    
    #[test]
    fn test_dc_fat_requires_governance() {
        let ledger = CreditsLedger::new();
        let minter = [1u8; 32];
        let recipient = [2u8; 32];
        
        // Try to mint DC FAT without governance approval - should fail
        let result = ledger.mint(&DC_FAT_TOKEN_ID, &recipient, 1000, &minter, false);
        assert!(matches!(result, Err(LedgerError::GovernanceRequired)));
        
        // With governance approval - should succeed
        let result = ledger.mint(&DC_FAT_TOKEN_ID, &recipient, 1000, &minter, true);
        assert!(result.is_ok());
        assert_eq!(ledger.balance_of(&recipient, &DC_FAT_TOKEN_ID), 1000);
    }
    
    #[test]
    fn test_transfer() {
        let ledger = CreditsLedger::new();
        let alice = [1u8; 32];
        let bob = [2u8; 32];
        
        let token_id = ledger.create_token(
            alice,
            "TRF".to_string(),
            "Transfer Token".to_string(),
            18,
            TokenType::Fungible,
            1000,
            None,
            TokenMetadata::default(),
            MintingRules::default(),
        ).unwrap();
        
        // Alice transfers to Bob
        let result = ledger.transfer(&token_id, &alice, &bob, 300).unwrap();
        
        assert!(result.success);
        assert_eq!(ledger.balance_of(&alice, &token_id), 700);
        assert_eq!(ledger.balance_of(&bob, &token_id), 300);
    }
    
    #[test]
    fn test_burn() {
        let ledger = CreditsLedger::new();
        let owner = [1u8; 32];
        
        let token_id = ledger.create_token(
            owner,
            "BURN".to_string(),
            "Burnable Token".to_string(),
            18,
            TokenType::Fungible,
            1000,
            None,
            TokenMetadata::default(),
            MintingRules::default(),
        ).unwrap();
        
        // Burn some tokens
        let result = ledger.burn(&token_id, &owner, 400).unwrap();
        
        assert!(result.success);
        assert_eq!(ledger.balance_of(&owner, &token_id), 600);
        assert_eq!(ledger.total_supply(&token_id), 600);
    }
    
    #[test]
    fn test_freeze_unfreeze() {
        let ledger = CreditsLedger::new();
        let owner = [1u8; 32];
        
        let token_id = ledger.create_token(
            owner,
            "FREEZE".to_string(),
            "Freezable Token".to_string(),
            18,
            TokenType::Fungible,
            1000,
            None,
            TokenMetadata::default(),
            MintingRules::default(),
        ).unwrap();
        
        // Freeze 500
        ledger.freeze(&token_id, &owner, 500).unwrap();
        
        // Available should be 500
        let acc = ledger.accounts.read();
        let account = acc.get(&owner).unwrap();
        assert_eq!(account.available_balance(&token_id), 500);
        drop(acc);
        
        // Can't transfer more than available
        let result = ledger.transfer(&token_id, &owner, &[2u8; 32], 600);
        assert!(result.is_err());
        
        // Unfreeze
        ledger.unfreeze(&token_id, &owner, 500).unwrap();
        
        // Now can transfer
        ledger.transfer(&token_id, &owner, &[2u8; 32], 600).unwrap();
    }
}

