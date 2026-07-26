//! Scheduled report generation — spec v1.0 §6.4: "Scheduled reports:
//! statutory / investor reporting generated automatically on a cadence,
//! grounded in the period's data and anchored on-chain."
//!
//! A project opts in by setting `Project.report_schedule` to `hourly`,
//! `daily`, `weekly`, or `monthly` (console route). The scheduler then:
//!
//! 1. selects the readings of the elapsed period from the live store,
//! 2. runs the full deterministic analytics catalogue over them
//!    (`ai::build_dossier`) — every figure traces to a named method,
//! 3. renders the deterministic narrative (`ai::deterministic_narrative`),
//! 4. persists the `ReportRecord` to the project journal, and
//! 5. anchors a `ScheduledReport` knot on the project string with the
//!    dossier digest, so the report is externally verifiable.
//!
//! Reports can also be generated on demand from the console
//! (`cadence = "on_demand"`), which shares the same generation path.

use std::sync::Arc;

use crate::ai;
use crate::registry::Registry;
use crate::types::{now_ts, Project, ProjectStatus, ReportRecord, TelemetryReading};

/// Report cadence label → period length in seconds. `on_demand` is not a
/// schedule; it maps to a trailing 24h window.
pub fn cadence_secs(cadence: &str) -> Option<i64> {
    match cadence {
        "hourly" => Some(3_600),
        "daily" => Some(86_400),
        "weekly" => Some(7 * 86_400),
        "monthly" => Some(30 * 86_400),
        _ => None,
    }
}

/// Generate one report for `project` covering `[period_start, period_end]`.
/// Pure over the live store contents — the caller decides the window.
pub fn generate(
    registry: &Arc<Registry>,
    project: &Project,
    cadence: &str,
    period_start: i64,
    period_end: i64,
) -> ReportRecord {
    let store = registry.live_store(&project.id);
    let readings: Vec<TelemetryReading> = {
        let s = store.read();
        s.readings
            .iter()
            .filter(|r| r.ts >= period_start && r.ts <= period_end)
            .cloned()
            .collect()
    };

    let dossier = ai::build_dossier(&readings, &project.inventory.sensors, period_end);
    let narrative = if readings.is_empty() {
        format!(
            "No telemetry was recorded for project '{}' between {} and {}. \
             All {} declared sensors were silent for the period.",
            project.name(),
            period_start,
            period_end,
            project.inventory.sensors.len()
        )
    } else {
        ai::deterministic_narrative(&dossier)
    };

    let uid = uuid::Uuid::new_v4().simple().to_string();
    ReportRecord {
        id: format!("rpt_{}", &uid[..12]),
        project_id: project.id.clone(),
        cadence: cadence.to_string(),
        period_start,
        period_end,
        generated_at: now_ts(),
        readings_in_scope: readings.len(),
        narrative,
        dossier: serde_json::to_value(&dossier).unwrap_or_default(),
        anchor: String::new(),
    }
}

/// Persist + anchor a generated report. Returns the record with its
/// anchor knot hash filled in (empty when the chain is unreachable —
/// the journal remains the primary record either way).
pub async fn persist_and_anchor(
    registry: &Arc<Registry>,
    project: &Project,
    mut report: ReportRecord,
) -> ReportRecord {
    // Anchor a digest, not the full dossier: the journal holds the body,
    // the chain holds the tamper-evident commitment.
    let dossier_digest = hex::encode(
        blake3::hash(report.dossier.to_string().as_bytes()).as_bytes(),
    );
    let anchor = registry
        .anchor(
            &project.wallet,
            "ScheduledReport",
            format!(
                "{} report for '{}' covering {}–{}: {} readings analysed",
                report.cadence,
                project.name(),
                report.period_start,
                report.period_end,
                report.readings_in_scope
            ),
            serde_json::json!({
                "report_id": report.id,
                "project_id": report.project_id,
                "cadence": report.cadence,
                "period_start": report.period_start,
                "period_end": report.period_end,
                "readings_in_scope": report.readings_in_scope,
                "dossier_blake3": dossier_digest,
            }),
        )
        .await;
    if let Some(a) = anchor {
        report.anchor = a;
    }
    registry.push_report(report.clone());
    report
}

/// One scheduler pass: generate every report that is due at `now`.
/// Returns the number of reports produced.
pub async fn run_due_reports(registry: &Arc<Registry>, now: i64) -> usize {
    let mut produced = 0usize;
    for project in registry.list_projects() {
        if !matches!(project.status, ProjectStatus::Live | ProjectStatus::Suspended) {
            continue;
        }
        let Some(period) = cadence_secs(&project.report_schedule) else {
            continue;
        };
        let last = if project.last_report_at > 0 {
            project.last_report_at
        } else {
            // First run: start the clock now rather than back-filling
            // arbitrary history.
            registry.update_project(&project.id, |p| p.last_report_at = now);
            continue;
        };
        if now - last < period {
            continue;
        }

        let report = generate(registry, &project, &project.report_schedule, last, now);
        let report = persist_and_anchor(registry, &project, report).await;
        registry.update_project(&project.id, |p| p.last_report_at = now);
        tracing::info!(
            "scheduled report {} generated for project {} ({} readings)",
            report.id,
            project.id,
            report.readings_in_scope
        );
        produced += 1;
    }
    produced
}

/// Background loop: check for due reports every 60 s.
pub async fn run_report_scheduler(registry: Arc<Registry>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let _ = run_due_reports(&registry, now_ts()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Project, SensorRecord};

    fn sensor(id: &str) -> SensorRecord {
        SensorRecord {
            id: id.into(),
            parent_asset_id: "a1".into(),
            parameter: "temperature".into(),
            unit: "°C".into(),
            cadence: "hourly".into(),
            readings_per_hour: 1.0,
            range: Some([-20.0, 60.0]),
            optimum: Some([15.0, 25.0]),
            warning: Some([5.0, 35.0]),
            protocol: "mqtt".into(),
            endpoint: String::new(),
            sharing_policy: "private".into(),
            write_path: "gateway".into(),
        }
    }

    #[test]
    fn cadence_parsing() {
        assert_eq!(cadence_secs("hourly"), Some(3_600));
        assert_eq!(cadence_secs("monthly"), Some(30 * 86_400));
        assert_eq!(cadence_secs(""), None);
        assert_eq!(cadence_secs("on_demand"), None);
    }

    #[test]
    fn report_grounded_in_period_data() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();
        let mut p = Project::new("Report test", "0xowner");
        p.inventory.sensors.push(sensor("s1"));
        let pid = p.id.clone();
        registry.insert_project(p.clone());

        let now = now_ts();
        for i in 0..10 {
            registry.push_reading(TelemetryReading {
                project_id: pid.clone(),
                asset_id: "a1".into(),
                sensor_id: "s1".into(),
                parameter: "temperature".into(),
                value: 20.0 + i as f64 * 0.1,
                unit: "°C".into(),
                ts: now - 1_000 + i * 10,
                band: "ok".into(),
                anchor: String::new(),
            });
        }

        let report = generate(&registry, &p, "daily", now - 86_400, now);
        assert_eq!(report.readings_in_scope, 10);
        assert!(!report.narrative.is_empty());
        assert!(report.dossier.is_array());

        // A window before the data holds nothing — and says so.
        let empty = generate(&registry, &p, "daily", now - 200_000, now - 100_000);
        assert_eq!(empty.readings_in_scope, 0);
        assert!(empty.narrative.contains("No telemetry"));
    }
}
