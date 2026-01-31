//! # Finality Engine
//!
//! State machine for managing string finality in the Datachain Rope.
//!
//! ## Finality Conditions
//!
//! A string achieves finality when ALL of the following are met:
//! 1. Referenced by at least 3 anchor strings
//! 2. Received 2f+1 testimonies (Byzantine threshold)
//! 3. All parent strings are also finalized
//!
//! ## State Transitions
//!
//! ```text
//! Pending → Tentative → Final
//!              ↓
//!          Rejected
//! ```

use parking_lot::RwLock;
use rope_core::types::StringId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::testimony::{TestimonyCollector, TestimonyConfig};
use crate::virtual_voting::VirtualVotingState;

/// Finality engine configuration
#[derive(Clone, Debug)]
pub struct FinalityConfig {
    /// Minimum anchor confirmations required
    pub min_anchor_confirmations: u32,

    /// Minimum testimonies required (2f+1)
    pub min_testimonies: usize,

    /// Maximum time to wait for finality (seconds)
    pub finality_timeout_secs: u64,

    /// Whether to require parent finality
    pub require_parent_finality: bool,
}

impl Default for FinalityConfig {
    fn default() -> Self {
        Self {
            min_anchor_confirmations: 3,
            min_testimonies: 15,        // 2f+1 for 21 validators
            finality_timeout_secs: 300, // 5 minutes
            require_parent_finality: true,
        }
    }
}

/// Finality state for a string
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinalityState {
    /// Just created, awaiting processing
    Pending,

    /// Has some confirmations but not finalized
    Tentative {
        anchor_confirmations: u32,
        testimony_count: usize,
        confidence: u8,
    },

    /// Fully finalized
    Final {
        anchor_id: [u8; 32],
        finalized_at: i64,
        total_testimonies: usize,
    },

    /// Rejected by consensus
    Rejected { reason: String, rejected_at: i64 },

    /// Expired without achieving finality
    Expired { expired_at: i64 },
}

impl FinalityState {
    pub fn is_final(&self) -> bool {
        matches!(self, FinalityState::Final { .. })
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, FinalityState::Pending)
    }

    pub fn is_rejected(&self) -> bool {
        matches!(
            self,
            FinalityState::Rejected { .. } | FinalityState::Expired { .. }
        )
    }

    pub fn confidence(&self) -> u8 {
        match self {
            FinalityState::Pending => 0,
            FinalityState::Tentative { confidence, .. } => *confidence,
            FinalityState::Final { .. } => 100,
            FinalityState::Rejected { .. } | FinalityState::Expired { .. } => 0,
        }
    }
}

impl Default for FinalityState {
    fn default() -> Self {
        FinalityState::Pending
    }
}

/// String metadata for finality tracking
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StringFinalityInfo {
    pub string_id: StringId,
    pub state: FinalityState,
    pub parents: Vec<StringId>,
    pub anchor_confirmations: u32,
    pub testimony_count: usize,
    pub created_at: i64,
    pub last_updated: i64,
}

impl StringFinalityInfo {
    pub fn new(string_id: StringId, parents: Vec<StringId>) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            string_id,
            state: FinalityState::Pending,
            parents,
            anchor_confirmations: 0,
            testimony_count: 0,
            created_at: now,
            last_updated: now,
        }
    }
}

/// Finality engine - manages the finality state machine
pub struct FinalityEngine {
    /// Configuration
    config: FinalityConfig,

    /// String finality states
    strings: RwLock<HashMap<StringId, StringFinalityInfo>>,

    /// Finalized strings (for quick lookup)
    finalized: RwLock<HashSet<StringId>>,

    /// Pending strings queue (ordered by creation time)
    pending_queue: RwLock<VecDeque<StringId>>,

    /// Testimony collector
    testimony_collector: TestimonyCollector,

    /// Virtual voting state
    voting_state: RwLock<VirtualVotingState>,

    /// Known anchor strings
    anchors: RwLock<Vec<AnchorInfo>>,
}

/// Anchor string info
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnchorInfo {
    pub anchor_id: [u8; 32],
    pub round: u64,
    pub referenced_strings: Vec<StringId>,
    pub created_at: i64,
}

impl FinalityEngine {
    /// Create new finality engine
    pub fn new(config: FinalityConfig) -> Self {
        let testimony_config = TestimonyConfig {
            finality_threshold: config.min_testimonies,
            max_testimony_age: 1000,
            verify_signatures: true,
        };

        Self {
            config,
            strings: RwLock::new(HashMap::new()),
            finalized: RwLock::new(HashSet::new()),
            pending_queue: RwLock::new(VecDeque::new()),
            testimony_collector: TestimonyCollector::new(testimony_config),
            voting_state: RwLock::new(VirtualVotingState::new()),
            anchors: RwLock::new(Vec::new()),
        }
    }

    /// Register a new string for finality tracking
    pub fn register_string(&self, string_id: StringId, parents: Vec<StringId>) {
        let info = StringFinalityInfo::new(string_id, parents);

        self.strings.write().insert(string_id, info);
        self.pending_queue.write().push_back(string_id);
    }

    /// Record an anchor string
    pub fn record_anchor(
        &self,
        anchor_id: [u8; 32],
        round: u64,
        referenced_strings: Vec<StringId>,
    ) {
        let anchor = AnchorInfo {
            anchor_id,
            round,
            referenced_strings: referenced_strings.clone(),
            created_at: chrono::Utc::now().timestamp(),
        };

        self.anchors.write().push(anchor);

        // Update anchor confirmations for referenced strings
        let mut strings = self.strings.write();
        for string_id in referenced_strings {
            if let Some(info) = strings.get_mut(&string_id) {
                info.anchor_confirmations += 1;
                info.last_updated = chrono::Utc::now().timestamp();

                // Check if this triggers finality
                self.check_and_update_finality(info, &anchor_id);
            }
        }
    }

    /// Update testimony count for a string
    pub fn update_testimony_count(&self, string_id: &StringId, count: usize) {
        let mut strings = self.strings.write();
        if let Some(info) = strings.get_mut(string_id) {
            info.testimony_count = count;
            info.last_updated = chrono::Utc::now().timestamp();

            // Check finality without anchor
            self.check_tentative_state(info);
        }
    }

    /// Check and update finality state
    fn check_and_update_finality(&self, info: &mut StringFinalityInfo, anchor_id: &[u8; 32]) {
        // Skip if already final or rejected
        if info.state.is_final() || info.state.is_rejected() {
            return;
        }

        let now = chrono::Utc::now().timestamp();

        // Check all conditions for finality
        let has_anchor_confirmations =
            info.anchor_confirmations >= self.config.min_anchor_confirmations;
        let has_testimonies = info.testimony_count >= self.config.min_testimonies;
        let parents_finalized = self.check_parents_finalized(&info.parents);

        if has_anchor_confirmations && has_testimonies && parents_finalized {
            // Achieve finality!
            info.state = FinalityState::Final {
                anchor_id: *anchor_id,
                finalized_at: now,
                total_testimonies: info.testimony_count,
            };

            // Add to finalized set
            self.finalized.write().insert(info.string_id);
        } else {
            // Update tentative state
            let confidence = self.calculate_confidence(
                info.anchor_confirmations,
                info.testimony_count,
                parents_finalized,
            );

            info.state = FinalityState::Tentative {
                anchor_confirmations: info.anchor_confirmations,
                testimony_count: info.testimony_count,
                confidence,
            };
        }
    }

    /// Check tentative state (without anchor)
    fn check_tentative_state(&self, info: &mut StringFinalityInfo) {
        if info.state.is_final() || info.state.is_rejected() {
            return;
        }

        let parents_finalized = self.check_parents_finalized(&info.parents);
        let confidence = self.calculate_confidence(
            info.anchor_confirmations,
            info.testimony_count,
            parents_finalized,
        );

        if confidence > 0 {
            info.state = FinalityState::Tentative {
                anchor_confirmations: info.anchor_confirmations,
                testimony_count: info.testimony_count,
                confidence,
            };
        }
    }

    /// Check if all parents are finalized
    fn check_parents_finalized(&self, parents: &[StringId]) -> bool {
        if !self.config.require_parent_finality {
            return true;
        }

        let finalized = self.finalized.read();
        parents.iter().all(|p| {
            // Genesis (zero) parents are always finalized
            p.as_bytes().iter().all(|&b| b == 0) || finalized.contains(p)
        })
    }

    /// Calculate confidence score (0-100)
    fn calculate_confidence(
        &self,
        anchor_confirmations: u32,
        testimony_count: usize,
        parents_finalized: bool,
    ) -> u8 {
        let mut score = 0u8;

        // Anchor confirmations (up to 40 points)
        let anchor_score =
            (anchor_confirmations as u8 * 40 / self.config.min_anchor_confirmations as u8).min(40);
        score += anchor_score;

        // Testimony count (up to 40 points)
        let testimony_score =
            (testimony_count as u8 * 40 / self.config.min_testimonies as u8).min(40);
        score += testimony_score;

        // Parent finality (20 points)
        if parents_finalized {
            score += 20;
        }

        score.min(99) // Cap at 99 until truly final
    }

    /// Get finality state for a string
    pub fn get_state(&self, string_id: &StringId) -> Option<FinalityState> {
        self.strings
            .read()
            .get(string_id)
            .map(|info| info.state.clone())
    }

    /// Get full finality info
    pub fn get_info(&self, string_id: &StringId) -> Option<StringFinalityInfo> {
        self.strings.read().get(string_id).cloned()
    }

    /// Check if a string is finalized
    pub fn is_finalized(&self, string_id: &StringId) -> bool {
        self.finalized.read().contains(string_id)
    }

    /// Get finality statistics
    pub fn stats(&self) -> FinalityStats {
        let strings = self.strings.read();
        let mut pending = 0;
        let mut tentative = 0;
        let mut finalized = 0;
        let mut rejected = 0;

        for info in strings.values() {
            match &info.state {
                FinalityState::Pending => pending += 1,
                FinalityState::Tentative { .. } => tentative += 1,
                FinalityState::Final { .. } => finalized += 1,
                FinalityState::Rejected { .. } | FinalityState::Expired { .. } => rejected += 1,
            }
        }

        FinalityStats {
            total_strings: strings.len(),
            pending,
            tentative,
            finalized,
            rejected,
            anchor_count: self.anchors.read().len(),
            current_round: self.voting_state.read().current_round(),
        }
    }

    /// Process expired strings
    pub fn process_expirations(&self) {
        let now = chrono::Utc::now().timestamp();
        let timeout = self.config.finality_timeout_secs as i64;

        let mut strings = self.strings.write();
        for info in strings.values_mut() {
            if info.state.is_pending() || matches!(info.state, FinalityState::Tentative { .. }) {
                if now - info.created_at > timeout {
                    info.state = FinalityState::Expired { expired_at: now };
                }
            }
        }
    }

    /// Reject a string
    pub fn reject_string(&self, string_id: &StringId, reason: String) {
        let mut strings = self.strings.write();
        if let Some(info) = strings.get_mut(string_id) {
            info.state = FinalityState::Rejected {
                reason,
                rejected_at: chrono::Utc::now().timestamp(),
            };
        }
    }

    /// Get testimony collector reference
    pub fn testimony_collector(&self) -> &TestimonyCollector {
        &self.testimony_collector
    }

    /// Advance to next voting round
    pub fn advance_round(&self) {
        self.voting_state.write().advance_round();
    }
}

impl Default for FinalityEngine {
    fn default() -> Self {
        Self::new(FinalityConfig::default())
    }
}

/// Finality statistics
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalityStats {
    pub total_strings: usize,
    pub pending: usize,
    pub tentative: usize,
    pub finalized: usize,
    pub rejected: usize,
    pub anchor_count: usize,
    pub current_round: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finality_engine_creation() {
        let engine = FinalityEngine::new(FinalityConfig::default());
        let stats = engine.stats();

        assert_eq!(stats.total_strings, 0);
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.finalized, 0);
    }

    #[test]
    fn test_register_string() {
        let engine = FinalityEngine::new(FinalityConfig::default());
        let string_id = StringId::from_content(b"test");

        engine.register_string(string_id, vec![]);

        let state = engine.get_state(&string_id);
        assert!(state.is_some());
        assert!(state.unwrap().is_pending());
    }

    #[test]
    fn test_tentative_state() {
        let engine = FinalityEngine::new(FinalityConfig::default());
        let string_id = StringId::from_content(b"test");

        engine.register_string(string_id, vec![]);

        // Update with some testimonies
        engine.update_testimony_count(&string_id, 5);

        let state = engine.get_state(&string_id).unwrap();
        assert!(matches!(state, FinalityState::Tentative { .. }));
    }

    #[test]
    fn test_finality_achievement() {
        let mut config = FinalityConfig::default();
        config.min_anchor_confirmations = 1;
        config.min_testimonies = 1;
        config.require_parent_finality = false;

        let engine = FinalityEngine::new(config);
        let string_id = StringId::from_content(b"test");

        engine.register_string(string_id, vec![]);
        engine.update_testimony_count(&string_id, 5);

        // Record anchor that references this string
        engine.record_anchor([1u8; 32], 1, vec![string_id]);

        assert!(engine.is_finalized(&string_id));

        let state = engine.get_state(&string_id).unwrap();
        assert!(state.is_final());
    }

    #[test]
    fn test_parent_dependency() {
        let mut config = FinalityConfig::default();
        config.min_anchor_confirmations = 1;
        config.min_testimonies = 1;
        config.require_parent_finality = true;

        let engine = FinalityEngine::new(config);

        let parent_id = StringId::from_content(b"parent");
        let child_id = StringId::from_content(b"child");

        // Register parent first
        engine.register_string(parent_id, vec![]);

        // Register child with parent dependency
        engine.register_string(child_id, vec![parent_id]);
        engine.update_testimony_count(&child_id, 5);
        engine.record_anchor([1u8; 32], 1, vec![child_id]);

        // Child should not be finalized (parent not finalized)
        assert!(!engine.is_finalized(&child_id));

        // Finalize parent
        engine.update_testimony_count(&parent_id, 5);
        engine.record_anchor([2u8; 32], 2, vec![parent_id, child_id]);

        // Now both should be finalized
        assert!(engine.is_finalized(&parent_id));
        assert!(engine.is_finalized(&child_id));
    }

    #[test]
    fn test_rejection() {
        let engine = FinalityEngine::new(FinalityConfig::default());
        let string_id = StringId::from_content(b"test");

        engine.register_string(string_id, vec![]);
        engine.reject_string(&string_id, "Invalid content".to_string());

        let state = engine.get_state(&string_id).unwrap();
        assert!(state.is_rejected());
    }

    #[test]
    fn test_stats() {
        let engine = FinalityEngine::new(FinalityConfig::default());

        for i in 0..5 {
            let string_id = StringId::from_content(&[i as u8]);
            engine.register_string(string_id, vec![]);
        }

        let stats = engine.stats();
        assert_eq!(stats.total_strings, 5);
        assert_eq!(stats.pending, 5);
    }
}
