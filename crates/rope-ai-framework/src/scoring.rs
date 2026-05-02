//! Confidence scoring and diagnosis result types.

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

/// A confidence score between 0.0 and 1.0.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfidenceScore {
    pub value: f64,
    pub method: ScoringMethod,
}

impl ConfidenceScore {
    pub fn new(value: f64, method: ScoringMethod) -> Self {
        Self {
            value: value.clamp(0.0, 1.0),
            method,
        }
    }

    pub fn is_high(&self) -> bool {
        self.value >= 0.8
    }

    pub fn is_acceptable(&self) -> bool {
        self.value >= 0.5
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ScoringMethod {
    Statistical,
    RuleBased,
    ModelInference,
    Ensemble,
    ThresholdBased,
}

/// The result of an agent's analysis of a device/user String.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosisResult {
    pub agent_id: String,
    pub target_wallet: String,
    pub diagnosis_type: String,
    pub severity: Severity,
    pub confidence: ConfidenceScore,
    pub description: String,
    pub evidence: Vec<EvidenceItem>,
    pub recommendations: Vec<Recommendation>,
    pub timestamp: i64,
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// A piece of evidence supporting a diagnosis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub source: String,
    pub reading_key: String,
    pub value: String,
    pub expected_range: Option<String>,
    pub timestamp: i64,
}

/// An actionable recommendation produced by an agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recommendation {
    pub action: String,
    pub priority: RecommendationPriority,
    pub rationale: String,
    pub estimated_impact: Option<String>,
    pub deadline: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecommendationPriority {
    Immediate,
    Scheduled,
    NextMaintenance,
    Advisory,
}
