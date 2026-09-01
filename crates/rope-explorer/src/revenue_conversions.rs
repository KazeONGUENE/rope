//! Public read surface for SaaS fiat-to-FAT conversion knots.
//!
//! Spec: `docs/FIAT_REVENUE_ONCHAIN_FAT_PURCHASE_SPEC_V1.md` §7 / §10.1.
//! Ledger: `0x000000000000000000000000000000000000d004`.
//!
//! This module is the **watchable substrate** promised to auditors and
//! to the 2026-08-16 circ-supply reply (Andrew Neophytou). It does
//! **not** run the converter. It never fabricates a conversion. Until
//! a real `FiatRevenueConvertedToFat` knot with a `swap_tx` lands on
//! the ledger, the payload stays `live: false` / `phase: "pending"`.
//!
//! Frozen product facts (do not reopen here):
//! - Ecosystem default `eta_fiat_to_fat` is **0.50**, not 0.80.
//! - Tanastok Private Pool USDC share stays **0.10**.
//! - Combined 0.50 + 0.10 = 0.60 tokenomics eta. Andrew's 80% is a
//!   per-product override that is **not** configured and **not** live.
//! - Do not tell anyone fiat buybacks are running. They are not.

use crate::AppState;
use crate::swr;
use axum::extract::{Query, State};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

/// Canonical conversion ledger (spec §7). Override with
/// `REVENUE_CONVERSION_LEDGER_WALLET` only if the operator relocates it.
pub const DEFAULT_LEDGER: &str = "0x000000000000000000000000000000000000d004";

/// Spec-frozen default share of fiat **net** revenue that will buy FAT
/// on DCSwap once Phase 1 is activated. Basis points form is 5000.
pub const DEFAULT_ETA_FIAT_TO_FAT: f64 = 0.50;
pub const DEFAULT_ETA_FIAT_TO_FAT_BPS: u32 = 5000;

/// Spec-frozen Tanastok Private Pool USDC share. Independent of the
/// FAT buy lever. Do not steal this to fund the AMM.
pub const DEFAULT_ETA_POOL_USDC: f64 = 0.10;

/// Interaction type that means a real AMM purchase happened.
const KNOT_CONVERTED: &str = "FiatRevenueConvertedToFat";
/// Interaction type for the one-time ledger establishment (not a buy).
const KNOT_ESTABLISHED: &str = "RevenueConversionLedgerEstablished";

const SWR_FRESH_SECS: i64 = 60;
const SWR_STALE_SECS: i64 = 600;
const SWR_COMPUTE_TIMEOUT_SECS: u64 = 8;

fn ledger_wallet() -> String {
    std::env::var("REVENUE_CONVERSION_LEDGER_WALLET").unwrap_or_else(|_| DEFAULT_LEDGER.to_string())
}

fn swr_cache() -> &'static Arc<swr::SwrCache> {
    static CACHE: OnceLock<Arc<swr::SwrCache>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(swr::SwrCache::new("api_v1_revenue_conversions")))
}

/// Honest empty / pre-activation payload. Used as the SWR fallback and
/// as the body when the ledger has no conversion knots yet.
pub fn pending_payload(ledger: &str, conversions: Vec<Value>) -> Value {
    let live = conversions.iter().any(is_live_conversion);
    json!({
        "live": live,
        "phase": if live { "converting" } else { "pending" },
        "eta_fiat_to_fat": DEFAULT_ETA_FIAT_TO_FAT,
        "eta_fiat_to_fat_bps": DEFAULT_ETA_FIAT_TO_FAT_BPS,
        "eta_pool_usdc": DEFAULT_ETA_POOL_USDC,
        "ledger": ledger.to_lowercase(),
        "label": "Revenue FAT Conversion Ledger",
        "explorer": format!("https://dcscan.io/address/{ledger}"),
        "spec": "FIAT_REVENUE_ONCHAIN_FAT_PURCHASE_SPEC_V1",
        "conversions": conversions,
        "count": conversions.len(),
        "note": if live {
            "Conversions listed below are rebuilt from knots on the conversion ledger. Each row must carry a DCSwap swap_tx."
        } else {
            "Phase 1 not activated. Spec is frozen. Default buy share is 50% of fiat net revenue (not 80%). Tanastok Private Pool USDC share stays 10%. No AMM buybacks have run."
        }
    })
}

fn is_live_conversion(row: &Value) -> bool {
    let Some(swap) = row.get("swap_tx").and_then(|v| v.as_str()) else {
        return false;
    };
    swap.starts_with("0x") && swap.len() == 66
}

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    pub project: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// `GET /api/v1/revenue-conversions`
pub async fn list_revenue_conversions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Json<Value> {
    let cfg = swr::SwrConfig {
        fresh_ttl_secs: SWR_FRESH_SECS,
        stale_ttl_secs: SWR_STALE_SECS,
        compute_timeout_secs: SWR_COMPUTE_TIMEOUT_SECS,
        endpoint_name: "api_v1_revenue_conversions",
    };
    let state_bg = Arc::clone(&state);
    let ledger = ledger_wallet();
    let ledger_fb = ledger.clone();
    let body = swr_cache()
        .serve(
            cfg,
            move || {
                let s = Arc::clone(&state_bg);
                async move { rebuild_from_rope(&s).await }
            },
            move || pending_payload(&ledger_fb, Vec::new()),
        )
        .await;
    Json(apply_filters(body, &q))
}

async fn post_rpc(state: &AppState, body: &Value) -> Result<Value, String> {
    let rpc = state.rpc_url_active().to_string();
    for attempt in 0..2 {
        match state.http_client.post(&rpc).json(body).send().await {
            Ok(resp) => {
                return resp
                    .json::<Value>()
                    .await
                    .map_err(|e| format!("unreadable rope-node response: {e}"));
            }
            Err(e) => {
                if attempt == 0 {
                    tracing::warn!(
                        "revenue-conversions RPC transport error, retrying once: {}",
                        e
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
                return Err(format!("rope-node unreachable after retry: {e}"));
            }
        }
    }
    Err("rope-node unreachable".into())
}

/// Walk the conversion ledger and return the public payload. Never
/// invents a conversion row. Establishment knots are counted but not
/// listed as buys.
async fn rebuild_from_rope(state: &AppState) -> Value {
    let wallet = ledger_wallet();
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "rope_repatriatePersonalLedger",
        "params": [wallet, {"decrypt": true}],
    });
    let body = match post_rpc(state, &req).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("revenue-conversions rebuild-from-rope failed: {}", e);
            let mut payload = pending_payload(&wallet, Vec::new());
            payload["rebuild_error"] = json!(e);
            return payload;
        }
    };

    let fragments = body
        .get("result")
        .and_then(|r| r.get("fragments"))
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();

    let mut conversions: Vec<Value> = Vec::new();
    let mut established = false;
    for frag in &fragments {
        let Some(interaction) = frag.get("interaction").filter(|i| !i.is_null()) else {
            continue;
        };
        let itype = interaction_type(interaction);
        if itype.contains(KNOT_ESTABLISHED) {
            established = true;
            continue;
        }
        if !itype.contains(KNOT_CONVERTED) {
            continue;
        }
        if let Some(row) = conversion_from_fragment(frag, interaction) {
            conversions.push(row);
        }
    }

    let mut payload = pending_payload(&wallet, conversions);
    payload["ledger_established"] = json!(established);
    payload["ledger_knot_count"] = json!(fragments.len());
    payload
}

fn interaction_type(interaction: &Value) -> String {
    interaction
        .get("interaction_type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string()
}

/// Build one public conversion row from a repatriated fragment.
/// Requires `swap_tx` in metadata (or a parseable description JSON).
/// A knot without a swap hash is dropped - that is the honest path,
/// not a fabricated fill.
pub fn conversion_from_fragment(frag: &Value, interaction: &Value) -> Option<Value> {
    let meta = interaction.get("metadata").cloned().unwrap_or(json!({}));
    let desc = interaction.get("description").and_then(|d| d.as_str());
    let desc_json = desc.and_then(|d| serde_json::from_str::<Value>(d).ok());

    let swap_tx = meta
        .get("swap_tx")
        .and_then(|v| v.as_str())
        .or_else(|| {
            desc_json
                .as_ref()
                .and_then(|j| j.get("swap_tx"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();

    if !(swap_tx.starts_with("0x") && swap_tx.len() == 66) {
        return None;
    }

    let project_id = meta
        .get("project_id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            desc_json
                .as_ref()
                .and_then(|j| j.get("project_id"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();

    let utc_date = meta
        .get("utc_date")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let eta_bps = meta
        .get("eta_bps")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_ETA_FIAT_TO_FAT_BPS as u64);

    Some(json!({
        "interaction_type": KNOT_CONVERTED,
        "project_id": project_id,
        "utc_date": utc_date,
        "batch_id": meta.get("batch_id").cloned().unwrap_or(Value::Null),
        "eta_bps": eta_bps,
        "net_fiat_usd": meta.get("net_fiat_usd").cloned().unwrap_or(Value::Null),
        "convertible_usd": meta.get("convertible_usd").cloned().unwrap_or(Value::Null),
        "usdc_in": meta.get("usdc_in").cloned().unwrap_or(Value::Null),
        "wfat_out": meta.get("wfat_out").cloned().unwrap_or(Value::Null),
        "native_fat_out": meta.get("native_fat_out").cloned().unwrap_or(Value::Null),
        "treasury": meta.get("treasury").cloned().unwrap_or(Value::Null),
        "pair": meta.get("pair").cloned().unwrap_or(Value::Null),
        "router": meta.get("router").cloned().unwrap_or(Value::Null),
        "swap_tx": swap_tx,
        "unwrap_tx": meta.get("unwrap_tx").cloned().unwrap_or(Value::Null),
        "price_usd_canonical": meta.get("price_usd_canonical").cloned().unwrap_or(Value::Null),
        "knot_id": frag.get("knot_id").cloned()
            .or_else(|| frag.get("hash").cloned())
            .unwrap_or(Value::Null),
        "description": desc.unwrap_or(""),
    }))
}

fn apply_filters(mut body: Value, q: &ListQuery) -> Value {
    let Some(arr) = body.get("conversions").and_then(|c| c.as_array()).cloned() else {
        return body;
    };
    let filtered: Vec<Value> = arr
        .into_iter()
        .filter(|row| {
            if let Some(ref project) = q.project {
                let pid = row
                    .get("project_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !pid.eq_ignore_ascii_case(project) {
                    return false;
                }
            }
            let date = row.get("utc_date").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(ref from) = q.from {
                if !date.is_empty() && date < from.as_str() {
                    return false;
                }
            }
            if let Some(ref to) = q.to {
                if !date.is_empty() && date > to.as_str() {
                    return false;
                }
            }
            true
        })
        .collect();
    let live = filtered.iter().any(is_live_conversion);
    body["conversions"] = json!(filtered);
    body["count"] = json!(filtered.len());
    if let Some(p) = &q.project {
        body["project_filter"] = json!(p);
    }
    // Filters never flip `live` to true. They can only hide rows.
    if !live {
        body["live"] = json!(false);
        if body.get("phase").and_then(|v| v.as_str()) == Some("converting")
            && filtered.is_empty()
        {
            body["phase"] = json!("pending");
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_eta_is_fifty_percent_not_eighty() {
        assert!((DEFAULT_ETA_FIAT_TO_FAT - 0.50).abs() < 1e-12);
        assert_eq!(DEFAULT_ETA_FIAT_TO_FAT_BPS, 5000);
        assert!((DEFAULT_ETA_POOL_USDC - 0.10).abs() < 1e-12);
        assert_ne!(DEFAULT_ETA_FIAT_TO_FAT, 0.80);
        assert_ne!(DEFAULT_ETA_FIAT_TO_FAT_BPS, 8000);
    }

    #[test]
    fn ledger_is_d004() {
        assert_eq!(
            DEFAULT_LEDGER.to_lowercase(),
            "0x000000000000000000000000000000000000d004"
        );
        assert_ne!(DEFAULT_LEDGER.to_lowercase().as_str(), "0x000000000000000000000000000000000000d001");
        assert_ne!(DEFAULT_LEDGER.to_lowercase().as_str(), "0x000000000000000000000000000000000000d002");
        assert_ne!(DEFAULT_LEDGER.to_lowercase().as_str(), "0x000000000000000000000000000000000000d003");
        assert_ne!(DEFAULT_LEDGER.to_lowercase().as_str(), "0x000000000000000000000000000000000000d005");
    }

    #[test]
    fn pending_payload_is_honest_and_empty() {
        let p = pending_payload(DEFAULT_LEDGER, Vec::new());
        assert_eq!(p["live"], json!(false));
        assert_eq!(p["phase"], json!("pending"));
        assert_eq!(p["eta_fiat_to_fat"], json!(0.50));
        assert_eq!(p["eta_pool_usdc"], json!(0.10));
        assert_eq!(p["count"], json!(0));
        assert!(p["conversions"].as_array().unwrap().is_empty());
        let note = p["note"].as_str().unwrap();
        assert!(note.contains("Phase 1 not activated"));
        assert!(note.contains("50%"));
        assert!(note.contains("not 80%"));
        assert!(!note.to_lowercase().contains("buybacks are live"));
        assert!(!note.to_lowercase().contains("currently converting"));
    }

    #[test]
    fn conversion_without_swap_tx_is_dropped() {
        let interaction = json!({
            "interaction_type": "FiatRevenueConvertedToFat",
            "description": "would-be conversion without a swap",
            "metadata": {
                "project_id": "tanastok",
                "utc_date": "2026-08-16",
                "eta_bps": 5000
            }
        });
        let frag = json!({ "knot_id": "0xabc" });
        assert!(conversion_from_fragment(&frag, &interaction).is_none());
    }

    #[test]
    fn conversion_with_real_swap_tx_is_kept() {
        let swap = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let interaction = json!({
            "interaction_type": "FiatRevenueConvertedToFat",
            "description": "Tanastok subscription fiat net converted to DC FAT via DCSwap FAT/USDC.",
            "metadata": {
                "schema": "datachain.fiat-revenue-fat/v1",
                "project_id": "tanastok",
                "utc_date": "2026-08-16",
                "batch_id": "batch-1",
                "eta_bps": 5000,
                "net_fiat_usd": "1041.00",
                "swap_tx": swap,
                "pair": "0xd9ebc3da001618a3ae90481d33ae7ef85e130317"
            }
        });
        let frag = json!({ "knot_id": "0xknot" });
        let row = conversion_from_fragment(&frag, &interaction).expect("kept");
        assert_eq!(row["swap_tx"], json!(swap));
        assert_eq!(row["project_id"], json!("tanastok"));
        assert_eq!(row["eta_bps"], json!(5000));
        assert!(is_live_conversion(&row));
    }

    #[test]
    fn pending_payload_flips_live_only_when_a_swap_exists() {
        let swap = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let row = json!({
            "project_id": "tanastok",
            "utc_date": "2026-08-16",
            "swap_tx": swap
        });
        let p = pending_payload(DEFAULT_LEDGER, vec![row]);
        assert_eq!(p["live"], json!(true));
        assert_eq!(p["phase"], json!("converting"));
        assert_eq!(p["count"], json!(1));
    }

    #[test]
    fn project_filter_hides_other_projects() {
        let swap = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let tan = json!({"project_id":"tanastok","utc_date":"2026-08-10","swap_tx":swap});
        let care = json!({"project_id":"careaway","utc_date":"2026-08-11","swap_tx":swap});
        let body = pending_payload(DEFAULT_LEDGER, vec![tan, care]);
        let q = ListQuery {
            project: Some("tanastok".into()),
            from: None,
            to: None,
        };
        let out = apply_filters(body, &q);
        assert_eq!(out["count"], json!(1));
        assert_eq!(out["conversions"][0]["project_id"], json!("tanastok"));
        assert_eq!(out["project_filter"], json!("tanastok"));
    }

    #[test]
    fn date_filter_from_to() {
        let swap = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        let a = json!({"project_id":"tanastok","utc_date":"2026-08-01","swap_tx":swap});
        let b = json!({"project_id":"tanastok","utc_date":"2026-08-15","swap_tx":swap});
        let c = json!({"project_id":"tanastok","utc_date":"2026-08-20","swap_tx":swap});
        let body = pending_payload(DEFAULT_LEDGER, vec![a, b, c]);
        let q = ListQuery {
            project: None,
            from: Some("2026-08-10".into()),
            to: Some("2026-08-16".into()),
        };
        let out = apply_filters(body, &q);
        assert_eq!(out["count"], json!(1));
        assert_eq!(out["conversions"][0]["utc_date"], json!("2026-08-15"));
    }
}
