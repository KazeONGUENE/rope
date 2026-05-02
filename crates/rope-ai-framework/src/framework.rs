//! Agent Framework — lifecycle management, scheduling, and orchestration
//! for all registered DomainAgents.

use crate::agent::{AgentInput, DomainAgent};
use crate::builtin::{AnomalyAgent, MaintenanceAgent};
use crate::scoring::DiagnosisResult;
use crate::types::{AgentDescriptor, AgentDomain, AgentState};
use hashbrown::HashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Callback: the node provides a closure that writes diagnosis results
/// back to the target wallet's String.
pub type DiagnosisSink = Arc<
    dyn Fn(String, String, String, HashMap<String, String>) -> Result<(), String> + Send + Sync,
>;

/// Callback: the node provides a closure that reads recent fragments
/// from a wallet's String for agent analysis.
pub type FragmentReader = Arc<
    dyn Fn(String, Option<i64>, Option<usize>) -> Result<Vec<serde_json::Value>, String>
        + Send
        + Sync,
>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentFrameworkConfig {
    pub enabled: bool,
    pub builtin_maintenance_agent: bool,
    pub builtin_anomaly_agent: bool,
    pub max_agents: usize,
    pub scheduler_interval_secs: u64,
    pub max_concurrent_analyses: usize,
}

impl Default for AgentFrameworkConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            builtin_maintenance_agent: true,
            builtin_anomaly_agent: true,
            max_agents: 100,
            scheduler_interval_secs: 60,
            max_concurrent_analyses: 10,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FrameworkStats {
    pub agents_registered: usize,
    pub agents_active: usize,
    pub total_analyses: u64,
    pub total_diagnoses_written: u64,
    pub avg_confidence: f64,
    pub last_run_at: Option<i64>,
    pub uptime_secs: u64,
}

struct RegisteredAgent {
    agent: Arc<dyn DomainAgent>,
    descriptor: AgentDescriptor,
    subscribed_wallets: Vec<String>,
}

/// The Agent Framework runtime.
pub struct AgentFramework {
    config: AgentFrameworkConfig,
    agents: RwLock<HashMap<String, RegisteredAgent>>,
    sink: Option<DiagnosisSink>,
    reader: Option<FragmentReader>,
    stats: Arc<RwLock<FrameworkStats>>,
    started_at: i64,
    diagnosis_log: RwLock<Vec<DiagnosisResult>>,
}

impl AgentFramework {
    pub fn new(config: AgentFrameworkConfig) -> Self {
        Self {
            config,
            agents: RwLock::new(HashMap::new()),
            sink: None,
            reader: None,
            stats: Arc::new(RwLock::new(FrameworkStats::default())),
            started_at: chrono::Utc::now().timestamp(),
            diagnosis_log: RwLock::new(Vec::new()),
        }
    }

    pub fn set_sink(&mut self, sink: DiagnosisSink) {
        self.sink = Some(sink);
    }

    pub fn set_reader(&mut self, reader: FragmentReader) {
        self.reader = Some(reader);
    }

    /// Register the built-in agents (MaintenanceAgent + AnomalyAgent).
    pub async fn register_builtins(&self) -> Result<(), String> {
        if self.config.builtin_maintenance_agent {
            let agent = Arc::new(MaintenanceAgent::new());
            self.register(agent).await?;
        }
        if self.config.builtin_anomaly_agent {
            let agent = Arc::new(AnomalyAgent::new());
            self.register(agent).await?;
        }
        Ok(())
    }

    /// Register a DomainAgent (built-in or third-party).
    pub async fn register(&self, agent: Arc<dyn DomainAgent>) -> Result<String, String> {
        let agents = self.agents.read();
        if agents.len() >= self.config.max_agents {
            return Err(format!("Agent limit reached ({})", self.config.max_agents));
        }
        let id = agent.agent_id().to_string();
        if agents.contains_key(&id) {
            return Err(format!("Agent {} already registered", id));
        }
        drop(agents);

        agent.initialize().await?;

        let descriptor = AgentDescriptor {
            agent_id: id.clone(),
            name: agent.name().to_string(),
            version: agent.version().to_string(),
            domain: agent.domain(),
            capabilities: agent.capabilities(),
            owner: "rope-native".into(),
            description: format!("{} v{}", agent.name(), agent.version()),
            state: AgentState::Active,
            registered_at: chrono::Utc::now().timestamp(),
            last_run_at: None,
            run_count: 0,
            avg_confidence: 0.0,
            error_count: 0,
            config: HashMap::new(),
        };

        let registered = RegisteredAgent {
            agent,
            descriptor: descriptor.clone(),
            subscribed_wallets: Vec::new(),
        };

        self.agents.write().insert(id.clone(), registered);
        tracing::info!(
            "AI agent registered: {} ({:?})",
            descriptor.name,
            descriptor.domain
        );

        self.update_stats();
        Ok(id)
    }

    /// Subscribe an agent to analyze a specific wallet's String.
    pub fn subscribe_agent_to_wallet(&self, agent_id: &str, wallet: &str) -> Result<(), String> {
        let mut agents = self.agents.write();
        let reg = agents
            .get_mut(agent_id)
            .ok_or_else(|| format!("Agent {} not found", agent_id))?;
        if !reg.subscribed_wallets.contains(&wallet.to_string()) {
            reg.subscribed_wallets.push(wallet.to_string());
        }
        Ok(())
    }

    /// Run a single agent against a specific wallet.
    pub async fn run_agent(&self, agent_id: &str, wallet: &str) -> Result<DiagnosisResult, String> {
        let reader = self
            .reader
            .as_ref()
            .ok_or("Fragment reader not configured")?;

        let agent = {
            let agents = self.agents.read();
            let reg = agents
                .get(agent_id)
                .ok_or_else(|| format!("Agent {} not found", agent_id))?;
            reg.agent.clone()
        };

        let since = Some(chrono::Utc::now().timestamp() - 3600);
        let fragments = reader(wallet.to_string(), since, Some(100))?;

        if fragments.len() < agent.min_fragment_count() {
            return Err(format!(
                "Insufficient fragments ({} < {})",
                fragments.len(),
                agent.min_fragment_count()
            ));
        }

        let input = AgentInput {
            target_wallet: wallet.to_string(),
            fragments,
            parameters: HashMap::new(),
            since_timestamp: since,
            until_timestamp: None,
        };

        let output = agent.analyze(input).await?;

        {
            let mut agents = self.agents.write();
            if let Some(reg) = agents.get_mut(agent_id) {
                reg.descriptor.last_run_at = Some(chrono::Utc::now().timestamp());
                reg.descriptor.run_count += 1;
                let n = reg.descriptor.run_count as f64;
                reg.descriptor.avg_confidence = (reg.descriptor.avg_confidence * (n - 1.0)
                    + output.diagnosis.confidence.value)
                    / n;
            }
        }

        if output.write_back {
            if let Some(sink) = &self.sink {
                let mut meta = HashMap::new();
                meta.insert("agent_id".into(), agent_id.to_string());
                meta.insert(
                    "diagnosis_type".into(),
                    output.diagnosis.diagnosis_type.clone(),
                );
                meta.insert("severity".into(), output.diagnosis.severity.as_str().into());
                meta.insert(
                    "confidence".into(),
                    format!("{:.2}", output.diagnosis.confidence.value),
                );
                if !output.diagnosis.recommendations.is_empty() {
                    meta.insert(
                        "recommendation".into(),
                        output.diagnosis.recommendations[0].action.clone(),
                    );
                }
                let _ = sink(
                    wallet.to_string(),
                    "Custom".into(),
                    format!("AI Diagnosis: {}", output.diagnosis.description),
                    meta,
                );
                self.stats.write().total_diagnoses_written += 1;
            }
        }

        self.diagnosis_log.write().push(output.diagnosis.clone());
        self.stats.write().total_analyses += 1;
        self.update_stats();

        Ok(output.diagnosis)
    }

    /// List all registered agents.
    pub fn list_agents(&self) -> Vec<AgentDescriptor> {
        self.agents
            .read()
            .values()
            .map(|r| r.descriptor.clone())
            .collect()
    }

    /// Get a specific agent's descriptor.
    pub fn get_agent(&self, agent_id: &str) -> Option<AgentDescriptor> {
        self.agents
            .read()
            .get(agent_id)
            .map(|r| r.descriptor.clone())
    }

    /// Get recent diagnosis results.
    pub fn recent_diagnoses(&self, limit: usize) -> Vec<DiagnosisResult> {
        let log = self.diagnosis_log.read();
        log.iter().rev().take(limit).cloned().collect()
    }

    pub fn stats(&self) -> FrameworkStats {
        let mut s = self.stats.read().clone();
        s.uptime_secs = (chrono::Utc::now().timestamp() - self.started_at) as u64;
        s
    }

    pub fn agent_count(&self) -> usize {
        self.agents.read().len()
    }

    fn update_stats(&self) {
        let agents = self.agents.read();
        let mut s = self.stats.write();
        s.agents_registered = agents.len();
        s.agents_active = agents
            .values()
            .filter(|r| r.descriptor.state == AgentState::Active)
            .count();
        if !agents.is_empty() {
            s.avg_confidence = agents
                .values()
                .map(|r| r.descriptor.avg_confidence)
                .sum::<f64>()
                / agents.len() as f64;
        }
        s.last_run_at = agents
            .values()
            .filter_map(|r| r.descriptor.last_run_at)
            .max();
    }

    /// Start the scheduler loop.
    pub async fn start_scheduler(self: Arc<Self>) {
        let interval = self.config.scheduler_interval_secs;
        if interval == 0 {
            tracing::info!("AI Agent scheduler disabled (interval=0)");
            return;
        }

        tracing::info!(
            "AI Agent scheduler started (interval={}s, {} agents)",
            interval,
            self.agent_count()
        );

        tokio::spawn(async move {
            let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(interval));
            loop {
                tick.tick().await;

                let work: Vec<(String, Vec<String>)> = {
                    let agents = self.agents.read();
                    agents
                        .iter()
                        .filter(|(_, r)| {
                            r.descriptor.state == AgentState::Active
                                && !r.subscribed_wallets.is_empty()
                                && r.agent.schedule_interval_secs() > 0
                        })
                        .map(|(id, r)| (id.clone(), r.subscribed_wallets.clone()))
                        .collect()
                };

                for (agent_id, wallets) in work {
                    for wallet in wallets {
                        match self.run_agent(&agent_id, &wallet).await {
                            Ok(diagnosis) => {
                                tracing::debug!(
                                    "Agent {} analyzed {} → {:?} (confidence: {:.2})",
                                    agent_id,
                                    wallet,
                                    diagnosis.severity,
                                    diagnosis.confidence.value
                                );
                            }
                            Err(e) => {
                                tracing::warn!("Agent {} failed on {}: {}", agent_id, wallet, e);
                                let mut agents = self.agents.write();
                                if let Some(reg) = agents.get_mut(&agent_id) {
                                    reg.descriptor.error_count += 1;
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}

impl Default for AgentFramework {
    fn default() -> Self {
        Self::new(AgentFrameworkConfig::default())
    }
}
