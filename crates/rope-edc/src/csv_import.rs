//! RFC-4180 CSV parsing for the bulk inventory import wizard
//! (spec v1.0 §4.5 - "bulk import via CSV or API against this exact schema").
//!
//! Self-contained parser: quoted fields, escaped quotes (`""`), embedded
//! commas and newlines inside quotes, CRLF and LF line endings.

use crate::types::{AssetRecord, SensorRecord};

/// Parse raw CSV text into rows of fields.
pub fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                _ => field.push(c),
            }
        } else {
            match c {
                '"' => in_quotes = true,
                ',' => {
                    row.push(std::mem::take(&mut field));
                }
                '\r' => { /* swallow; LF closes the row */ }
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    if !(row.len() == 1 && row[0].is_empty()) {
                        rows.push(std::mem::take(&mut row));
                    } else {
                        row.clear();
                    }
                }
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if !(row.len() == 1 && row[0].is_empty()) {
            rows.push(row);
        }
    }
    rows
}

/// Result of a bulk import: what was accepted plus per-line rejections.
#[derive(Debug, serde::Serialize)]
pub struct ImportReport {
    pub imported: usize,
    pub rejected: Vec<ImportRejection>,
}

#[derive(Debug, serde::Serialize)]
pub struct ImportRejection {
    pub line: usize,
    pub reason: String,
}

fn col<'a>(header: &[String], row: &'a [String], name: &str) -> Option<&'a str> {
    header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
        .and_then(|i| row.get(i))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
}

fn col_f64(header: &[String], row: &[String], name: &str) -> Option<f64> {
    col(header, row, name).and_then(|s| s.parse().ok())
}

/// Import assets from CSV. Required columns: `id`, `name`, `category`,
/// `sub_type`. Optional: `lat`, `lon`, `manufacturer`, `model`,
/// `serial_number`, `commissioning_date`, `warranty_expiry`, `ownership`,
/// `mutability_class`, `tag_id`.
pub fn import_assets(input: &str) -> (Vec<AssetRecord>, ImportReport) {
    let rows = parse_csv(input);
    let mut out = Vec::new();
    let mut rejected = Vec::new();
    if rows.is_empty() {
        return (
            out,
            ImportReport { imported: 0, rejected: vec![ImportRejection { line: 0, reason: "empty file".into() }] },
        );
    }
    let header = &rows[0];
    for (i, row) in rows.iter().enumerate().skip(1) {
        let line = i + 1;
        let (Some(id), Some(name), Some(category), Some(sub_type)) = (
            col(header, row, "id"),
            col(header, row, "name"),
            col(header, row, "category"),
            col(header, row, "sub_type"),
        ) else {
            rejected.push(ImportRejection {
                line,
                reason: "missing one of required columns: id, name, category, sub_type".into(),
            });
            continue;
        };
        let gps = match (col_f64(header, row, "lat"), col_f64(header, row, "lon")) {
            (Some(lat), Some(lon)) => Some([lat, lon]),
            _ => None,
        };
        let mutability = col(header, row, "mutability_class").unwrap_or("OwnerErasable");
        const VALID: [&str; 5] = [
            "Immutable", "OwnerErasable", "TimeBound", "GDPRCompliant", "ConditionalErasure",
        ];
        if !VALID.contains(&mutability) {
            rejected.push(ImportRejection {
                line,
                reason: format!("invalid mutability_class '{mutability}'"),
            });
            continue;
        }
        out.push(AssetRecord {
            id: id.to_string(),
            name: name.to_string(),
            category: category.to_string(),
            sub_type: sub_type.to_string(),
            gps,
            manufacturer: col(header, row, "manufacturer").unwrap_or_default().to_string(),
            model: col(header, row, "model").unwrap_or_default().to_string(),
            serial_number: col(header, row, "serial_number").unwrap_or_default().to_string(),
            commissioning_date: col(header, row, "commissioning_date").unwrap_or_default().to_string(),
            warranty_expiry: col(header, row, "warranty_expiry").unwrap_or_default().to_string(),
            ownership: col(header, row, "ownership").unwrap_or_default().to_string(),
            mutability_class: mutability.to_string(),
            wallet: String::new(),
            tag_id: col(header, row, "tag_id").unwrap_or_default().to_string(),
            health_score: 100.0,
            last_seen_at: 0,
        });
    }
    let report = ImportReport { imported: out.len(), rejected };
    (out, report)
}

/// Import sensors from CSV. Required: `id`, `parent_asset_id`, `parameter`,
/// `unit`, `cadence`. Optional: `readings_per_hour`, `range_min`,
/// `range_max`, `optimum_min`, `optimum_max`, `warning_min`, `warning_max`,
/// `protocol`, `endpoint`, `sharing_policy`, `write_path`.
pub fn import_sensors(input: &str) -> (Vec<SensorRecord>, ImportReport) {
    let rows = parse_csv(input);
    let mut out = Vec::new();
    let mut rejected = Vec::new();
    if rows.is_empty() {
        return (
            out,
            ImportReport { imported: 0, rejected: vec![ImportRejection { line: 0, reason: "empty file".into() }] },
        );
    }
    let header = &rows[0];
    for (i, row) in rows.iter().enumerate().skip(1) {
        let line = i + 1;
        let (Some(id), Some(parent), Some(parameter), Some(unit), Some(cadence)) = (
            col(header, row, "id"),
            col(header, row, "parent_asset_id"),
            col(header, row, "parameter"),
            col(header, row, "unit"),
            col(header, row, "cadence"),
        ) else {
            rejected.push(ImportRejection {
                line,
                reason: "missing one of required columns: id, parent_asset_id, parameter, unit, cadence".into(),
            });
            continue;
        };
        let band = |lo: &str, hi: &str| -> Option<[f64; 2]> {
            match (col_f64(header, row, lo), col_f64(header, row, hi)) {
                (Some(a), Some(b)) if a <= b => Some([a, b]),
                _ => None,
            }
        };
        out.push(SensorRecord {
            id: id.to_string(),
            parent_asset_id: parent.to_string(),
            parameter: parameter.to_string(),
            unit: unit.to_string(),
            cadence: cadence.to_string(),
            readings_per_hour: col_f64(header, row, "readings_per_hour").unwrap_or(0.0),
            range: band("range_min", "range_max"),
            optimum: band("optimum_min", "optimum_max"),
            warning: band("warning_min", "warning_max"),
            protocol: col(header, row, "protocol").unwrap_or_default().to_string(),
            endpoint: col(header, row, "endpoint").unwrap_or_default().to_string(),
            sharing_policy: col(header, row, "sharing_policy").unwrap_or("private").to_string(),
            write_path: col(header, row, "write_path").unwrap_or("gateway").to_string(),
        });
    }
    let report = ImportReport { imported: out.len(), rejected };
    (out, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4180_quoting() {
        let rows = parse_csv("a,\"b,with,commas\",\"quote \"\" inside\"\r\nnext,line,3\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec!["a", "b,with,commas", "quote \" inside"]);
        assert_eq!(rows[1], vec!["next", "line", "3"]);
    }

    #[test]
    fn asset_import_happy_and_rejects() {
        let csv = "id,name,category,sub_type,lat,lon,mutability_class\n\
                   SL-1,Street light 1,Cities,street_light,52.08,4.31,TimeBound\n\
                   ,missing id,Cities,bench,,,\n\
                   SL-2,Bad class,Cities,street_light,,,NotAClass\n";
        let (assets, report) = import_assets(csv);
        assert_eq!(assets.len(), 1);
        assert_eq!(report.imported, 1);
        assert_eq!(report.rejected.len(), 2);
        assert_eq!(assets[0].gps, Some([52.08, 4.31]));
        assert_eq!(assets[0].mutability_class, "TimeBound");
    }

    #[test]
    fn sensor_import_bands() {
        let csv = "id,parent_asset_id,parameter,unit,cadence,readings_per_hour,optimum_min,optimum_max,warning_min,warning_max\n\
                   s1,SL-1,soil_moisture,%,6min,10,35,55,20,70\n";
        let (sensors, report) = import_sensors(csv);
        assert_eq!(report.imported, 1);
        assert_eq!(sensors[0].optimum, Some([35.0, 55.0]));
        assert_eq!(sensors[0].warning, Some([20.0, 70.0]));
        assert_eq!(sensors[0].readings_per_hour, 10.0);
    }
}
