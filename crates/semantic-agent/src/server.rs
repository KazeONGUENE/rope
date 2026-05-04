//! Axum HTTP server exposing:
//!
//! - `GET /v1/health` — liveness probe
//! - `GET /v1/metrics` — agent counters (JSON)
//! - `GET /v1/search` — full search API
//!   (`q`, `event_type`, `string_kind`, `string_id`, `status`, `from`,
//!   `to`, `limit`)
//! - `GET /v1/checkpoint` — current merkle root + total indexed
//!   (read-only — does NOT anchor; use the POST endpoint for that)
//! - `POST /v1/checkpoint` — force-build-and-anchor a checkpoint right
//!   now (operational tool; respects the `read_only` config flag)
//!
//! All responses are JSON. Errors are returned as
//! `{ "error": "...", "kind": "..." }` with appropriate HTTP status.

use crate::checkpoint::CheckpointBuilder;
use crate::search::SearchQuery;
use crate::SemanticAgent;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

/// Wire-shape for `GET /v1/search`. Mirrors [`SearchQuery`] one-for-one
/// but every field is optional and parsed from URL query parameters.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub event_type: Option<String>,
    pub string_kind: Option<String>,
    pub string_id: Option<String>,
    pub status: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: Option<u32>,
}

impl From<SearchParams> for SearchQuery {
    fn from(p: SearchParams) -> Self {
        SearchQuery {
            q: p.q,
            event_type: p.event_type,
            string_kind: p.string_kind,
            string_id: p.string_id,
            status: p.status,
            from: p.from,
            to: p.to,
            limit: p.limit,
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
    kind: &'static str,
}

fn err500(e: anyhow::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: format!("{e:#}"),
            kind: "internal",
        }),
    )
        .into_response()
}

/// Wire-shape for `GET /v1/search` response.
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub total: usize,
    pub hits: Vec<crate::search::SearchHit>,
    pub query: serde_json::Value,
}

async fn handle_search(
    State(agent): State<Arc<SemanticAgent>>,
    Query(params): Query<SearchParams>,
) -> Response {
    let echoed = serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);
    let query: SearchQuery = params.into();
    match agent.search.search(&query) {
        Ok(hits) => {
            {
                let mut m = agent.metrics.write();
                m.search_count = m.search_count.saturating_add(1);
            }
            (
                StatusCode::OK,
                Json(SearchResponse {
                    total: hits.len(),
                    hits,
                    query: echoed,
                }),
            )
                .into_response()
        }
        Err(e) => err500(e),
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    agent_id: String,
    wallet: String,
    indexed_count: u64,
    rpc_url: String,
}

async fn handle_health(State(agent): State<Arc<SemanticAgent>>) -> impl IntoResponse {
    Json(HealthResponse {
        ok: true,
        agent_id: agent.config.identity.agent_id.clone(),
        wallet: agent.config.identity.wallet.clone(),
        indexed_count: agent.search.doc_count(),
        rpc_url: agent.config.rpc_url.clone(),
    })
}

async fn handle_metrics(State(agent): State<Arc<SemanticAgent>>) -> impl IntoResponse {
    let metrics = agent.metrics();
    let stats = agent.indexer.stats();
    let body = serde_json::json!({
        "agent_id": agent.config.identity.agent_id,
        "wallet": agent.config.identity.wallet,
        "indexed_count_index": agent.search.doc_count(),
        "indexed_count_lifetime": metrics.indexed_count,
        "search_count": metrics.search_count,
        "checkpoint_count": metrics.checkpoint_count,
        "indexer_errors": metrics.indexer_errors,
        "anchor_errors": metrics.anchor_errors,
        "last_indexed_at": metrics.last_indexed_at,
        "last_indexed_knot_id": metrics.last_indexed_knot_id,
        "last_indexed_string_id": metrics.last_indexed_string_id,
        "last_checkpoint_at": metrics.last_checkpoint_at,
        "last_checkpoint_root": metrics.last_checkpoint_root,
        "last_checkpoint_total_indexed": metrics.last_checkpoint_total_indexed,
        "last_anchor_knot_id": metrics.last_anchor_knot_id,
        "last_anchor_at": metrics.last_anchor_at,
        "indexer_strings_seen": stats.strings_seen,
        "indexer_knots_seen": stats.knots_seen,
        "rpc_url": agent.rpc_url(),
    });
    Json(body)
}

async fn handle_checkpoint_get(State(agent): State<Arc<SemanticAgent>>) -> Response {
    let last_string_id = agent.metrics.read().last_indexed_string_id.clone();
    let builder = CheckpointBuilder::new(agent.config.clone(), agent.search.clone());
    match builder.build(last_string_id) {
        Ok((testimony, _root)) => (StatusCode::OK, Json(testimony)).into_response(),
        Err(e) => err500(e),
    }
}

async fn handle_checkpoint_post(State(agent): State<Arc<SemanticAgent>>) -> Response {
    let last_string_id = agent.metrics.read().last_indexed_string_id.clone();
    match agent.anchor.build_and_anchor(last_string_id).await {
        Ok(Some(outcome)) => (StatusCode::OK, Json(outcome)).into_response(),
        Ok(None) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "skipped": true,
                "reason": "agent is in read_only mode"
            })),
        )
            .into_response(),
        Err(e) => err500(e),
    }
}

impl SemanticAgent {
    /// Helper for `/v1/metrics` — exposes the configured RPC URL.
    pub fn rpc_url(&self) -> String {
        self.config.rpc_url.clone()
    }
}

/// Build the axum router. Returns the router so callers can either
/// `axum::serve(listener, router).await` or run an in-process
/// `tower::ServiceExt::oneshot` against it (used by tests).
pub fn build_router(agent: Arc<SemanticAgent>) -> Router {
    Router::new()
        .route("/v1/health", get(handle_health))
        .route("/v1/metrics", get(handle_metrics))
        .route("/v1/search", get(handle_search))
        .route("/v1/checkpoint", get(handle_checkpoint_get))
        .route("/v1/checkpoint", post(handle_checkpoint_post))
        .with_state(agent)
        .layer(CorsLayer::permissive())
}

/// Run the HTTP server on `config.listen_addr`. Blocks until the server
/// exits.
pub async fn serve(agent: Arc<SemanticAgent>) -> anyhow::Result<()> {
    let listen_addr = agent.config.listen_addr.clone();
    let router = build_router(agent);
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!(addr = %listen_addr, "SemanticAgent HTTP server listening");
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::KnotIndexEntry;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::time::Duration;
    use tower::util::ServiceExt;

    fn build_agent_with_seed_data() -> Arc<SemanticAgent> {
        let dir = tempfile::tempdir().unwrap().keep();
        let cfg = AgentConfig {
            rpc_url: "http://127.0.0.1:0".to_string(),
            rpc_timeout: Duration::from_secs(1),
            index_path: dir,
            listen_addr: "127.0.0.1:0".to_string(),
            poll_interval: Duration::from_secs(60),
            checkpoint_interval: Duration::from_secs(60),
            list_strings_limit: 50,
            max_knots_per_poll: 5_000,
            identity: crate::config::AgentIdentity::default(),
            read_only: true,
        };
        let agent = Arc::new(SemanticAgent::new(cfg).unwrap());
        // Seed deterministic knots so the search route has hits.
        let entries: Vec<KnotIndexEntry> = (0..3)
            .map(|i| KnotIndexEntry {
                knot_id: format!("0x{:064x}", i + 1),
                string_id: "0xowner-A".into(),
                string_kind: "wallet".into(),
                event_type: if i == 0 {
                    "Transfer"
                } else {
                    "TestimonySubmission"
                }
                .into(),
                knot_index: i as u64,
                status: "active".into(),
                indexed_at: 1_700_000_000 + i as i64,
                knot_timestamp: 1_700_000_000 + i as i64,
                payload_text: format!("knot {i} payload"),
                payload_size: 0,
            })
            .collect();
        agent.search.index_entries(&entries).unwrap();
        agent
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let agent = build_agent_with_seed_data();
        let app = build_router(agent);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["agent_id"], "semantic");
        assert_eq!(v["indexed_count"], 3);
    }

    #[tokio::test]
    async fn search_endpoint_filters_by_event_type() {
        let agent = build_agent_with_seed_data();
        let app = build_router(agent);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/search?event_type=TestimonySubmission")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let total = v["total"].as_u64().unwrap();
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn search_endpoint_full_text_query() {
        let agent = build_agent_with_seed_data();
        let app = build_router(agent);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/search?q=payload")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["total"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn checkpoint_get_returns_merkle_root() {
        let agent = build_agent_with_seed_data();
        let app = build_router(agent);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/checkpoint")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["agent_id"], "semantic");
        assert_eq!(v["total_indexed"], 3);
        let root = v["merkle_root"].as_str().unwrap();
        assert!(root.starts_with("0x") && root.len() == 66);
    }

    #[tokio::test]
    async fn metrics_endpoint_reports_index_size() {
        let agent = build_agent_with_seed_data();
        let app = build_router(agent);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["indexed_count_index"], 3);
        assert_eq!(v["agent_id"], "semantic");
    }
}
