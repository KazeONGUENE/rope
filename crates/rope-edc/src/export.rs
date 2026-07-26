//! Scheduled bulk export — spec v1.0 §6.3: "Scheduled bulk export |
//! Batched CSV or Parquet extracts on a fixed schedule | Auditors and
//! statutory reporting".
//!
//! Grants whose `delivery` includes `export` and whose `export_schedule`
//! is `hourly` / `daily` / `weekly` get a background producer: at each
//! cadence boundary the scheduler writes a scope-filtered CSV extract to
//! `{data_dir}/exports/{grant_id}/{unix_ts}-readings.csv`. Stakeholders
//! list and download their extracts through the gateway
//! (`GET /stakeholder/exports`, `GET /stakeholder/exports/:name`) with
//! the same grant credential that scoped them. On-demand pulls
//! (`GET /stakeholder/export?facet=…`) reuse the same CSV builders.
//!
//! CSV output is RFC-4180: CRLF line endings, quoting only where needed,
//! stable column order — importable by Excel, pandas, and R without
//! options.

use std::sync::Arc;

use crate::grants::AccessGrant;
use crate::registry::Registry;
use crate::types::{now_ts, ApprovalEvent, DiagnosisEvent, Project, TelemetryReading};

/// How many extract files are retained per grant (oldest pruned first).
const RETAINED_FILES_PER_GRANT: usize = 30;

/// Cadence label → period seconds. Unknown labels mean "no schedule".
pub fn schedule_secs(schedule: &str) -> Option<i64> {
    match schedule {
        "hourly" => Some(3_600),
        "daily" => Some(86_400),
        "weekly" => Some(7 * 86_400),
        _ => None,
    }
}

fn csv_field(raw: &str) -> String {
    if raw.contains(',') || raw.contains('"') || raw.contains('\n') || raw.contains('\r') {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw.to_string()
    }
}

fn csv_row(fields: &[String]) -> String {
    let mut line = fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",");
    line.push_str("\r\n");
    line
}

/// Render readings as RFC-4180 CSV (header included).
pub fn readings_csv(readings: &[TelemetryReading]) -> String {
    let mut out = csv_row(&[
        "ts".into(),
        "iso_time".into(),
        "asset_id".into(),
        "sensor_id".into(),
        "parameter".into(),
        "value".into(),
        "unit".into(),
        "band".into(),
        "anchor".into(),
    ]);
    for r in readings {
        let iso = chrono::DateTime::from_timestamp(r.ts, 0)
            .map(|d| d.to_rfc3339())
            .unwrap_or_default();
        out.push_str(&csv_row(&[
            r.ts.to_string(),
            iso,
            r.asset_id.clone(),
            r.sensor_id.clone(),
            r.parameter.clone(),
            format!("{}", r.value),
            r.unit.clone(),
            r.band.clone(),
            r.anchor.clone(),
        ]));
    }
    out
}

/// Render diagnoses as RFC-4180 CSV.
pub fn diagnoses_csv(items: &[DiagnosisEvent]) -> String {
    let mut out = csv_row(&[
        "ts".into(),
        "asset_id".into(),
        "agent_id".into(),
        "diagnosis".into(),
        "recommendation".into(),
        "confidence".into(),
        "anchor".into(),
    ]);
    for d in items {
        out.push_str(&csv_row(&[
            d.ts.to_string(),
            d.asset_id.clone(),
            d.agent_id.clone(),
            d.diagnosis.clone(),
            d.recommendation.clone(),
            format!("{}", d.confidence),
            d.anchor.clone(),
        ]));
    }
    out
}

/// Render approvals as RFC-4180 CSV.
pub fn approvals_csv(items: &[ApprovalEvent]) -> String {
    let mut out = csv_row(&[
        "ts".into(),
        "subject".into(),
        "approved_by".into(),
        "role".into(),
        "note".into(),
        "anchor".into(),
    ]);
    for a in items {
        out.push_str(&csv_row(&[
            a.ts.to_string(),
            a.subject.clone(),
            a.approved_by.clone(),
            format!("{:?}", a.role),
            a.note.clone(),
            a.anchor.clone(),
        ]));
    }
    out
}

/// Scope-filter readings against a grant (same rule the REST gateway
/// applies): asset id + category restrictions.
pub fn scoped_readings(
    project: &Project,
    grant: &AccessGrant,
    readings: &[TelemetryReading],
) -> Vec<TelemetryReading> {
    readings
        .iter()
        .filter(|r| {
            let category = project
                .inventory
                .assets
                .iter()
                .find(|a| a.id == r.asset_id)
                .map(|a| a.category.as_str())
                .unwrap_or("");
            grant.scope.allows_asset(&r.asset_id, category)
        })
        .cloned()
        .collect()
}

/// One pass of the export scheduler: produce every extract that is due.
/// Returns the number of extracts written (for logging / tests).
pub fn run_due_exports(registry: &Arc<Registry>, now: i64) -> usize {
    let mut produced = 0usize;
    for grant in registry.all_grants() {
        if !grant.allows_delivery("export") {
            continue;
        }
        let Some(period) = schedule_secs(&grant.export_schedule) else {
            continue;
        };
        if !grant.is_usable(now) || now - grant.last_export_at < period {
            continue;
        }
        let Some(project) = registry.get_project(&grant.project_id) else {
            continue;
        };
        // Extract window: everything since the last export (or one full
        // period for the first run).
        let since = if grant.last_export_at > 0 {
            grant.last_export_at
        } else {
            now - period
        };
        let readings: Vec<TelemetryReading> = {
            let store = registry.live_store(&project.id);
            let s = store.read();
            scoped_readings(&project, &grant, &s.readings)
                .into_iter()
                .filter(|r| r.ts >= since)
                .collect()
        };
        let csv = readings_csv(&readings);
        let dir = registry.exports_dir(&grant.id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::error!("export dir create failed for {}: {e}", grant.id);
            continue;
        }
        let filename = format!("{now}-readings.csv");
        if let Err(e) = std::fs::write(dir.join(&filename), csv.as_bytes()) {
            tracing::error!("export write failed for {}: {e}", grant.id);
            continue;
        }
        prune_old(&dir);
        registry.update_grant(&grant.id, |g| g.last_export_at = now);
        tracing::info!(
            "scheduled export produced: grant={} file={} rows={}",
            grant.id,
            filename,
            readings.len()
        );
        produced += 1;
    }
    produced
}

fn prune_old(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    while files.len() > RETAINED_FILES_PER_GRANT {
        let oldest = files.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
}

/// List the extract files available to a grant, newest first.
pub fn list_exports(registry: &Arc<Registry>, grant_id: &str) -> Vec<serde_json::Value> {
    let dir = registry.exports_dir(grant_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<serde_json::Value> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let size = e.metadata().ok()?.len();
            Some(serde_json::json!({ "name": name, "bytes": size }))
        })
        .collect();
    out.sort_by(|a, b| {
        b["name"]
            .as_str()
            .unwrap_or("")
            .cmp(a["name"].as_str().unwrap_or(""))
    });
    out
}

/// Read one extract file for a grant. The filename is validated against
/// path traversal before touching the filesystem.
pub fn read_export(
    registry: &Arc<Registry>,
    grant_id: &str,
    filename: &str,
) -> Option<Vec<u8>> {
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return None;
    }
    std::fs::read(registry.exports_dir(grant_id).join(filename)).ok()
}

/// Background loop: check for due exports every 60 seconds.
pub async fn run_export_scheduler(registry: Arc<Registry>) {
    loop {
        run_due_exports(&registry, now_ts());
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escaping() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn readings_csv_shape() {
        let r = TelemetryReading {
            project_id: "p".into(),
            asset_id: "a,1".into(),
            sensor_id: "s1".into(),
            parameter: "vibration".into(),
            value: 3.25,
            unit: "mm/s".into(),
            ts: 1_800_000_000,
            band: "ok".into(),
            anchor: String::new(),
        };
        let csv = readings_csv(&[r]);
        let lines: Vec<&str> = csv.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("ts,iso_time,asset_id"));
        assert!(lines[1].contains("\"a,1\""));
        assert!(lines[1].contains("3.25"));
    }

    #[test]
    fn schedule_parsing() {
        assert_eq!(schedule_secs("hourly"), Some(3_600));
        assert_eq!(schedule_secs("daily"), Some(86_400));
        assert_eq!(schedule_secs("weekly"), Some(604_800));
        assert_eq!(schedule_secs(""), None);
        assert_eq!(schedule_secs("fortnightly"), None);
    }
}
