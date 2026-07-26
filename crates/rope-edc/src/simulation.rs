//! Simulation / sandbox mode — spec v1.0 §6.3 ("A sandbox mode, served
//! from historical or synthetic data, lets a regulator or investor
//! validate their integration before it touches the live stream") plus
//! the community-testing extension in spec v2.0 §9.
//!
//! Two independent capabilities share this module:
//!
//! 1. **Simulation projects** (`Project.simulation == true`): a community
//!    tester walks the full nine-step wizard, deploys, ingests synthetic
//!    telemetry, exercises the AI analytics, grants, keys, and SSE
//!    streams — without KYB, without cloud provisioning, and without ever
//!    appearing in the real dcscan.io directory. Ready-made archetype
//!    templates populate a realistic inventory in one call.
//!
//! 2. **Sandbox API keys** (`ApiKeyRecord.sandbox == true`) on *any*
//!    project, including live production ones: the stakeholder gateway
//!    answers with deterministic synthetic data derived from the
//!    project's own sensor declarations, never from the live ring. A
//!    regulator can validate its whole integration (REST, GraphQL, SSE,
//!    exports) against the exact schema of the real feed before the
//!    owner mints a production key.
//!
//! Everything here is a pure function of `(project, sensor, timestamp)`.
//! The noise source is blake3-based, so two nodes (or two test runs)
//! generate byte-identical series — no hidden RNG state, fully
//! reproducible, air-gap friendly.

use crate::types::{
    classify_band, AssetRecord, Project, ProjectArchetype, SensorRecord,
    TelemetryReading,
};

/// Deterministic uniform value in `[-1.0, 1.0]` derived from a domain
/// tag, a per-sensor seed, and a time bucket.
fn noise(seed: &str, tag: &str, bucket: i64) -> f64 {
    let h = blake3::hash(format!("edc-sim:{tag}:{seed}:{bucket}").as_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&h.as_bytes()[..8]);
    let u = u64::from_le_bytes(b);
    // Map u64 → [-1, 1].
    (u as f64 / u64::MAX as f64) * 2.0 - 1.0
}

/// Deterministic uniform value in `[0.0, 1.0)`.
fn unit_noise(seed: &str, tag: &str, bucket: i64) -> f64 {
    (noise(seed, tag, bucket) + 1.0) / 2.0
}

/// The baseline / amplitude model for one sensor, derived from its own
/// declared bands so synthetic data always makes sense against the
/// Plage/Optimum/Frequence model the project itself configured.
struct SensorModel {
    baseline: f64,
    half_width: f64,
}

fn model_for(sensor: &SensorRecord) -> SensorModel {
    if let Some([lo, hi]) = sensor.optimum {
        return SensorModel {
            baseline: (lo + hi) / 2.0,
            half_width: ((hi - lo) / 2.0).max(f64::EPSILON),
        };
    }
    if let Some([lo, hi]) = sensor.range {
        return SensorModel {
            baseline: (lo + hi) / 2.0,
            half_width: ((hi - lo) / 8.0).max(f64::EPSILON),
        };
    }
    SensorModel {
        baseline: 50.0,
        half_width: 10.0,
    }
}

/// Synthetic value for `sensor` at `ts` — deterministic, seeded from the
/// project + sensor ids. Composition:
///
/// * daily sinusoidal seasonality (phase differs per sensor),
/// * slow linear drift (positive or negative per sensor — gives the
///   degradation-slope / RUL analytics something real to detect),
/// * blake3 pseudo-noise at 10 % of the optimum half-width,
/// * ~2 % of readings are injected excursions beyond the warning band so
///   anomaly detection, breach counting, and alerting are exercised.
pub fn value_at(project_id: &str, sensor: &SensorRecord, ts: i64) -> f64 {
    let seed = format!("{project_id}:{}", sensor.id);
    let m = model_for(sensor);

    let phase = unit_noise(&seed, "phase", 0) * std::f64::consts::TAU;
    let day_frac = (ts.rem_euclid(86_400)) as f64 / 86_400.0;
    let seasonal =
        (day_frac * std::f64::consts::TAU + phase).sin() * m.half_width * 0.35;

    // Drift: up to ±40 % of the half-width per 30 days, sensor-fixed sign.
    let drift_rate = noise(&seed, "drift", 0) * 0.4 * m.half_width / (30.0 * 86_400.0);
    let drift = drift_rate * (ts.rem_euclid(90 * 86_400)) as f64;

    let jitter = noise(&seed, "jitter", ts) * m.half_width * 0.10;

    let mut value = m.baseline + seasonal + drift + jitter;

    // Injected excursions: ~2 % of readings jump past the warning band.
    if unit_noise(&seed, "spike", ts) < 0.02 {
        let dir = if noise(&seed, "spikedir", ts) >= 0.0 { 1.0 } else { -1.0 };
        let past_warning = sensor
            .warning
            .map(|[lo, hi]| ((hi - lo) / 2.0) * 1.4)
            .unwrap_or(m.half_width * 2.5);
        value = m.baseline + dir * past_warning;
    }

    // Clamp to the physical range when one is declared.
    if let Some([lo, hi]) = sensor.range {
        value = value.clamp(lo, hi);
    }
    value
}

/// Sampling interval (seconds) for a sensor, derived from its declared
/// cadence. Event-driven sensors (0 readings/hour) get a 5-minute pace so
/// the sandbox still produces a stream.
pub fn interval_secs(sensor: &SensorRecord) -> i64 {
    if sensor.readings_per_hour <= 0.0 {
        return 300;
    }
    ((3_600.0 / sensor.readings_per_hour) as i64).clamp(10, 3_600)
}

/// Build one synthetic reading for `sensor` at `ts`.
pub fn reading_at(project: &Project, sensor: &SensorRecord, ts: i64) -> TelemetryReading {
    let value = value_at(&project.id, sensor, ts);
    TelemetryReading {
        project_id: project.id.clone(),
        asset_id: sensor.parent_asset_id.clone(),
        sensor_id: sensor.id.clone(),
        parameter: sensor.parameter.clone(),
        value,
        unit: sensor.unit.clone(),
        ts,
        band: classify_band(sensor, value).to_string(),
        anchor: String::new(),
    }
}

/// Generate a backfilled synthetic history: `points_per_sensor` readings
/// per sensor, ending at `end_ts`, spaced at each sensor's own cadence.
/// Output is sorted by timestamp ascending (ready for the live ring).
pub fn synth_history(
    project: &Project,
    points_per_sensor: usize,
    end_ts: i64,
) -> Vec<TelemetryReading> {
    let mut out = Vec::new();
    for sensor in &project.inventory.sensors {
        let step = interval_secs(sensor);
        for i in 0..points_per_sensor {
            let ts = end_ts - step * (points_per_sensor - 1 - i) as i64;
            out.push(reading_at(project, sensor, ts));
        }
    }
    out.sort_by_key(|r| r.ts);
    out
}

/// One synthetic reading per sensor at `ts` — the sandbox live-stream
/// tick used by the SSE path for sandbox keys.
pub fn synth_tick(project: &Project, ts: i64) -> Vec<TelemetryReading> {
    project
        .inventory
        .sensors
        .iter()
        .map(|s| reading_at(project, s, ts))
        .collect()
}

// ---------------------------------------------------------------------------
// Archetype templates — one-call realistic inventories for community testing
// ---------------------------------------------------------------------------

/// Template identifiers accepted by `POST /projects { template: … }`.
pub const TEMPLATES: [&str; 2] = ["den_haag_escalators", "agri_estate"];

fn sensor(
    id: &str,
    asset: &str,
    parameter: &str,
    unit: &str,
    rph: f64,
    range: [f64; 2],
    optimum: [f64; 2],
    warning: [f64; 2],
) -> SensorRecord {
    SensorRecord {
        id: id.to_string(),
        parent_asset_id: asset.to_string(),
        parameter: parameter.to_string(),
        unit: unit.to_string(),
        cadence: "6min".to_string(),
        readings_per_hour: rph,
        range: Some(range),
        optimum: Some(optimum),
        warning: Some(warning),
        protocol: "mqtt".to_string(),
        endpoint: String::new(),
        sharing_policy: "private".to_string(),
        write_path: "gateway".to_string(),
    }
}

fn asset(id: &str, name: &str, category: &str, sub_type: &str, gps: [f64; 2]) -> AssetRecord {
    AssetRecord {
        id: id.to_string(),
        name: name.to_string(),
        category: category.to_string(),
        sub_type: sub_type.to_string(),
        gps: Some(gps),
        manufacturer: "SimulatedWorks".to_string(),
        model: "SIM-1".to_string(),
        serial_number: format!("SIM-{id}"),
        commissioning_date: "2024-01-15".to_string(),
        warranty_expiry: "2032-01-15".to_string(),
        ownership: "municipality".to_string(),
        mutability_class: "OwnerErasable".to_string(),
        wallet: String::new(),
        tag_id: String::new(),
        health_score: 100.0,
        last_seen_at: 0,
    }
}

/// Populate `project` from a named template. Returns `false` for an
/// unknown template name (project untouched).
pub fn apply_template(project: &mut Project, template: &str) -> bool {
    match template {
        "den_haag_escalators" => {
            if let Some(d) = project.definition.as_mut() {
                d.archetype = ProjectArchetype::PredictiveMaintenance;
                d.tags = vec!["Cities".into(), "Industry".into()];
                d.country = "NL".into();
                d.region = "Den Haag".into();
                d.gps = Some([52.0705, 4.3007]);
                d.description = "Simulation of the Den Haag Ecosystemic Autonomous \
                                 Maintenance escalator fleet: vibration, temperature \
                                 and power monitoring with AI diagnosis."
                    .into();
                d.expected_assets_band = "up to 50".into();
            }
            for i in 1..=3u32 {
                let aid = format!("esc-{i:03}");
                project.inventory.assets.push(asset(
                    &aid,
                    &format!("Escalator {i} — Den Haag Centraal"),
                    "Cities",
                    "escalator",
                    [52.0705 + i as f64 * 0.0002, 4.3007],
                ));
                project.inventory.sensors.push(sensor(
                    &format!("{aid}-vib"),
                    &aid,
                    "vibration",
                    "mm/s",
                    10.0,
                    [0.0, 30.0],
                    [0.5, 4.5],
                    [0.2, 7.1],
                ));
                project.inventory.sensors.push(sensor(
                    &format!("{aid}-temp"),
                    &aid,
                    "motor_temperature",
                    "°C",
                    10.0,
                    [-10.0, 120.0],
                    [25.0, 60.0],
                    [15.0, 80.0],
                ));
                project.inventory.sensors.push(sensor(
                    &format!("{aid}-pwr"),
                    &aid,
                    "power_draw",
                    "kW",
                    10.0,
                    [0.0, 25.0],
                    [3.0, 9.0],
                    [1.0, 14.0],
                ));
            }
            true
        }
        "agri_estate" => {
            if let Some(d) = project.definition.as_mut() {
                d.archetype = ProjectArchetype::EnvironmentalMonitoring;
                d.tags = vec!["Agriculture".into(), "Environment".into()];
                d.country = "FR".into();
                d.region = "Occitanie".into();
                d.gps = Some([43.6045, 1.4442]);
                d.description = "Simulation of an agricultural estate: soil probes \
                                 and a weather station feeding environmental \
                                 monitoring analytics (DC Sentient Environment model)."
                    .into();
                d.expected_assets_band = "up to 50".into();
            }
            for i in 1..=4u32 {
                let aid = format!("parcel-{i:02}");
                project.inventory.assets.push(asset(
                    &aid,
                    &format!("Parcel {i} soil probe"),
                    "Agriculture",
                    "soil probe",
                    [43.6045 + i as f64 * 0.001, 1.4442],
                ));
                project.inventory.sensors.push(sensor(
                    &format!("{aid}-moist"),
                    &aid,
                    "soil_moisture",
                    "%",
                    6.0,
                    [0.0, 100.0],
                    [35.0, 55.0],
                    [20.0, 70.0],
                ));
                project.inventory.sensors.push(sensor(
                    &format!("{aid}-ph"),
                    &aid,
                    "soil_ph",
                    "pH",
                    2.0,
                    [3.0, 10.0],
                    [6.0, 7.2],
                    [5.2, 8.0],
                ));
            }
            project.inventory.assets.push(asset(
                "weather-01",
                "Estate weather station",
                "Environment",
                "weather station",
                [43.6050, 1.4448],
            ));
            project.inventory.sensors.push(sensor(
                "weather-01-temp",
                "weather-01",
                "air_temperature",
                "°C",
                12.0,
                [-20.0, 50.0],
                [8.0, 28.0],
                [-2.0, 36.0],
            ));
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Background ticker for Live simulation projects
// ---------------------------------------------------------------------------

/// Every `EDC_SIM_TICK_SECS` (default 60 s, min 5 s), tie one synthetic
/// reading per sensor into the live store of every Live simulation
/// project — so the console dashboard, dossier, and SSE streams of a
/// sandbox project keep moving exactly like a real deployment's.
/// Live (non-simulation) projects are never touched.
pub async fn run_simulation_ticker(registry: std::sync::Arc<crate::registry::Registry>) {
    let tick = std::env::var("EDC_SIM_TICK_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .max(5);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(tick)).await;
        let now = crate::types::now_ts();
        for project in registry.list_projects() {
            if !project.simulation
                || !matches!(project.status, crate::types::ProjectStatus::Live)
            {
                continue;
            }
            for reading in synth_tick(&project, now) {
                registry.push_reading(reading);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Project;

    fn sim_project() -> Project {
        let mut p = Project::new("Sim test", "0x0000000000000000000000000000000000000001");
        p.simulation = true;
        assert!(apply_template(&mut p, "den_haag_escalators"));
        p
    }

    #[test]
    fn synthetic_values_deterministic() {
        let p = sim_project();
        let s = &p.inventory.sensors[0];
        let a = value_at(&p.id, s, 1_800_000_000);
        let b = value_at(&p.id, s, 1_800_000_000);
        assert_eq!(a, b);
        // A different project id gives a different series.
        assert_ne!(a, value_at("prj_other", s, 1_800_000_000));
    }

    #[test]
    fn synthetic_values_respect_declared_range() {
        let p = sim_project();
        for s in &p.inventory.sensors {
            let [lo, hi] = s.range.unwrap();
            for i in 0..500 {
                let v = value_at(&p.id, s, 1_800_000_000 + i * 360);
                assert!(v >= lo && v <= hi, "{} out of [{lo},{hi}]", v);
            }
        }
    }

    #[test]
    fn synthetic_history_shape_and_bands() {
        let p = sim_project();
        let readings = synth_history(&p, 50, 1_800_000_000);
        assert_eq!(readings.len(), 50 * p.inventory.sensors.len());
        // Sorted ascending.
        assert!(readings.windows(2).all(|w| w[0].ts <= w[1].ts));
        // The excursion injector must produce at least one non-ok band
        // across 450 readings (~2 % spike rate).
        assert!(readings.iter().any(|r| r.band != "ok"));
        // But the majority must be in the optimum band.
        let ok = readings.iter().filter(|r| r.band == "ok").count();
        assert!(ok * 2 > readings.len());
    }

    #[test]
    fn tick_produces_one_reading_per_sensor() {
        let p = sim_project();
        let tick = synth_tick(&p, 1_800_000_123);
        assert_eq!(tick.len(), p.inventory.sensors.len());
        assert!(tick.iter().all(|r| r.ts == 1_800_000_123));
    }

    #[test]
    fn templates_populate_inventory() {
        let mut p = Project::new("t", "0x0000000000000000000000000000000000000001");
        assert!(apply_template(&mut p, "agri_estate"));
        assert_eq!(p.inventory.assets.len(), 5);
        assert_eq!(p.inventory.sensors.len(), 9);
        assert!(!apply_template(&mut p, "no_such_template"));
    }

    #[test]
    fn event_driven_sensor_gets_default_interval() {
        let mut s = sensor("x", "a", "p", "u", 0.0, [0.0, 1.0], [0.2, 0.8], [0.1, 0.9]);
        s.readings_per_hour = 0.0;
        assert_eq!(interval_secs(&s), 300);
    }
}
