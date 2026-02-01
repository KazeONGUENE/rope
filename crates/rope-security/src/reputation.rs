//! On-Chain Reputation System
//!
//! Entity reputation tracking with slashing for misbehavior

use super::*;

/// Reputation score (0-1000)
pub type ReputationScore = u32;

/// Maximum reputation score
pub const MAX_REPUTATION: ReputationScore = 1000;

/// Minimum reputation for participation
pub const MIN_REPUTATION: ReputationScore = 100;

/// Entity reputation record
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReputationRecord {
    /// Entity identifier
    pub entity_id: [u8; 32],
    /// Current reputation score
    pub score: ReputationScore,
    /// Total positive actions
    pub positive_actions: u64,
    /// Total negative actions (violations)
    pub negative_actions: u64,
    /// Total slashed amount (in wei)
    pub total_slashed: u128,
    /// Last activity timestamp
    pub last_activity: i64,
    /// Registration timestamp
    pub registered_at: i64,
    /// Is entity active
    pub active: bool,
    /// Violation history
    pub violations: Vec<Violation>,
}

/// Violation record
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Violation {
    pub id: String,
    pub violation_type: ViolationType,
    pub severity: Severity,
    pub timestamp: i64,
    pub slash_amount: u128,
    pub evidence: String,
    pub resolved: bool,
}

/// Types of violations
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ViolationType {
    /// Invalid testimony provided
    InvalidTestimony,
    /// Double-voting detected
    DoubleVoting,
    /// Offline too long
    Downtime,
    /// Malicious behavior
    MaliciousBehavior,
    /// Spam/flooding
    Spam,
    /// Failed to complete assigned task
    TaskFailure,
    /// Collusion detected
    Collusion,
    /// Data corruption
    DataCorruption,
}

impl ViolationType {
    /// Get default slash percentage for violation type
    pub fn slash_percentage(&self) -> u32 {
        match self {
            ViolationType::InvalidTestimony => 5,
            ViolationType::DoubleVoting => 50,
            ViolationType::Downtime => 1,
            ViolationType::MaliciousBehavior => 100,
            ViolationType::Spam => 10,
            ViolationType::TaskFailure => 2,
            ViolationType::Collusion => 100,
            ViolationType::DataCorruption => 25,
        }
    }

    /// Get reputation penalty
    pub fn reputation_penalty(&self) -> ReputationScore {
        match self {
            ViolationType::InvalidTestimony => 50,
            ViolationType::DoubleVoting => 200,
            ViolationType::Downtime => 10,
            ViolationType::MaliciousBehavior => 500,
            ViolationType::Spam => 100,
            ViolationType::TaskFailure => 20,
            ViolationType::Collusion => 500,
            ViolationType::DataCorruption => 150,
        }
    }
}

/// Reputation manager
pub struct ReputationManager {
    /// Entity records
    records: RwLock<HashMap<[u8; 32], ReputationRecord>>,
    /// Slashing configuration
    config: SlashingConfig,
    /// Total slashed amount
    total_slashed: RwLock<u128>,
    /// Slash event listeners
    slash_history: RwLock<Vec<SlashEvent>>,
}

/// Slashing configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlashingConfig {
    /// Minimum stake required
    pub min_stake: u128,
    /// Grace period for downtime (seconds)
    pub downtime_grace_seconds: u64,
    /// Maximum violations before ban
    pub max_violations: u32,
    /// Reputation recovery rate per epoch
    pub recovery_rate: ReputationScore,
    /// Cooldown period after violation (seconds)
    pub violation_cooldown: u64,
}

impl Default for SlashingConfig {
    fn default() -> Self {
        Self {
            min_stake: 1_000_000_000_000_000_000_000, // 1000 tokens
            downtime_grace_seconds: 3600,             // 1 hour
            max_violations: 10,
            recovery_rate: 5,
            violation_cooldown: 86400, // 24 hours
        }
    }
}

/// Slash event
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlashEvent {
    pub entity_id: [u8; 32],
    pub violation_type: ViolationType,
    pub slash_amount: u128,
    pub new_reputation: ReputationScore,
    pub timestamp: i64,
}

impl ReputationManager {
    /// Create new reputation manager
    pub fn new(config: SlashingConfig) -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
            config,
            total_slashed: RwLock::new(0),
            slash_history: RwLock::new(Vec::new()),
        }
    }

    /// Register new entity
    pub fn register_entity(&self, entity_id: [u8; 32]) -> Result<(), ReputationError> {
        let mut records = self.records.write();

        if records.contains_key(&entity_id) {
            return Err(ReputationError::AlreadyRegistered);
        }

        let now = chrono::Utc::now().timestamp();

        records.insert(
            entity_id,
            ReputationRecord {
                entity_id,
                score: MAX_REPUTATION / 2, // Start at 500
                positive_actions: 0,
                negative_actions: 0,
                total_slashed: 0,
                last_activity: now,
                registered_at: now,
                active: true,
                violations: Vec::new(),
            },
        );

        tracing::info!("Entity registered: {}", hex::encode(entity_id));
        Ok(())
    }

    /// Get entity reputation
    pub fn get_reputation(&self, entity_id: &[u8; 32]) -> Option<ReputationScore> {
        self.records.read().get(entity_id).map(|r| r.score)
    }

    /// Get full record
    pub fn get_record(&self, entity_id: &[u8; 32]) -> Option<ReputationRecord> {
        self.records.read().get(entity_id).cloned()
    }

    /// Record positive action
    pub fn record_positive(&self, entity_id: &[u8; 32], points: ReputationScore) {
        let mut records = self.records.write();

        if let Some(record) = records.get_mut(entity_id) {
            record.positive_actions += 1;
            record.score = (record.score + points).min(MAX_REPUTATION);
            record.last_activity = chrono::Utc::now().timestamp();
        }
    }

    /// Report violation and slash
    pub fn report_violation(
        &self,
        entity_id: &[u8; 32],
        violation_type: ViolationType,
        stake: u128,
        evidence: &str,
    ) -> Result<SlashEvent, ReputationError> {
        let mut records = self.records.write();

        let record = records
            .get_mut(entity_id)
            .ok_or(ReputationError::EntityNotFound)?;

        // Check cooldown
        let now = chrono::Utc::now().timestamp();
        if let Some(last_violation) = record.violations.last() {
            if now - last_violation.timestamp < self.config.violation_cooldown as i64 {
                // Already in cooldown, reduce penalty
            }
        }

        // Calculate slash amount
        let slash_percentage = violation_type.slash_percentage();
        let slash_amount = (stake as f64 * slash_percentage as f64 / 100.0) as u128;

        // Calculate reputation penalty
        let reputation_penalty = violation_type.reputation_penalty();
        record.score = record.score.saturating_sub(reputation_penalty);

        // Record violation
        let violation = Violation {
            id: format!("V-{}", now),
            violation_type: violation_type.clone(),
            severity: match slash_percentage {
                0..=10 => Severity::Low,
                11..=25 => Severity::Medium,
                26..=50 => Severity::High,
                _ => Severity::Critical,
            },
            timestamp: now,
            slash_amount,
            evidence: evidence.to_string(),
            resolved: false,
        };

        record.violations.push(violation);
        record.negative_actions += 1;
        record.total_slashed += slash_amount;
        record.last_activity = now;

        // Check if should be deactivated
        if record.violations.len() as u32 >= self.config.max_violations
            || record.score < MIN_REPUTATION
        {
            record.active = false;
            tracing::warn!(
                "Entity {} deactivated due to violations",
                hex::encode(entity_id)
            );
        }

        // Record slash event
        let event = SlashEvent {
            entity_id: *entity_id,
            violation_type,
            slash_amount,
            new_reputation: record.score,
            timestamp: now,
        };

        // Update totals
        *self.total_slashed.write() += slash_amount;

        // Store in history
        self.slash_history.write().push(event.clone());

        tracing::warn!(
            "Slashed entity {}: {} wei, new reputation: {}",
            hex::encode(entity_id),
            slash_amount,
            record.score
        );

        Ok(event)
    }

    /// Check if entity can participate
    pub fn can_participate(&self, entity_id: &[u8; 32]) -> bool {
        self.records
            .read()
            .get(entity_id)
            .map(|r| r.active && r.score >= MIN_REPUTATION)
            .unwrap_or(false)
    }

    /// Process epoch - recover reputation for good actors
    pub fn process_epoch(&self) {
        let mut records = self.records.write();
        let recovery = self.config.recovery_rate;

        for record in records.values_mut() {
            if record.active && record.score < MAX_REPUTATION {
                // Only recover if no recent violations
                let now = chrono::Utc::now().timestamp();
                let recent_violation = record.violations.iter().any(|v| now - v.timestamp < 86400);

                if !recent_violation {
                    record.score = (record.score + recovery).min(MAX_REPUTATION);
                }
            }
        }
    }

    /// Get top entities by reputation
    pub fn top_entities(&self, count: usize) -> Vec<(ReputationScore, [u8; 32])> {
        let records = self.records.read();
        let mut entities: Vec<_> = records
            .values()
            .filter(|r| r.active)
            .map(|r| (r.score, r.entity_id))
            .collect();

        entities.sort_by(|a, b| b.0.cmp(&a.0));
        entities.truncate(count);
        entities
    }

    /// Get total slashed amount
    pub fn total_slashed(&self) -> u128 {
        *self.total_slashed.read()
    }

    /// Get slash history
    pub fn slash_history(&self) -> Vec<SlashEvent> {
        self.slash_history.read().clone()
    }

    /// Get entity count
    pub fn entity_count(&self) -> usize {
        self.records.read().len()
    }

    /// Get active entity count
    pub fn active_entity_count(&self) -> usize {
        self.records.read().values().filter(|r| r.active).count()
    }
}

impl Default for ReputationManager {
    fn default() -> Self {
        Self::new(SlashingConfig::default())
    }
}

/// Reputation errors
#[derive(Debug, Error)]
pub enum ReputationError {
    #[error("Entity already registered")]
    AlreadyRegistered,

    #[error("Entity not found")]
    EntityNotFound,

    #[error("Entity deactivated")]
    EntityDeactivated,

    #[error("Insufficient reputation")]
    InsufficientReputation,

    #[error("Insufficient stake")]
    InsufficientStake,
}

/// AI Agent reputation specifically
pub struct AgentReputationManager {
    /// Base reputation manager
    base: ReputationManager,
    /// Agent-specific metrics
    agent_metrics: RwLock<HashMap<[u8; 32], AgentMetrics>>,
}

/// Agent-specific performance metrics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentMetrics {
    /// Total testimonies provided
    pub testimonies_total: u64,
    /// Valid testimonies
    pub testimonies_valid: u64,
    /// Average response time (ms)
    pub avg_response_time_ms: u64,
    /// Uptime percentage (0-100)
    pub uptime_percentage: u8,
    /// Skill executions
    pub skill_executions: u64,
    /// Skill failures
    pub skill_failures: u64,
}

impl AgentReputationManager {
    pub fn new(config: SlashingConfig) -> Self {
        Self {
            base: ReputationManager::new(config),
            agent_metrics: RwLock::new(HashMap::new()),
        }
    }

    /// Register AI agent
    pub fn register_agent(&self, agent_id: [u8; 32]) -> Result<(), ReputationError> {
        self.base.register_entity(agent_id)?;
        self.agent_metrics
            .write()
            .insert(agent_id, AgentMetrics::default());
        Ok(())
    }

    /// Record testimony
    pub fn record_testimony(&self, agent_id: &[u8; 32], valid: bool, response_time_ms: u64) {
        {
            let mut metrics = self.agent_metrics.write();
            if let Some(m) = metrics.get_mut(agent_id) {
                m.testimonies_total += 1;
                if valid {
                    m.testimonies_valid += 1;
                }
                // Update average response time
                m.avg_response_time_ms = (m.avg_response_time_ms * (m.testimonies_total - 1)
                    + response_time_ms)
                    / m.testimonies_total;
            }
        }

        // Update reputation
        if valid {
            self.base.record_positive(agent_id, 1);
        } else {
            let _ = self.base.report_violation(
                agent_id,
                ViolationType::InvalidTestimony,
                0,
                "Invalid testimony provided",
            );
        }
    }

    /// Get agent metrics
    pub fn get_metrics(&self, agent_id: &[u8; 32]) -> Option<AgentMetrics> {
        self.agent_metrics.read().get(agent_id).cloned()
    }

    /// Get agent accuracy rate
    pub fn accuracy_rate(&self, agent_id: &[u8; 32]) -> Option<f64> {
        self.agent_metrics.read().get(agent_id).map(|m| {
            if m.testimonies_total > 0 {
                m.testimonies_valid as f64 / m.testimonies_total as f64
            } else {
                1.0
            }
        })
    }

    /// Get base reputation
    pub fn get_reputation(&self, agent_id: &[u8; 32]) -> Option<ReputationScore> {
        self.base.get_reputation(agent_id)
    }

    /// Can agent participate
    pub fn can_participate(&self, agent_id: &[u8; 32]) -> bool {
        self.base.can_participate(agent_id)
    }
}

impl Default for AgentReputationManager {
    fn default() -> Self {
        Self::new(SlashingConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_registration() {
        let manager = ReputationManager::default();
        let entity = [1u8; 32];

        assert!(manager.register_entity(entity).is_ok());
        assert!(manager.register_entity(entity).is_err()); // Already registered

        let rep = manager.get_reputation(&entity).unwrap();
        assert_eq!(rep, 500); // Starting reputation
    }

    #[test]
    fn test_positive_actions() {
        let manager = ReputationManager::default();
        let entity = [2u8; 32];

        manager.register_entity(entity).unwrap();
        manager.record_positive(&entity, 100);

        let rep = manager.get_reputation(&entity).unwrap();
        assert_eq!(rep, 600);
    }

    #[test]
    fn test_violation_slashing() {
        let manager = ReputationManager::default();
        let entity = [3u8; 32];

        manager.register_entity(entity).unwrap();

        let event = manager
            .report_violation(&entity, ViolationType::DoubleVoting, 1_000_000, "Evidence")
            .unwrap();

        assert_eq!(event.slash_amount, 500_000); // 50% of stake

        let rep = manager.get_reputation(&entity).unwrap();
        assert!(rep < 500); // Reputation reduced
    }

    #[test]
    fn test_deactivation() {
        let config = SlashingConfig {
            max_violations: 2,
            ..Default::default()
        };
        let manager = ReputationManager::new(config);
        let entity = [4u8; 32];

        manager.register_entity(entity).unwrap();

        // Report violations until deactivated
        manager
            .report_violation(&entity, ViolationType::Spam, 1000, "")
            .unwrap();
        manager
            .report_violation(&entity, ViolationType::Spam, 1000, "")
            .unwrap();

        assert!(!manager.can_participate(&entity));
    }

    #[test]
    fn test_agent_reputation() {
        let manager = AgentReputationManager::default();
        let agent = [5u8; 32];

        manager.register_agent(agent).unwrap();

        // Record testimonies
        manager.record_testimony(&agent, true, 100);
        manager.record_testimony(&agent, true, 150);
        manager.record_testimony(&agent, false, 200);

        let metrics = manager.get_metrics(&agent).unwrap();
        assert_eq!(metrics.testimonies_total, 3);
        assert_eq!(metrics.testimonies_valid, 2);

        let accuracy = manager.accuracy_rate(&agent).unwrap();
        assert!((accuracy - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_violation_types() {
        assert_eq!(ViolationType::MaliciousBehavior.slash_percentage(), 100);
        assert_eq!(ViolationType::Downtime.slash_percentage(), 1);
        assert!(
            ViolationType::DoubleVoting.reputation_penalty()
                > ViolationType::Downtime.reputation_penalty()
        );
    }
}
