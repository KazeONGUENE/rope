// =============================================================================
// HTTP server — the inbound surface of the ComplianceAgent
// =============================================================================
//
// Routes:
//
//   POST /v1/gdpr/article17     — submit a GDPR Art. 17 erasure request
//   POST /v1/mifid/event        — submit a MiFID II trade event for batching
//   POST /v1/dora/incident      — submit a DORA ICT incident for batching
//   POST /v1/admin/flush-now    — force a reporter tick (operator)
//   GET  /v1/health             — JSON status snapshot
//   GET  /metrics               — Prometheus text format
//
// All POST handlers respond with structured JSON. The Art. 17 handler
// also blocks on the orchestrator + anchor — it returns the
// `ComplianceTestimonyEnvelope` and the rope-node anchor receipt in a
// single response, so the caller has the full audit trail in hand by
// the time their HTTP request completes.
// =============================================================================

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::anchor::AnchorClient;
use crate::gdpr::{
    Article17Request, Article17Validator, Article17Verdict, RejectionReason,
};
use crate::metrics::ComplianceMetrics;
use crate::orchestrator::UntieOrchestrator;
use crate::reporting::{PeriodicReporter, ReporterStats};
use crate::testimony::{
    ComplianceTestimony, ComplianceTestimonyEnvelope, DoraIncident, GdprArticle17Testimony,
    MiFidIIEvent,
};

/// State shared across all HTTP handlers. Cheap to clone.
#[derive(Clone)]
pub struct ServerState {
    pub validator: Arc<Article17Validator>,
    pub orchestrator: Arc<UntieOrchestrator>,
    pub anchor: AnchorClient,
    pub reporter: PeriodicReporter,
    pub metrics: Arc<ComplianceMetrics>,
    pub agent_wallet: String,
    pub agent_id: String,
}

pub fn build_router(state: ServerState) -> Router {
    Router::new()
        .route("/v1/gdpr/article17", post(handle_article17))
        .route("/v1/mifid/event", post(handle_mifid_event))
        .route("/v1/dora/incident", post(handle_dora_incident))
        .route("/v1/admin/flush-now", post(handle_flush_now))
        .route("/v1/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Article 17 handler
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Article17Response {
    pub verdict: Article17Verdict,
    /// Present iff the verdict was Approved AND orchestration was attempted.
    /// (It will still be present on partial-failure so the caller has the
    /// per-knot audit trail.)
    pub orchestration: Option<crate::orchestrator::OrchestrationReport>,
    pub testimony: GdprArticle17Testimony,
    pub anchor_receipt: Option<crate::anchor::AnchorReceipt>,
    pub anchor_error: Option<String>,
}

async fn handle_article17(
    State(state): State<ServerState>,
    Json(mut req): Json<Article17Request>,
) -> Response {
    state.metrics.gdpr_requests_total.inc();

    let verdict = state.validator.validate(&mut req);
    let now = chrono::Utc::now().timestamp();

    if !verdict.is_approved() {
        state.metrics.gdpr_requests_rejected.inc();
        let testimony = GdprArticle17Testimony::from_rejected(&req, &verdict, &state.agent_id, now);
        let envelope = ComplianceTestimonyEnvelope::seal(
            state.agent_id.clone(),
            state.agent_wallet.clone(),
            ComplianceTestimony::GdprArticle17(testimony.clone()),
            now,
        );
        let (anchor_receipt, anchor_error) = match state.anchor.anchor(&envelope).await {
            Ok(r) => {
                state.metrics.gdpr_testimony_anchored.inc();
                (Some(r), None)
            }
            Err(e) => {
                state.metrics.gdpr_testimony_anchor_failed.inc();
                (None, Some(e.to_string()))
            }
        };
        let body = Article17Response {
            verdict: verdict.clone(),
            orchestration: None,
            testimony,
            anchor_receipt,
            anchor_error,
        };
        let status = match status_for_rejection(&verdict) {
            Some(s) => s,
            None => StatusCode::BAD_REQUEST,
        };
        return (status, Json(body)).into_response();
    }

    state.metrics.gdpr_requests_approved.inc();

    // Approved — orchestrate the per-knot untying.
    let report = match state.orchestrator.execute(&req, &verdict).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    state
        .metrics
        .gdpr_knots_untied_success
        .inc_by(report.success_count as u64);
    state
        .metrics
        .gdpr_knots_untied_failure
        .inc_by(report.failure_count as u64);

    let testimony =
        GdprArticle17Testimony::from_processed(&req, &verdict, &report, &state.agent_id, now);
    let envelope = ComplianceTestimonyEnvelope::seal(
        state.agent_id.clone(),
        state.agent_wallet.clone(),
        ComplianceTestimony::GdprArticle17(testimony.clone()),
        now,
    );
    let (anchor_receipt, anchor_error) = match state.anchor.anchor(&envelope).await {
        Ok(r) => {
            state.metrics.gdpr_testimony_anchored.inc();
            (Some(r), None)
        }
        Err(e) => {
            state.metrics.gdpr_testimony_anchor_failed.inc();
            tracing::error!(
                target: "compliance::server",
                request_id = %report.request_id,
                error = %e,
                "GdprArticle17 testimony anchor FAILED — orchestrated knots are untied but the audit knot is missing"
            );
            (None, Some(e.to_string()))
        }
    };

    let body = Article17Response {
        verdict,
        orchestration: Some(report),
        testimony,
        anchor_receipt,
        anchor_error,
    };
    (StatusCode::OK, Json(body)).into_response()
}

fn status_for_rejection(v: &Article17Verdict) -> Option<StatusCode> {
    match v {
        Article17Verdict::Rejected { reason_code, .. } => Some(match reason_code {
            RejectionReason::SchemaInvalid
            | RejectionReason::ProofMissing
            | RejectionReason::ProofMalformed
            | RejectionReason::ProofTooShort
            | RejectionReason::SubjectWalletMalformed
            | RejectionReason::KnotIdMalformed
            | RejectionReason::NoKnotsRequested
            | RejectionReason::TooManyKnots => StatusCode::BAD_REQUEST,
            RejectionReason::JurisdictionNotAllowed => StatusCode::FORBIDDEN,
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// MiFID II + DORA submission handlers
// ---------------------------------------------------------------------------

async fn handle_mifid_event(
    State(state): State<ServerState>,
    Json(event): Json<MiFidIIEvent>,
) -> Json<Value> {
    state.metrics.mifid_events_received.inc();
    state.reporter.record_mifid_event(event);
    Json(json!({"accepted": true}))
}

async fn handle_dora_incident(
    State(state): State<ServerState>,
    Json(incident): Json<DoraIncident>,
) -> Json<Value> {
    state.metrics.dora_incidents_received.inc();
    state.reporter.record_dora_incident(incident);
    Json(json!({"accepted": true}))
}

async fn handle_flush_now(State(state): State<ServerState>) -> Json<Value> {
    state.reporter.flush_now();
    Json(json!({"flush_now_signal_sent": true}))
}

// ---------------------------------------------------------------------------
// Health + metrics
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
    agent_id: String,
    agent_wallet: String,
    reporter: ReporterStats,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct EmptyBody {}

async fn handle_health(State(state): State<ServerState>) -> Json<HealthBody> {
    Json(HealthBody {
        status: "ok",
        agent_id: state.agent_id.clone(),
        agent_wallet: state.agent_wallet.clone(),
        reporter: state.reporter.buffer_stats(),
    })
}

async fn handle_metrics(State(state): State<ServerState>) -> Response {
    let body = state.metrics.render();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GdprPolicy;
    use crate::gdpr::JustificationClass;
    use crate::rpc::testing::MockRopeRpcClient;
    use crate::rpc::RopeRpcClient;
    use std::time::Duration;

    use axum::body::{to_bytes, Body};
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    fn approving_policy() -> GdprPolicy {
        GdprPolicy {
            allowed_jurisdictions: Default::default(),
            require_requestor_proof: true,
            min_proof_bytes: 32,
            max_knots_per_request: 16,
        }
    }

    fn build_state(mock: Arc<MockRopeRpcClient>) -> ServerState {
        let validator = Arc::new(Article17Validator::new(approving_policy()));
        let orchestrator =
            Arc::new(UntieOrchestrator::new(mock.clone() as Arc<dyn RopeRpcClient>));
        let anchor = AnchorClient::new(mock as Arc<dyn RopeRpcClient>, "0xC005");
        let reporter = PeriodicReporter::new(anchor.clone(), Duration::from_secs(60), 1024);
        ServerState {
            validator,
            orchestrator,
            anchor,
            reporter,
            metrics: Arc::new(ComplianceMetrics::new()),
            agent_wallet: "0xC005".to_string(),
            agent_id: "compliance".to_string(),
        }
    }

    #[tokio::test]
    async fn article17_happy_path_returns_envelope_and_receipt() {
        let mock = Arc::new(MockRopeRpcClient::new());
        // 1 untie + 1 anchor.
        mock.enqueue_ok(
            "rope_untieKnot",
            json!({
                "tombstone_audit_hash": "0xabc",
                "untied_at": 1700000000i64,
                "knots_remaining": 4u64,
                "tombstones_total": 1u64,
            }),
        );
        mock.enqueue_ok(
            "rope_appendToLedger",
            json!({"index": 1, "hash": "0xtestknot"}),
        );

        let app = build_router(build_state(mock));
        let body = json!({
            "request_id": "req-int-1",
            "subject_wallet": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "requestor_proof": format!("0x{}", "ab".repeat(32)),
            "justification": "consent_withdrawn",
            "affected_knots": [format!("0x{}", "11".repeat(32))],
            "jurisdiction": 250,
            "note": "test"
        });
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/gdpr/article17")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v.get("orchestration").unwrap().get("success_count").unwrap(),
            &json!(1)
        );
        assert_eq!(
            v.get("anchor_receipt")
                .unwrap()
                .get("knot_string_id")
                .unwrap()
                .as_str()
                .unwrap(),
            "0xtestknot"
        );
        assert!(v.get("anchor_error").unwrap().is_null());
    }

    #[tokio::test]
    async fn article17_rejection_still_anchors_and_returns_400() {
        let mock = Arc::new(MockRopeRpcClient::new());
        mock.enqueue_ok(
            "rope_appendToLedger",
            json!({"index": 1, "hash": "0xrejknot"}),
        );
        let app = build_router(build_state(mock));
        let body = json!({
            "request_id": "req-rej-1",
            "subject_wallet": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "requestor_proof": "0x", // missing
            "justification": "consent_withdrawn",
            "affected_knots": [format!("0x{}", "11".repeat(32))],
            "jurisdiction": 250
        });
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/gdpr/article17")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v.get("verdict").unwrap().get("decision").unwrap().as_str().unwrap(),
            "rejected"
        );
        assert_eq!(
            v.get("anchor_receipt").unwrap().get("knot_string_id").unwrap().as_str().unwrap(),
            "0xrejknot"
        );
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_text_format() {
        let mock = Arc::new(MockRopeRpcClient::new());
        let app = build_router(build_state(mock));
        let req = Request::builder()
            .method(Method::GET)
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let txt = std::str::from_utf8(&bytes).unwrap();
        assert!(txt.contains("gdpr_requests_total"));
        assert!(txt.contains("mifid_events_received"));
        assert!(txt.contains("dora_digests_anchored"));
    }

    #[tokio::test]
    async fn health_endpoint_returns_reporter_stats() {
        let mock = Arc::new(MockRopeRpcClient::new());
        let app = build_router(build_state(mock));
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v.get("status").unwrap().as_str().unwrap(), "ok");
        assert_eq!(v.get("agent_id").unwrap().as_str().unwrap(), "compliance");
        assert_eq!(v.get("agent_wallet").unwrap().as_str().unwrap(), "0xC005");
    }

    #[tokio::test]
    async fn mifid_event_flows_into_buffer_and_increments_metric() {
        let mock = Arc::new(MockRopeRpcClient::new());
        let state = build_state(mock);
        let metrics = state.metrics.clone();
        let reporter = state.reporter.clone();
        let app = build_router(state);

        let body = json!({
            "trade_id": "t1",
            "instrument": "DC-FAT",
            "venue": "dcswap",
            "buyer": "0xbuyer",
            "seller": "0xseller",
            "notional": 1000,
            "currency": "USDC",
            "executed_at": 1700000000
        });
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/mifid/event")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(metrics.mifid_events_received.get(), 1);
        let stats = reporter.buffer_stats();
        assert_eq!(stats.mifid_pending, 1);
    }

    // Cross-check: a request with an unsupported justification still
    // serialises (just to make sure JustificationClass round-trips).
    #[test]
    fn justification_serialisation_round_trip() {
        let j = JustificationClass::ConsentWithdrawn;
        let s = serde_json::to_string(&j).unwrap();
        assert_eq!(s, "\"consent_withdrawn\"");
        let back: JustificationClass = serde_json::from_str(&s).unwrap();
        assert_eq!(back, j);
    }
}
