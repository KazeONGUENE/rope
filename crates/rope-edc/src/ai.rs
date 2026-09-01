//! AI analytics orchestration - spec v2.0 §6.
//!
//! Design principle: **the AI never invents numbers.** Every question is
//! answered in two stages:
//!
//! 1. The deterministic [`crate::analytics`] catalogue (descriptive
//!    statistics, time-series, anomaly detection, forecasting,
//!    correlation, distribution, clustering, cohort comparison,
//!    predictive-maintenance reliability, compliance, data quality) is
//!    executed over the scoped readings. This produces the **analytics
//!    dossier** - the complete quantitative picture, computed locally on
//!    the project's own node.
//! 2. If an AI provider is configured (Alteros orchestrator routing across
//!    Ollama / Anthropic Claude / OpenAI), the dossier + question are sent
//!    for narration and chart selection. The response is returned WITH the
//!    dossier attached as grounding, so every AI statement traces back to
//!    a deterministic computation over on-chain-anchored data.
//!
//! With no provider configured (`EDC_AI_DISABLE=1` or no keys), the
//! deterministic engine answers alone: the dossier is rendered through the
//! template narrator and charts are selected by data-shape heuristics.
//! The dashboard is fully functional in sovereign / air-gapped mode.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use rope_agent_runtime::ai::{AIModelConfig, AIProvider, ChatMessage, CompletionRequest};

use crate::analytics::{self, Sample};
use crate::types::{SensorRecord, TelemetryReading};

/// One provenance reference attached to an AI (or deterministic) answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grounding {
    /// Which analytics method produced the figure (e.g. `holt_winters`).
    pub method: String,
    /// The computed result, verbatim.
    pub result: serde_json::Value,
}

/// Declarative chart specification rendered by the dashboard's own SVG
/// engine. The model returns THIS shape, never markup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartSpec {
    /// line | bar | donut | gauge | scatter | heatmap
    pub chart: String,
    pub title: String,
    /// X-axis labels (time buckets or category names).
    pub x: Vec<String>,
    /// One or more named series.
    pub series: Vec<ChartSeries>,
    /// Optional y-axis unit label.
    #[serde(default)]
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartSeries {
    pub name: String,
    pub values: Vec<f64>,
}

/// The full response returned to `/ask` and `/chart` callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsAnswer {
    pub answer: String,
    pub charts: Vec<ChartSpec>,
    pub grounding: Vec<Grounding>,
    /// `alteros`, `anthropic`, `openai`, `ollama`, or `deterministic`.
    pub engine: String,
    pub latency_ms: u64,
}

// ---------------------------------------------------------------------------
// The analytics dossier - every known method, computed deterministically
// ---------------------------------------------------------------------------

/// Compute the complete analytics dossier over a set of readings scoped to
/// one parameter (or a small set of parameters).
pub fn build_dossier(
    readings: &[TelemetryReading],
    sensors: &[SensorRecord],
    now: i64,
) -> Vec<Grounding> {
    let mut out: Vec<Grounding> = Vec::new();
    if readings.is_empty() {
        return out;
    }

    // Group readings by parameter so multi-parameter scopes stay coherent.
    let mut by_param: std::collections::BTreeMap<String, Vec<&TelemetryReading>> =
        Default::default();
    for r in readings {
        by_param.entry(r.parameter.clone()).or_default().push(r);
    }

    // Per-parameter single-series analytics.
    let mut bucketed_for_corr: Vec<(String, Vec<f64>)> = Vec::new();
    for (param, rows) in &by_param {
        let mut series: Vec<Sample> = rows.iter().map(|r| (r.ts, r.value)).collect();
        analytics::sort_series(&mut series);
        let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
        let bands: Vec<String> = rows.iter().map(|r| r.band.clone()).collect();

        // 1. Descriptive statistics.
        if let Some(stats) = analytics::describe(&values) {
            out.push(Grounding {
                method: format!("descriptive_stats:{param}"),
                result: serde_json::to_value(&stats).unwrap_or_default(),
            });
        }

        // 2. Trend + rate of change.
        if let Some(trend) = analytics::linear_trend(&series) {
            out.push(Grounding {
                method: format!("linear_trend:{param}"),
                result: serde_json::to_value(&trend).unwrap_or_default(),
            });
        }
        if let Some(roc) = analytics::rate_of_change_per_hour(&series) {
            out.push(Grounding {
                method: format!("rate_of_change_per_hour:{param}"),
                result: serde_json::json!(roc),
            });
        }

        // 3. Moving averages (recent smoothing snapshot).
        let w = (values.len() / 10).clamp(2, 24);
        let sma = analytics::sma(&values, w);
        if let Some(last_sma) = sma.last() {
            out.push(Grounding {
                method: format!("sma{w}_latest:{param}"),
                result: serde_json::json!(last_sma),
            });
        }
        if let Some(last_ema) = analytics::ema(&values, 0.3).last() {
            out.push(Grounding {
                method: format!("ema0.3_latest:{param}"),
                result: serde_json::json!(last_ema),
            });
        }

        // 4. Anomaly detection - all four detectors.
        let z = analytics::zscore_anomalies(&series, 3.0);
        let mad = analytics::mad_anomalies(&series, 3.5);
        let iqr = analytics::iqr_anomalies(&series, 1.5);
        let ewma = analytics::ewma_control_anomalies(&series, 0.2, 3.0);
        let cusum = analytics::cusum_drift(&series, 0.5, 5.0);
        out.push(Grounding {
            method: format!("anomaly_summary:{param}"),
            result: serde_json::json!({
                "zscore": z.len(),
                "mad": mad.len(),
                "iqr": iqr.len(),
                "ewma_control": ewma.len(),
                "cusum_drifts": cusum.len(),
                "latest": mad.last().or(z.last()).map(|a| serde_json::to_value(a).unwrap_or_default()),
                "latest_drift": cusum.last().map(|d| serde_json::to_value(d).unwrap_or_default()),
            }),
        });

        // 5. Seasonality + decomposition.
        let season = analytics::detect_seasonality(&values, values.len() / 2);
        if let Some(s) = &season {
            out.push(Grounding {
                method: format!("seasonality:{param}"),
                result: serde_json::to_value(s).unwrap_or_default(),
            });
        }

        // 6. Forecasting - linear always; Holt & Holt-Winters when enough data.
        let step = median_step_secs(&series).unwrap_or(3600);
        if let Some(f) = analytics::forecast_linear(&series, 12, step) {
            out.push(Grounding {
                method: format!("forecast_linear:{param}"),
                result: serde_json::to_value(&f).unwrap_or_default(),
            });
        }
        if let Some(f) = analytics::forecast_holt(&series, 0.5, 0.3, 12, step) {
            out.push(Grounding {
                method: format!("forecast_holt:{param}"),
                result: serde_json::to_value(&f).unwrap_or_default(),
            });
        }
        if let Some(s) = &season {
            if let Some(f) = analytics::forecast_holt_winters(
                &series, s.period, 0.5, 0.3, 0.3, s.period.min(24), step,
            ) {
                out.push(Grounding {
                    method: format!("forecast_holt_winters:{param}"),
                    result: serde_json::to_value(&f).unwrap_or_default(),
                });
            }
        }

        // 7. Distribution.
        out.push(Grounding {
            method: format!("histogram:{param}"),
            result: serde_json::to_value(analytics::histogram(&values, 12))
                .unwrap_or_default(),
        });
        if let Some(nrm) = analytics::normality(&values) {
            out.push(Grounding {
                method: format!("normality_jarque_bera:{param}"),
                result: serde_json::to_value(&nrm).unwrap_or_default(),
            });
        }

        // 8. Segmentation.
        if values.len() >= 6 {
            if let Some(km) = analytics::kmeans_1d(&values, 3.min(values.len()), 50) {
                out.push(Grounding {
                    method: format!("kmeans_centroids:{param}"),
                    result: serde_json::json!(km.centroids),
                });
            }
        }

        // 9. Cohort: period-over-period around the window midpoint.
        if let (Some(first), Some(last)) = (series.first(), series.last()) {
            let mid = (first.0 + last.0) / 2;
            if let Some(pop) = analytics::period_over_period(&series, mid) {
                out.push(Grounding {
                    method: format!("period_over_period:{param}"),
                    result: serde_json::to_value(&pop).unwrap_or_default(),
                });
            }
        }

        // 10. Predictive maintenance: RUL against the sensor's critical band.
        let sensor = sensors.iter().find(|s| s.parameter == *param);
        if let Some(sensor) = sensor {
            let threshold = sensor
                .warning
                .map(|[_, hi]| hi)
                .or(sensor.range.map(|[_, hi]| hi));
            if let Some(th) = threshold {
                if let Some(rul) = analytics::remaining_useful_life(&series, th) {
                    out.push(Grounding {
                        method: format!("remaining_useful_life:{param}"),
                        result: serde_json::to_value(&rul).unwrap_or_default(),
                    });
                }
            }
            // 11. Cadence conformity + data quality.
            let interval = cadence_to_secs(&sensor.cadence, sensor.readings_per_hour);
            if let Some(first) = series.first() {
                if let Some(cc) =
                    analytics::cadence_conformity(&series, interval, first.0, now)
                {
                    out.push(Grounding {
                        method: format!("cadence_conformity:{param}"),
                        result: serde_json::to_value(&cc).unwrap_or_default(),
                    });
                }
                if let Some(dq) = analytics::data_quality(&series, interval, first.0, now) {
                    out.push(Grounding {
                        method: format!("data_quality:{param}"),
                        result: serde_json::to_value(&dq).unwrap_or_default(),
                    });
                }
            }
        }

        // 12. Compliance report against the declared bands.
        out.push(Grounding {
            method: format!("compliance:{param}"),
            result: serde_json::to_value(analytics::compliance_report(&bands, 95.0))
                .unwrap_or_default(),
        });

        // Keep an hourly-bucketed mean series for the correlation matrix.
        let buckets = analytics::resample(&series, 3600);
        bucketed_for_corr.push((
            param.clone(),
            buckets.iter().map(|b| b.mean).collect(),
        ));
    }

    // 13. Cross-parameter correlation matrix (Pearson + Spearman).
    if bucketed_for_corr.len() >= 2 {
        let matrix = analytics::correlation_matrix(&bucketed_for_corr);
        if !matrix.is_empty() {
            out.push(Grounding {
                method: "correlation_matrix".to_string(),
                result: serde_json::to_value(&matrix).unwrap_or_default(),
            });
        }
    }

    // 14. Group-by across assets (which asset runs hottest / driest / …).
    let asset_rows: Vec<(String, f64)> = readings
        .iter()
        .map(|r| (r.asset_id.clone(), r.value))
        .collect();
    let groups = analytics::group_by(&asset_rows);
    if groups.len() >= 2 {
        out.push(Grounding {
            method: "group_by_asset".to_string(),
            result: serde_json::to_value(analytics::top_n(groups, "mean", 10, true))
                .unwrap_or_default(),
        });
    }

    out
}

fn median_step_secs(series: &[Sample]) -> Option<i64> {
    if series.len() < 2 {
        return None;
    }
    let mut gaps: Vec<i64> = series.windows(2).map(|w| w[1].0 - w[0].0).collect();
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2].max(1))
}

fn cadence_to_secs(cadence: &str, readings_per_hour: f64) -> i64 {
    if readings_per_hour > 0.0 {
        return (3600.0 / readings_per_hour) as i64;
    }
    match cadence {
        c if c.contains("min") => {
            let n: i64 = c
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(6);
            n.max(1) * 60
        }
        "hourly" => 3600,
        "daily" => 86_400,
        _ => 3600,
    }
}

// ---------------------------------------------------------------------------
// Deterministic chart selection (data-shape heuristics)
// ---------------------------------------------------------------------------

/// Select charts from the data shape: time-series → line, per-asset
/// comparison → bar, band share → donut, single KPI → gauge,
/// two-parameter relationship → scatter.
pub fn deterministic_charts(
    readings: &[TelemetryReading],
    dossier: &[Grounding],
) -> Vec<ChartSpec> {
    let mut charts = Vec::new();
    if readings.is_empty() {
        return charts;
    }

    // Line chart: hourly mean per parameter.
    let mut by_param: std::collections::BTreeMap<String, Vec<Sample>> = Default::default();
    for r in readings {
        by_param.entry(r.parameter.clone()).or_default().push((r.ts, r.value));
    }
    for (param, mut series) in by_param.clone() {
        analytics::sort_series(&mut series);
        let buckets = analytics::resample(&series, 3600);
        if buckets.len() >= 2 {
            charts.push(ChartSpec {
                chart: "line".to_string(),
                title: format!("{param} - hourly mean"),
                x: buckets
                    .iter()
                    .map(|b| {
                        chrono::DateTime::from_timestamp(b.ts_start, 0)
                            .map(|d| d.format("%m-%d %H:%M").to_string())
                            .unwrap_or_default()
                    })
                    .collect(),
                series: vec![ChartSeries {
                    name: param.clone(),
                    values: buckets.iter().map(|b| b.mean).collect(),
                }],
                unit: readings
                    .iter()
                    .find(|r| r.parameter == param)
                    .map(|r| r.unit.clone())
                    .unwrap_or_default(),
            });
        }
    }

    // Bar chart: mean per asset (top 10) from the group_by grounding.
    if let Some(g) = dossier.iter().find(|g| g.method == "group_by_asset") {
        if let Ok(groups) =
            serde_json::from_value::<Vec<analytics::GroupStat>>(g.result.clone())
        {
            if groups.len() >= 2 {
                charts.push(ChartSpec {
                    chart: "bar".to_string(),
                    title: "Mean value by asset (top 10)".to_string(),
                    x: groups.iter().map(|s| s.group.clone()).collect(),
                    series: vec![ChartSeries {
                        name: "mean".to_string(),
                        values: groups.iter().map(|s| s.mean).collect(),
                    }],
                    unit: String::new(),
                });
            }
        }
    }

    // Donut: band share.
    let ok = readings.iter().filter(|r| r.band == "ok").count() as f64;
    let warn = readings.iter().filter(|r| r.band == "warning").count() as f64;
    let crit = readings.iter().filter(|r| r.band == "critical").count() as f64;
    charts.push(ChartSpec {
        chart: "donut".to_string(),
        title: "Readings by band".to_string(),
        x: vec!["ok".into(), "warning".into(), "critical".into()],
        series: vec![ChartSeries {
            name: "readings".to_string(),
            values: vec![ok, warn, crit],
        }],
        unit: String::new(),
    });

    // Gauge: overall in-optimum percentage.
    let total = ok + warn + crit;
    if total > 0.0 {
        charts.push(ChartSpec {
            chart: "gauge".to_string(),
            title: "In-optimum share".to_string(),
            x: vec![],
            series: vec![ChartSeries {
                name: "pct".to_string(),
                values: vec![ok / total * 100.0],
            }],
            unit: "%".to_string(),
        });
    }

    // Scatter: first correlated pair from the matrix.
    if let Some(g) = dossier.iter().find(|g| g.method == "correlation_matrix") {
        if let Ok(matrix) =
            serde_json::from_value::<Vec<analytics::CorrelationEntry>>(g.result.clone())
        {
            if let Some(strongest) = matrix.iter().max_by(|a, b| {
                a.pearson
                    .abs()
                    .partial_cmp(&b.pearson.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                let sa = by_param.get(&strongest.a);
                let sb = by_param.get(&strongest.b);
                if let (Some(sa), Some(sb)) = (sa, sb) {
                    let ba = analytics::resample(sa, 3600);
                    let bb = analytics::resample(sb, 3600);
                    let n = ba.len().min(bb.len());
                    if n >= 3 {
                        charts.push(ChartSpec {
                            chart: "scatter".to_string(),
                            title: format!(
                                "{} vs {} (Pearson {:.2})",
                                strongest.a, strongest.b, strongest.pearson
                            ),
                            x: ba[..n].iter().map(|b| format!("{:.2}", b.mean)).collect(),
                            series: vec![ChartSeries {
                                name: strongest.b.clone(),
                                values: bb[..n].iter().map(|b| b.mean).collect(),
                            }],
                            unit: String::new(),
                        });
                    }
                }
            }
        }
    }

    charts
}

/// Deterministic narration: renders the dossier's headline figures as
/// plain language, used when no AI provider is configured and as the
/// factual skeleton the AI narration is asked to stay within.
pub fn deterministic_narrative(dossier: &[Grounding]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for g in dossier {
        let (method, param) = g
            .method
            .split_once(':')
            .map(|(m, p)| (m, p))
            .unwrap_or((g.method.as_str(), ""));
        match method {
            "descriptive_stats" => {
                if let (Some(mean), Some(sd), Some(n)) = (
                    g.result.get("mean").and_then(|v| v.as_f64()),
                    g.result.get("stddev").and_then(|v| v.as_f64()),
                    g.result.get("count").and_then(|v| v.as_u64()),
                ) {
                    lines.push(format!(
                        "{param}: {n} readings, mean {mean:.2} (σ {sd:.2})."
                    ));
                }
            }
            "linear_trend" => {
                if let (Some(dir), Some(sph), Some(r2)) = (
                    g.result.get("direction").and_then(|v| v.as_str()),
                    g.result.get("slope_per_hour").and_then(|v| v.as_f64()),
                    g.result.get("r_squared").and_then(|v| v.as_f64()),
                ) {
                    lines.push(format!(
                        "{param} trend is {dir} at {sph:.4}/hour (R² {r2:.2})."
                    ));
                }
            }
            "anomaly_summary" => {
                let mad = g.result.get("mad").and_then(|v| v.as_u64()).unwrap_or(0);
                let drifts = g
                    .result
                    .get("cusum_drifts")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if mad > 0 || drifts > 0 {
                    lines.push(format!(
                        "{param}: {mad} robust outliers and {drifts} CUSUM drift signals detected."
                    ));
                }
            }
            "remaining_useful_life" => {
                if let Some(h) = g.result.get("hours_remaining").and_then(|v| v.as_f64()) {
                    lines.push(format!(
                        "{param}: projected to cross its critical threshold in {h:.0} hours at the current degradation rate."
                    ));
                }
            }
            "compliance" => {
                if let (Some(pct), Some(met)) = (
                    g.result.get("in_optimum_pct").and_then(|v| v.as_f64()),
                    g.result.get("sla_met").and_then(|v| v.as_bool()),
                ) {
                    lines.push(format!(
                        "{param}: {pct:.1}% of readings in the optimal band - SLA {}.",
                        if met { "met" } else { "NOT met" }
                    ));
                }
            }
            "data_quality" => {
                if let (Some(c), Some(stale)) = (
                    g.result.get("completeness_pct").and_then(|v| v.as_f64()),
                    g.result.get("staleness_secs").and_then(|v| v.as_i64()),
                ) {
                    lines.push(format!(
                        "{param}: data completeness {c:.0}%, last reading {stale}s ago."
                    ));
                }
            }
            "correlation_matrix" => {
                if let Ok(matrix) = serde_json::from_value::<Vec<analytics::CorrelationEntry>>(
                    g.result.clone(),
                ) {
                    for e in matrix.iter().filter(|e| e.pearson.abs() > 0.6) {
                        lines.push(format!(
                            "{} and {} are strongly correlated (Pearson {:.2}).",
                            e.a, e.b, e.pearson
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        "No readings in scope for this query window.".to_string()
    } else {
        lines.join(" ")
    }
}

// ---------------------------------------------------------------------------
// The orchestrated engine
// ---------------------------------------------------------------------------

pub struct AiAnalytics {
    provider: Option<Arc<dyn AIProvider>>,
    engine_label: String,
}

impl AiAnalytics {
    /// Build from environment. Provider preference: AlterOS orchestrator
    /// whenever at least one backend (Ollama endpoint, Anthropic key, or
    /// OpenAI key) is configured; deterministic-only otherwise or when
    /// `EDC_AI_DISABLE=1`.
    pub fn from_env() -> Self {
        if std::env::var("EDC_AI_DISABLE").map(|v| v == "1").unwrap_or(false) {
            return Self {
                provider: None,
                engine_label: "deterministic".to_string(),
            };
        }
        let anthropic = std::env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty());
        let openai = std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty());
        let ollama = std::env::var("EDC_OLLAMA_ENDPOINT").ok().filter(|s| !s.is_empty());

        if anthropic.is_none() && openai.is_none() && ollama.is_none() {
            return Self {
                provider: None,
                engine_label: "deterministic".to_string(),
            };
        }

        let config = AIModelConfig {
            local_endpoint: ollama.clone(),
            local_model: std::env::var("EDC_OLLAMA_MODEL").ok(),
            openai_api_key: openai,
            openai_model: std::env::var("EDC_OPENAI_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
            anthropic_api_key: anthropic,
            anthropic_model: std::env::var("EDC_ANTHROPIC_MODEL")
                .unwrap_or_else(|_| "claude-3-haiku-20240307".to_string()),
            ..AIModelConfig::default()
        };
        let provider: Arc<dyn AIProvider> = Arc::from(config.build_provider());
        Self {
            provider: Some(provider),
            engine_label: "alteros".to_string(),
        }
    }

    /// Deterministic-only engine (used in tests and air-gapped mode).
    pub fn deterministic() -> Self {
        Self {
            provider: None,
            engine_label: "deterministic".to_string(),
        }
    }

    pub fn engine_label(&self) -> &str {
        &self.engine_label
    }

    /// Answer a natural-language question over scoped readings. Runs the
    /// full analytics dossier first; the AI (when configured) narrates the
    /// dossier and refines chart selection, never replacing the numbers.
    pub async fn ask(
        &self,
        question: &str,
        readings: &[TelemetryReading],
        sensors: &[SensorRecord],
        now: i64,
    ) -> AnalyticsAnswer {
        let start = std::time::Instant::now();
        let dossier = build_dossier(readings, sensors, now);
        let charts = deterministic_charts(readings, &dossier);
        let skeleton = deterministic_narrative(&dossier);

        let mut engine = "deterministic".to_string();
        let mut answer = skeleton.clone();

        if let Some(provider) = &self.provider {
            let dossier_json = serde_json::to_string(
                &dossier.iter().map(|g| {
                    serde_json::json!({"method": g.method, "result": g.result})
                }).collect::<Vec<_>>(),
            )
            .unwrap_or_default();
            // Bound the dossier payload sent to the model.
            let dossier_bounded: String = dossier_json.chars().take(24_000).collect();

            let request = CompletionRequest {
                system_prompt: SYSTEM_PROMPT.to_string(),
                messages: vec![ChatMessage {
                    role: "user".to_string(),
                    content: format!(
                        "QUESTION: {question}\n\nANALYTICS DOSSIER (deterministic, computed from on-chain-anchored readings - every figure you state MUST come from here):\n{dossier_bounded}\n\nFACTUAL SKELETON: {skeleton}"
                    ),
                }],
                temperature: 0.3,
                max_tokens: 900,
            };
            match provider.complete(request).await {
                Ok(resp) => {
                    answer = resp.content;
                    engine = format!("{}:{}", self.engine_label, resp.model);
                }
                Err(e) => {
                    tracing::warn!("AI provider failed, deterministic answer served: {e}");
                }
            }
        }

        AnalyticsAnswer {
            answer,
            charts,
            grounding: dossier,
            engine,
            latency_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Narrate a threshold breach for a non-technical stakeholder.
    pub async fn narrate_anomaly(
        &self,
        reading: &TelemetryReading,
        sensor: Option<&SensorRecord>,
        recent: &[TelemetryReading],
        now: i64,
    ) -> AnalyticsAnswer {
        let question = format!(
            "Sensor {} on asset {} reported {} = {} {} which is in the {} band. Explain what happened and what should be checked, in plain language for a non-technical reader.",
            reading.sensor_id, reading.asset_id, reading.parameter,
            reading.value, reading.unit, reading.band
        );
        let sensors: Vec<SensorRecord> = sensor.cloned().into_iter().collect();
        self.ask(&question, recent, &sensors, now).await
    }
}

const SYSTEM_PROMPT: &str = "You are the analytics narrator of a Datachain Rope Ecosystem \
Deployment Console. You are given a QUESTION and an ANALYTICS DOSSIER of deterministic \
computations (descriptive statistics, trends, anomaly detection, forecasts, correlations, \
reliability, compliance, data quality) over on-chain-anchored sensor readings. Rules: \
(1) Every number you state must appear in the dossier - never invent or extrapolate figures \
yourself. (2) Answer plainly for the stakeholder audience; explain method names in ordinary \
words. (3) When the dossier is empty or lacks the data to answer, say so. (4) You have \
read-only access; never claim to have changed anything. Keep answers under 250 words.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::now_ts;

    fn sample_readings(n: usize) -> Vec<TelemetryReading> {
        let t0 = now_ts() - (n as i64) * 600;
        (0..n)
            .map(|i| TelemetryReading {
                project_id: "prj_t".into(),
                asset_id: format!("a{}", i % 3),
                sensor_id: "s1".into(),
                parameter: "soil_moisture".into(),
                value: 40.0 + (i % 7) as f64,
                unit: "%".into(),
                ts: t0 + i as i64 * 600,
                band: if i % 11 == 0 { "warning".into() } else { "ok".into() },
                anchor: String::new(),
            })
            .collect()
    }

    fn sensor() -> SensorRecord {
        SensorRecord {
            id: "s1".into(),
            parent_asset_id: "a0".into(),
            parameter: "soil_moisture".into(),
            unit: "%".into(),
            cadence: "10min".into(),
            readings_per_hour: 6.0,
            range: Some([0.0, 100.0]),
            optimum: Some([35.0, 55.0]),
            warning: Some([20.0, 70.0]),
            protocol: "mqtt".into(),
            endpoint: String::new(),
            sharing_policy: "private".into(),
            write_path: "gateway".into(),
        }
    }

    #[test]
    fn dossier_covers_the_catalogue() {
        let readings = sample_readings(120);
        let dossier = build_dossier(&readings, &[sensor()], now_ts());
        let methods: Vec<&str> = dossier.iter().map(|g| g.method.as_str()).collect();
        for expect in [
            "descriptive_stats:soil_moisture",
            "linear_trend:soil_moisture",
            "anomaly_summary:soil_moisture",
            "forecast_linear:soil_moisture",
            "histogram:soil_moisture",
            "compliance:soil_moisture",
            "cadence_conformity:soil_moisture",
            "data_quality:soil_moisture",
            "group_by_asset",
        ] {
            assert!(
                methods.contains(&expect),
                "dossier missing {expect}; got {methods:?}"
            );
        }
    }

    #[test]
    fn deterministic_charts_from_shape() {
        let readings = sample_readings(120);
        let dossier = build_dossier(&readings, &[sensor()], now_ts());
        let charts = deterministic_charts(&readings, &dossier);
        let kinds: Vec<&str> = charts.iter().map(|c| c.chart.as_str()).collect();
        assert!(kinds.contains(&"line"));
        assert!(kinds.contains(&"bar"));
        assert!(kinds.contains(&"donut"));
        assert!(kinds.contains(&"gauge"));
    }

    #[tokio::test]
    async fn deterministic_ask_grounded() {
        let engine = AiAnalytics::deterministic();
        let readings = sample_readings(60);
        let ans = engine
            .ask("How is soil moisture behaving?", &readings, &[sensor()], now_ts())
            .await;
        assert_eq!(ans.engine, "deterministic");
        assert!(!ans.grounding.is_empty());
        assert!(!ans.charts.is_empty());
        assert!(ans.answer.contains("soil_moisture"));
    }

    #[tokio::test]
    async fn empty_scope_is_honest() {
        let engine = AiAnalytics::deterministic();
        let ans = engine.ask("Anything?", &[], &[], now_ts()).await;
        assert!(ans.answer.contains("No readings"));
        assert!(ans.grounding.is_empty());
    }
}
