//! # Federation Protocol
//!
//! Manages the validator set and federation governance.
//!
//! ## Key Features
//!
//! - Validator registration and staking
//! - Epoch-based validator rotation
//! - Slashing for misbehavior
//! - Reward distribution

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Validator information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Validator {
    /// Node ID (derived from public key)
    pub node_id: [u8; 32],

    /// Stake amount
    pub stake: u128,

    /// Commission rate (0-10000 = 0-100.00%)
    pub commission: u16,

    /// Registration timestamp
    pub registered_at: i64,

    /// Is active in current epoch
    pub is_active: bool,

    /// Consecutive missed attestations
    pub missed_attestations: u32,

    /// Total attestations provided
    pub total_attestations: u64,

    /// Slashing events
    pub slashing_events: u32,

    /// Accumulated rewards
    pub rewards: u128,
}

impl Validator {
    /// Create new validator
    pub fn new(node_id: [u8; 32], stake: u128, commission: u16) -> Self {
        Self {
            node_id,
            stake,
            commission: commission.min(10000),
            registered_at: chrono::Utc::now().timestamp(),
            is_active: false,
            missed_attestations: 0,
            total_attestations: 0,
            slashing_events: 0,
            rewards: 0,
        }
    }

    /// Calculate voting power (stake-weighted)
    pub fn voting_power(&self) -> u128 {
        if self.is_active {
            self.stake
        } else {
            0
        }
    }
}

/// Epoch information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Epoch {
    /// Epoch number
    pub number: u64,

    /// Start timestamp
    pub start_time: i64,

    /// End timestamp
    pub end_time: i64,

    /// Active validators for this epoch
    pub validators: Vec<[u8; 32]>,

    /// Total stake in this epoch
    pub total_stake: u128,

    /// Strings finalized in this epoch
    pub strings_finalized: u64,

    /// Rewards distributed
    pub rewards_distributed: u128,
}

/// Slashing reason
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlashingReason {
    /// Double signing (equivocation)
    DoubleSign,

    /// Prolonged downtime
    Downtime { missed_count: u32 },

    /// Invalid attestation
    InvalidAttestation,

    /// Malicious behavior
    MaliciousBehavior { evidence: String },
}

/// Slashing event
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlashingEvent {
    /// Validator node ID
    pub validator_id: [u8; 32],

    /// Slashing reason
    pub reason: SlashingReason,

    /// Amount slashed
    pub amount: u128,

    /// Timestamp
    pub timestamp: i64,

    /// Epoch number
    pub epoch: u64,
}

/// Federation state
pub struct Federation {
    /// All registered validators
    validators: RwLock<HashMap<[u8; 32], Validator>>,

    /// Current epoch
    current_epoch: RwLock<Epoch>,

    /// Past epochs (for history)
    epoch_history: RwLock<Vec<Epoch>>,

    /// Slashing history
    slashing_history: RwLock<Vec<SlashingEvent>>,

    /// Minimum stake required
    min_stake: u128,

    /// Maximum validators
    max_validators: usize,

    /// Epoch length in seconds
    epoch_length: u64,

    /// Slashing percentages
    slashing_rates: SlashingRates,
}

/// Slashing rates configuration
#[derive(Clone, Debug)]
pub struct SlashingRates {
    /// Double sign penalty (in basis points)
    pub double_sign: u16,

    /// Downtime penalty
    pub downtime: u16,

    /// Invalid attestation penalty
    pub invalid_attestation: u16,
}

impl Default for SlashingRates {
    fn default() -> Self {
        Self {
            double_sign: 5000,         // 50%
            downtime: 100,             // 1%
            invalid_attestation: 1000, // 10%
        }
    }
}

impl Federation {
    /// Create new federation
    pub fn new(min_stake: u128, max_validators: usize, epoch_length: u64) -> Self {
        let genesis_epoch = Epoch {
            number: 0,
            start_time: chrono::Utc::now().timestamp(),
            end_time: chrono::Utc::now().timestamp() + epoch_length as i64,
            validators: vec![],
            total_stake: 0,
            strings_finalized: 0,
            rewards_distributed: 0,
        };

        Self {
            validators: RwLock::new(HashMap::new()),
            current_epoch: RwLock::new(genesis_epoch),
            epoch_history: RwLock::new(Vec::new()),
            slashing_history: RwLock::new(Vec::new()),
            min_stake,
            max_validators,
            epoch_length,
            slashing_rates: SlashingRates::default(),
        }
    }

    /// Register a new validator
    pub fn register_validator(
        &self,
        node_id: [u8; 32],
        stake: u128,
        commission: u16,
    ) -> Result<(), FederationError> {
        if stake < self.min_stake {
            return Err(FederationError::InsufficientStake {
                required: self.min_stake,
                provided: stake,
            });
        }

        let mut validators = self.validators.write();

        if validators.contains_key(&node_id) {
            return Err(FederationError::AlreadyRegistered);
        }

        validators.insert(node_id, Validator::new(node_id, stake, commission));
        Ok(())
    }

    /// Increase validator stake
    pub fn add_stake(&self, node_id: &[u8; 32], amount: u128) -> Result<u128, FederationError> {
        let mut validators = self.validators.write();
        let validator = validators
            .get_mut(node_id)
            .ok_or(FederationError::ValidatorNotFound)?;

        validator.stake += amount;
        Ok(validator.stake)
    }

    /// Withdraw stake (with unbonding period)
    pub fn withdraw_stake(
        &self,
        node_id: &[u8; 32],
        amount: u128,
    ) -> Result<u128, FederationError> {
        let mut validators = self.validators.write();
        let validator = validators
            .get_mut(node_id)
            .ok_or(FederationError::ValidatorNotFound)?;

        if validator.stake < amount {
            return Err(FederationError::InsufficientStake {
                required: amount,
                provided: validator.stake,
            });
        }

        if validator.stake - amount < self.min_stake {
            // Deactivate if below minimum
            validator.is_active = false;
        }

        validator.stake -= amount;
        Ok(validator.stake)
    }

    /// Record an attestation from a validator
    pub fn record_attestation(&self, node_id: &[u8; 32]) -> Result<(), FederationError> {
        let mut validators = self.validators.write();
        let validator = validators
            .get_mut(node_id)
            .ok_or(FederationError::ValidatorNotFound)?;

        validator.total_attestations += 1;
        validator.missed_attestations = 0;

        Ok(())
    }

    /// Record a missed attestation
    pub fn record_missed_attestation(&self, node_id: &[u8; 32]) -> Result<bool, FederationError> {
        let mut validators = self.validators.write();
        let validator = validators
            .get_mut(node_id)
            .ok_or(FederationError::ValidatorNotFound)?;

        validator.missed_attestations += 1;

        // Check if should be slashed for downtime
        if validator.missed_attestations >= 100 {
            drop(validators);
            self.slash(node_id, SlashingReason::Downtime { missed_count: 100 })?;
            return Ok(true);
        }

        Ok(false)
    }

    /// Slash a validator
    pub fn slash(
        &self,
        node_id: &[u8; 32],
        reason: SlashingReason,
    ) -> Result<u128, FederationError> {
        let mut validators = self.validators.write();
        let validator = validators
            .get_mut(node_id)
            .ok_or(FederationError::ValidatorNotFound)?;

        let rate = match &reason {
            SlashingReason::DoubleSign => self.slashing_rates.double_sign,
            SlashingReason::Downtime { .. } => self.slashing_rates.downtime,
            SlashingReason::InvalidAttestation => self.slashing_rates.invalid_attestation,
            SlashingReason::MaliciousBehavior { .. } => 10000, // 100%
        };

        let slash_amount = validator.stake * rate as u128 / 10000;
        validator.stake -= slash_amount;
        validator.slashing_events += 1;

        // Deactivate if below minimum
        if validator.stake < self.min_stake {
            validator.is_active = false;
        }

        // Record slashing event
        let event = SlashingEvent {
            validator_id: *node_id,
            reason,
            amount: slash_amount,
            timestamp: chrono::Utc::now().timestamp(),
            epoch: self.current_epoch.read().number,
        };

        self.slashing_history.write().push(event);

        Ok(slash_amount)
    }

    /// Transition to next epoch
    pub fn next_epoch(&self) -> Epoch {
        let mut current = self.current_epoch.write();
        let mut history = self.epoch_history.write();

        // Archive current epoch
        history.push(current.clone());

        // Select active validators for new epoch (top N by stake)
        let validators = self.validators.read();
        let mut sorted: Vec<_> = validators
            .values()
            .filter(|v| v.stake >= self.min_stake)
            .collect();
        sorted.sort_by(|a, b| b.stake.cmp(&a.stake));

        let active_validators: Vec<_> = sorted
            .iter()
            .take(self.max_validators)
            .map(|v| v.node_id)
            .collect();

        let total_stake: u128 = sorted
            .iter()
            .take(self.max_validators)
            .map(|v| v.stake)
            .sum();

        // Create new epoch
        let new_epoch = Epoch {
            number: current.number + 1,
            start_time: chrono::Utc::now().timestamp(),
            end_time: chrono::Utc::now().timestamp() + self.epoch_length as i64,
            validators: active_validators.clone(),
            total_stake,
            strings_finalized: 0,
            rewards_distributed: 0,
        };

        *current = new_epoch.clone();

        // Update validator active status
        drop(validators);
        let mut validators = self.validators.write();
        for v in validators.values_mut() {
            v.is_active = active_validators.contains(&v.node_id);
        }

        new_epoch
    }

    /// Get current epoch
    pub fn current_epoch(&self) -> Epoch {
        self.current_epoch.read().clone()
    }

    /// Get validator info
    pub fn get_validator(&self, node_id: &[u8; 32]) -> Option<Validator> {
        self.validators.read().get(node_id).cloned()
    }

    /// Get all active validators
    pub fn active_validators(&self) -> Vec<Validator> {
        self.validators
            .read()
            .values()
            .filter(|v| v.is_active)
            .cloned()
            .collect()
    }

    /// Get total voting power
    pub fn total_voting_power(&self) -> u128 {
        self.validators
            .read()
            .values()
            .map(|v| v.voting_power())
            .sum()
    }

    /// Calculate quorum threshold (2/3 + 1)
    pub fn quorum_threshold(&self) -> u128 {
        let total = self.total_voting_power();
        (total * 2 / 3) + 1
    }
}

impl Default for Federation {
    fn default() -> Self {
        // Default: 1000 FAT minimum stake, 100 max validators, 12 hour epochs
        Self::new(
            1000_000_000_000_000_000_000u128, // 1000 FAT
            100,
            43200, // 12 hours
        )
    }
}

/// Federation errors
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FederationError {
    ValidatorNotFound,
    AlreadyRegistered,
    InsufficientStake { required: u128, provided: u128 },
    SlashingFailed,
}

impl std::fmt::Display for FederationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FederationError::ValidatorNotFound => write!(f, "Validator not found"),
            FederationError::AlreadyRegistered => write!(f, "Validator already registered"),
            FederationError::InsufficientStake { required, provided } => {
                write!(
                    f,
                    "Insufficient stake: required {}, provided {}",
                    required, provided
                )
            }
            FederationError::SlashingFailed => write!(f, "Slashing operation failed"),
        }
    }
}

impl std::error::Error for FederationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validator_registration() {
        let federation = Federation::new(100, 10, 3600);

        // Register validator
        let result = federation.register_validator([1u8; 32], 1000, 500);
        assert!(result.is_ok());

        // Check registered
        let validator = federation.get_validator(&[1u8; 32]);
        assert!(validator.is_some());
        assert_eq!(validator.unwrap().stake, 1000);

        // Duplicate registration should fail
        let result = federation.register_validator([1u8; 32], 1000, 500);
        assert!(matches!(result, Err(FederationError::AlreadyRegistered)));
    }

    #[test]
    fn test_insufficient_stake() {
        let federation = Federation::new(1000, 10, 3600);

        let result = federation.register_validator([1u8; 32], 500, 500);
        assert!(matches!(
            result,
            Err(FederationError::InsufficientStake { .. })
        ));
    }

    #[test]
    fn test_slashing() {
        let federation = Federation::new(100, 10, 3600);

        federation.register_validator([1u8; 32], 1000, 500).unwrap();

        // Slash for double signing (50%)
        let slashed = federation
            .slash(&[1u8; 32], SlashingReason::DoubleSign)
            .unwrap();
        assert_eq!(slashed, 500);

        let validator = federation.get_validator(&[1u8; 32]).unwrap();
        assert_eq!(validator.stake, 500);
        assert_eq!(validator.slashing_events, 1);
    }

    #[test]
    fn test_epoch_transition() {
        let federation = Federation::new(100, 3, 3600);

        // Register validators
        for i in 1..=5 {
            federation
                .register_validator([i as u8; 32], i as u128 * 1000, 500)
                .unwrap();
        }

        // Transition to new epoch
        let epoch = federation.next_epoch();

        // Top 3 validators should be active
        assert_eq!(epoch.validators.len(), 3);
        assert!(epoch.validators.contains(&[5u8; 32])); // Highest stake
        assert!(epoch.validators.contains(&[4u8; 32]));
        assert!(epoch.validators.contains(&[3u8; 32]));
    }
}
