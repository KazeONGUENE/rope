//! Shared types for the AI Agent Framework.

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

/// Domain categories that agents can specialize in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentDomain {
    Maintenance,
    Energy,
    Traffic,
    Environmental,
    SupplyChain,
    Healthcare,
    Agriculture,
    Security,
    Finance,
    Custom(String),
}

impl AgentDomain {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Maintenance => "maintenance",
            Self::Energy => "energy",
            Self::Traffic => "traffic",
            Self::Environmental => "environmental",
            Self::SupplyChain => "supply_chain",
            Self::Healthcare => "healthcare",
            Self::Agriculture => "agriculture",
            Self::Security => "security",
            Self::Finance => "finance",
            Self::Custom(s) => s,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "maintenance" => Self::Maintenance,
            "energy" => Self::Energy,
            "traffic" => Self::Traffic,
            "environmental" => Self::Environmental,
            "supply_chain" => Self::SupplyChain,
            "healthcare" => Self::Healthcare,
            "agriculture" => Self::Agriculture,
            "security" => Self::Security,
            "finance" => Self::Finance,
            other => Self::Custom(other.to_string()),
        }
    }
}

/// Capabilities that an agent declares during registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentCapability {
    Diagnosis,
    Prediction,
    AnomalyDetection,
    Optimization,
    Recommendation,
    Alerting,
    Scheduling,
    Custom(String),
}

impl AgentCapability {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Diagnosis => "diagnosis",
            Self::Prediction => "prediction",
            Self::AnomalyDetection => "anomaly_detection",
            Self::Optimization => "optimization",
            Self::Recommendation => "recommendation",
            Self::Alerting => "alerting",
            Self::Scheduling => "scheduling",
            Self::Custom(s) => s,
        }
    }
}

/// Metadata describing a registered agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub agent_id: String,
    pub name: String,
    pub version: String,
    pub domain: AgentDomain,
    pub capabilities: Vec<AgentCapability>,
    pub owner: String,
    pub description: String,
    pub state: AgentState,
    pub registered_at: i64,
    pub last_run_at: Option<i64>,
    pub run_count: u64,
    pub avg_confidence: f64,
    pub error_count: u64,
    pub config: HashMap<String, String>,
}

/// Lifecycle state of a registered agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    Registered,
    Active,
    Running,
    Paused,
    Failed,
    Decommissioned,
}

impl AgentState {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Registered => "registered",
            Self::Active => "active",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Failed => "failed",
            Self::Decommissioned => "decommissioned",
        }
    }
}

/// Query to read String fragments for agent analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StringQuery {
    pub wallet_address: String,
    pub interaction_types: Option<Vec<String>>,
    pub since_timestamp: Option<i64>,
    pub limit: Option<usize>,
}
