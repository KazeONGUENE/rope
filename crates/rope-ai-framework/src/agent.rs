//! The `DomainAgent` trait — the extension point for third-party AI agents.
//!
//! Any third-party developer who wants their AI model to run inside
//! Datachain Rope implements this trait and registers it via the framework.

use crate::scoring::DiagnosisResult;
use crate::types::{AgentCapability, AgentDomain, StringQuery};
use async_trait::async_trait;
use hashbrown::HashMap;
use serde_json::Value as JsonValue;

/// Input context provided to an agent when it runs.
#[derive(Clone, Debug)]
pub struct AgentInput {
    /// The wallet address whose String is being analyzed.
    pub target_wallet: String,

    /// Recent String fragments (telemetry, events, interactions) as JSON.
    pub fragments: Vec<JsonValue>,

    /// Optional parameters from the scheduler or manual trigger.
    pub parameters: HashMap<String, String>,

    /// Timestamp window: only fragments within this range.
    pub since_timestamp: Option<i64>,
    pub until_timestamp: Option<i64>,
}

/// Output returned by an agent after analysis.
#[derive(Clone, Debug)]
pub struct AgentOutput {
    pub diagnosis: DiagnosisResult,
    pub write_back: bool,
    pub additional_queries: Vec<StringQuery>,
}

/// The core trait that all domain-specific AI agents implement.
///
/// # Lifecycle
///
/// 1. `initialize()` — Called once when the agent is registered. Load models, warm caches.
/// 2. `analyze()` — Called by the scheduler with String fragments. Return diagnosis.
/// 3. `health_check()` — Periodic check to ensure the agent is functioning.
/// 4. `shutdown()` — Called when the agent is decommissioned.
///
/// # Example (third-party)
///
/// ```ignore
/// struct StreetLightMaintenanceAgent { /* domain model */ }
///
/// #[async_trait]
/// impl DomainAgent for StreetLightMaintenanceAgent {
///     fn agent_id(&self) -> &str { "streetlight-maint-v1" }
///     fn domain(&self) -> AgentDomain { AgentDomain::Maintenance }
///     fn capabilities(&self) -> Vec<AgentCapability> {
///         vec![AgentCapability::Diagnosis, AgentCapability::Prediction]
///     }
///     async fn analyze(&self, input: AgentInput) -> Result<AgentOutput, String> {
///         // Read telemetry fragments, run wear model, produce diagnosis
///     }
/// }
/// ```
#[async_trait]
pub trait DomainAgent: Send + Sync {
    /// Unique identifier for this agent instance.
    fn agent_id(&self) -> &str;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Semantic version (e.g., "1.0.0").
    fn version(&self) -> &str;

    /// Primary domain this agent operates in.
    fn domain(&self) -> AgentDomain;

    /// Capabilities this agent provides.
    fn capabilities(&self) -> Vec<AgentCapability>;

    /// Called once when the agent is registered with the framework.
    async fn initialize(&self) -> Result<(), String> {
        Ok(())
    }

    /// Run analysis on String fragments. This is the core intelligence method.
    async fn analyze(&self, input: AgentInput) -> Result<AgentOutput, String>;

    /// Periodic health check. Return Ok(()) if healthy, Err(reason) if not.
    async fn health_check(&self) -> Result<(), String> {
        Ok(())
    }

    /// Called when the agent is being decommissioned.
    async fn shutdown(&self) -> Result<(), String> {
        Ok(())
    }

    /// Optional: which interaction types does this agent want to read?
    /// If None, the framework delivers all fragment types.
    fn subscribed_interaction_types(&self) -> Option<Vec<String>> {
        None
    }

    /// Optional: minimum number of fragments before triggering analysis.
    fn min_fragment_count(&self) -> usize {
        1
    }

    /// Optional: how often (in seconds) the scheduler should run this agent
    /// on each subscribed wallet. 0 = trigger-based only.
    fn schedule_interval_secs(&self) -> u64 {
        0
    }
}
