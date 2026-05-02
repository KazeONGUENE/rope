//! Built-in agents bundled with Rope — MaintenanceAgent and AnomalyAgent.
//!
//! These demonstrate the DomainAgent trait and provide useful out-of-the-box
//! intelligence for common use cases.

use crate::agent::{AgentInput, AgentOutput, DomainAgent};
use crate::scoring::{
    ConfidenceScore, DiagnosisResult, EvidenceItem, Recommendation, RecommendationPriority,
    ScoringMethod, Severity,
};
use crate::types::{AgentCapability, AgentDomain};
use async_trait::async_trait;
use hashbrown::HashMap;

// ═══════════════════════════════════════════════════════════════════════════
// MaintenanceAgent — Predictive maintenance from IoT telemetry Strings
// ═══════════════════════════════════════════════════════════════════════════

pub struct MaintenanceAgent {
    id: String,
    thresholds: MaintenanceThresholds,
}

struct MaintenanceThresholds {
    temperature_high: f64,
    temperature_critical: f64,
    vibration_high: f64,
    vibration_critical: f64,
    humidity_high: f64,
    battery_low: f64,
    battery_critical: f64,
}

impl Default for MaintenanceThresholds {
    fn default() -> Self {
        Self {
            temperature_high: 70.0,
            temperature_critical: 85.0,
            vibration_high: 4.0,
            vibration_critical: 7.0,
            humidity_high: 85.0,
            battery_low: 20.0,
            battery_critical: 5.0,
        }
    }
}

impl MaintenanceAgent {
    pub fn new() -> Self {
        Self {
            id: "rope-maintenance-agent-v1".into(),
            thresholds: MaintenanceThresholds::default(),
        }
    }
}

impl Default for MaintenanceAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DomainAgent for MaintenanceAgent {
    fn agent_id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Predictive Maintenance Agent"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn domain(&self) -> AgentDomain {
        AgentDomain::Maintenance
    }

    fn capabilities(&self) -> Vec<AgentCapability> {
        vec![
            AgentCapability::Diagnosis,
            AgentCapability::Prediction,
            AgentCapability::Recommendation,
            AgentCapability::Alerting,
        ]
    }

    fn subscribed_interaction_types(&self) -> Option<Vec<String>> {
        Some(vec!["Custom".into()])
    }

    fn schedule_interval_secs(&self) -> u64 {
        300
    }

    fn min_fragment_count(&self) -> usize {
        5
    }

    async fn analyze(&self, input: AgentInput) -> Result<AgentOutput, String> {
        let mut evidence = Vec::new();
        let mut max_severity = Severity::Info;
        let mut issues = Vec::new();

        for fragment in &input.fragments {
            let ts = fragment
                .get("timestamp")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            if let Some(meta) = fragment.get("metadata").and_then(|v| v.as_object()) {
                if let Some(temp_str) = meta.get("reading_temperature").and_then(|v| v.as_str()) {
                    if let Ok(temp) = temp_str.parse::<f64>() {
                        if temp >= self.thresholds.temperature_critical {
                            max_severity = Severity::Critical;
                            issues.push("Critical temperature");
                            evidence.push(EvidenceItem {
                                source: "telemetry".into(),
                                reading_key: "temperature".into(),
                                value: format!("{}°C", temp),
                                expected_range: Some(format!(
                                    "<{}°C",
                                    self.thresholds.temperature_critical
                                )),
                                timestamp: ts,
                            });
                        } else if temp >= self.thresholds.temperature_high {
                            if max_severity == Severity::Info {
                                max_severity = Severity::Medium;
                            }
                            issues.push("High temperature");
                            evidence.push(EvidenceItem {
                                source: "telemetry".into(),
                                reading_key: "temperature".into(),
                                value: format!("{}°C", temp),
                                expected_range: Some(format!(
                                    "<{}°C",
                                    self.thresholds.temperature_high
                                )),
                                timestamp: ts,
                            });
                        }
                    }
                }

                if let Some(vib_str) = meta.get("reading_vibration").and_then(|v| v.as_str()) {
                    if let Ok(vib) = vib_str.parse::<f64>() {
                        if vib >= self.thresholds.vibration_critical {
                            max_severity = Severity::Critical;
                            issues.push("Critical vibration");
                            evidence.push(EvidenceItem {
                                source: "telemetry".into(),
                                reading_key: "vibration".into(),
                                value: format!("{} mm/s", vib),
                                expected_range: Some(format!(
                                    "<{} mm/s",
                                    self.thresholds.vibration_critical
                                )),
                                timestamp: ts,
                            });
                        } else if vib >= self.thresholds.vibration_high {
                            if max_severity == Severity::Info || max_severity == Severity::Low {
                                max_severity = Severity::Medium;
                            }
                            issues.push("High vibration");
                        }
                    }
                }

                if let Some(bat_str) = meta.get("reading_battery").and_then(|v| v.as_str()) {
                    if let Ok(bat) = bat_str.parse::<f64>() {
                        if bat <= self.thresholds.battery_critical {
                            if max_severity != Severity::Critical {
                                max_severity = Severity::High;
                            }
                            issues.push("Critical battery");
                            evidence.push(EvidenceItem {
                                source: "telemetry".into(),
                                reading_key: "battery".into(),
                                value: format!("{}%", bat),
                                expected_range: Some(format!(">{}%", self.thresholds.battery_low)),
                                timestamp: ts,
                            });
                        }
                    }
                }
            }
        }

        let confidence_value = if evidence.is_empty() { 0.95 } else { 0.85 };
        let description = if issues.is_empty() {
            "All readings within normal parameters".into()
        } else {
            format!("Issues detected: {}", issues.join(", "))
        };

        let mut recommendations = Vec::new();
        if max_severity == Severity::Critical {
            recommendations.push(Recommendation {
                action: "Schedule immediate inspection".into(),
                priority: RecommendationPriority::Immediate,
                rationale: description.clone(),
                estimated_impact: Some("Prevent equipment failure".into()),
                deadline: Some(chrono::Utc::now().timestamp() + 86400),
            });
        } else if max_severity == Severity::Medium || max_severity == Severity::High {
            recommendations.push(Recommendation {
                action: "Schedule maintenance within 7 days".into(),
                priority: RecommendationPriority::Scheduled,
                rationale: description.clone(),
                estimated_impact: Some("Extend equipment life".into()),
                deadline: Some(chrono::Utc::now().timestamp() + 7 * 86400),
            });
        }

        let diagnosis = DiagnosisResult {
            agent_id: self.id.clone(),
            target_wallet: input.target_wallet,
            diagnosis_type: "predictive_maintenance".into(),
            severity: max_severity,
            confidence: ConfidenceScore::new(confidence_value, ScoringMethod::ThresholdBased),
            description,
            evidence,
            recommendations,
            timestamp: chrono::Utc::now().timestamp(),
            metadata: HashMap::new(),
        };

        Ok(AgentOutput {
            diagnosis,
            write_back: true,
            additional_queries: Vec::new(),
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// AnomalyAgent — Statistical anomaly detection on any String
// ═══════════════════════════════════════════════════════════════════════════

pub struct AnomalyAgent {
    id: String,
    z_score_threshold: f64,
}

impl AnomalyAgent {
    pub fn new() -> Self {
        Self {
            id: "rope-anomaly-agent-v1".into(),
            z_score_threshold: 2.5,
        }
    }
}

impl Default for AnomalyAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DomainAgent for AnomalyAgent {
    fn agent_id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Statistical Anomaly Detection Agent"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn domain(&self) -> AgentDomain {
        AgentDomain::Security
    }

    fn capabilities(&self) -> Vec<AgentCapability> {
        vec![AgentCapability::AnomalyDetection, AgentCapability::Alerting]
    }

    fn schedule_interval_secs(&self) -> u64 {
        600
    }

    fn min_fragment_count(&self) -> usize {
        10
    }

    async fn analyze(&self, input: AgentInput) -> Result<AgentOutput, String> {
        let mut series: HashMap<String, Vec<f64>> = HashMap::new();

        for fragment in &input.fragments {
            if let Some(meta) = fragment.get("metadata").and_then(|v| v.as_object()) {
                for (key, val) in meta {
                    if key.starts_with("reading_") {
                        if let Some(v_str) = val.as_str() {
                            if let Ok(v) = v_str.parse::<f64>() {
                                series.entry(key.clone()).or_default().push(v);
                            }
                        }
                    }
                }
            }
        }

        let mut anomalies = Vec::new();
        for (key, values) in &series {
            if values.len() < 3 {
                continue;
            }
            let n = values.len() as f64;
            let mean = values.iter().sum::<f64>() / n;
            let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
            let std_dev = variance.sqrt();

            if std_dev > 0.0 {
                if let Some(&last) = values.last() {
                    let z_score = (last - mean).abs() / std_dev;
                    if z_score > self.z_score_threshold {
                        anomalies.push((key.clone(), last, mean, std_dev, z_score));
                    }
                }
            }
        }

        let severity = if anomalies.is_empty() {
            Severity::Info
        } else if anomalies.iter().any(|(_, _, _, _, z)| *z > 4.0) {
            Severity::High
        } else {
            Severity::Medium
        };

        let evidence: Vec<EvidenceItem> = anomalies
            .iter()
            .map(|(key, val, mean, std_dev, z)| EvidenceItem {
                source: "statistical_analysis".into(),
                reading_key: key.replace("reading_", ""),
                value: format!("{:.2} (z-score: {:.2})", val, z),
                expected_range: Some(format!("{:.2} ± {:.2}", mean, std_dev * 2.0)),
                timestamp: chrono::Utc::now().timestamp(),
            })
            .collect();

        let description = if anomalies.is_empty() {
            "No anomalies detected — all readings within statistical norms".into()
        } else {
            format!(
                "{} anomal{} detected in: {}",
                anomalies.len(),
                if anomalies.len() == 1 { "y" } else { "ies" },
                anomalies
                    .iter()
                    .map(|(k, _, _, _, _)| k.replace("reading_", ""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let recommendations = if !anomalies.is_empty() {
            vec![Recommendation {
                action: "Investigate anomalous readings".into(),
                priority: if severity == Severity::High {
                    RecommendationPriority::Immediate
                } else {
                    RecommendationPriority::Scheduled
                },
                rationale: description.clone(),
                estimated_impact: None,
                deadline: None,
            }]
        } else {
            Vec::new()
        };

        let confidence_value = if series.values().any(|v| v.len() >= 30) {
            0.90
        } else if series.values().any(|v| v.len() >= 10) {
            0.75
        } else {
            0.55
        };

        let diagnosis = DiagnosisResult {
            agent_id: self.id.clone(),
            target_wallet: input.target_wallet,
            diagnosis_type: "anomaly_detection".into(),
            severity,
            confidence: ConfidenceScore::new(confidence_value, ScoringMethod::Statistical),
            description,
            evidence,
            recommendations,
            timestamp: chrono::Utc::now().timestamp(),
            metadata: HashMap::new(),
        };

        Ok(AgentOutput {
            diagnosis,
            write_back: !anomalies.is_empty(),
            additional_queries: Vec::new(),
        })
    }
}
