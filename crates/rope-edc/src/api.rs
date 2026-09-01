//! HTTP API - spec v2.0 §7.
//!
//! Three surfaces on one Axum router:
//!
//! * **Console API** (`/api/v1/ecosystem/*`) - the nine-step wizard, the
//!   live dashboard, inventory bulk import, AI analytics, and grant
//!   administration. Caller identity is the `X-Edc-Wallet` header; role
//!   checks run against the project team. When `EDC_CONSOLE_TOKEN` is set,
//!   every console request must additionally carry it in
//!   `X-Edc-Console-Token` (defense in depth for consoles exposed beyond
//!   the owner's own network).
//! * **Stakeholder gateway** (`/api/v1/ecosystem/stakeholder/*`) -
//!   disintermediated access for regulators / investors / buyers /
//!   the public. Authenticated by grant-minted bearer tokens OR an
//!   EIP-191 wallet signature (`X-Edc-Address` + `X-Edc-Timestamp` +
//!   `X-Edc-Signature` headers, spec v1.0 §6.3); every response is
//!   filtered to the grant scope; every request is metered on the grant.
//!   Sandbox keys (`edc_sbx_…`) are served from the project's
//!   deterministic synthetic stream and never metered.
//! * **Public directory** (`/api/v1/ecosystem/public/*`) - unauthenticated
//!   project cards for dcscan.io + QR/NFC tag resolution.

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json,
    },
    routing::{delete, get, post, put},
    Router,
};
use futures::stream::Stream;
use serde::Deserialize;
use serde_json::json;

use crate::ai::AiAnalytics;
use crate::csv_import;
use crate::grants::{
    mint_key, AccessGrant, GrantPrice, GrantScope, Grantee, StakeholderClass,
};
use crate::registry::Registry;
use crate::types::{
    classify_band, now_ts, AiAgentConfig, ApprovalEvent, AssetRecord,
    CryptoAssetConfig, DiagnosisEvent, ExternalSource, IdentityInfo, MeshNode,
    MutabilityPolicy, NodePlan, Project, ProjectDefinition, ProjectStatus,
    ReportRecord, Role, SensorRecord, TeamMember, TelemetryReading,
};
use crate::{billing, export, graphql, provision, reports, session, simulation, walletsig};

/// Default Timelock delay for regulator/public grants: 1 hour, matching the
/// DCSwapTimelock production minDelay.
fn timelock_delay_secs() -> i64 {
    std::env::var("EDC_TIMELOCK_DELAY_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_600)
}

pub struct AppState {
    pub registry: Arc<Registry>,
    pub ai: Arc<AiAnalytics>,
    /// Shared cloud-provider registry (Exoscale + DigitalOcean + Local).
    /// Built once at startup from the environment so every `/nodes`
    /// route reuses the same in-memory state files and the same
    /// reqwest client pool.
    pub deployer: rope_deployer::ProviderRegistry,
}

pub(crate) type SharedState = Arc<AppState>;

pub fn router(state: SharedState) -> Router {
    Router::new()
        // -- console: wallet-signature sign-in (hosted console auth) --
        .route("/api/v1/ecosystem/auth/session", post(create_session).delete(delete_session))
        // -- console: projects & wizard steps --
        .route("/api/v1/ecosystem/projects", post(create_project).get(list_projects))
        .route("/api/v1/ecosystem/projects/:id", get(get_project))
        .route("/api/v1/ecosystem/projects/:id/identity", put(put_identity))
        .route("/api/v1/ecosystem/projects/:id/definition", put(put_definition))
        .route("/api/v1/ecosystem/projects/:id/crypto", put(put_crypto))
        .route("/api/v1/ecosystem/projects/:id/team", put(put_team))
        .route("/api/v1/ecosystem/projects/:id/policy", put(put_policy))
        .route("/api/v1/ecosystem/projects/:id/inventory/assets", post(post_assets))
        .route("/api/v1/ecosystem/projects/:id/inventory/assets/import", post(import_assets_csv))
        .route("/api/v1/ecosystem/projects/:id/inventory/sensors", post(post_sensors))
        .route("/api/v1/ecosystem/projects/:id/inventory/sensors/import", post(import_sensors_csv))
        .route("/api/v1/ecosystem/projects/:id/inventory/mesh", post(post_mesh))
        .route("/api/v1/ecosystem/projects/:id/inventory/external", post(post_external))
        .route("/api/v1/ecosystem/projects/:id/inventory/agents", post(post_agents))
        .route("/api/v1/ecosystem/projects/:id/node-plan", get(get_node_plan))
        .route("/api/v1/ecosystem/projects/:id/deploy", post(deploy_project))
        .route("/api/v1/ecosystem/projects/:id/status", put(put_status))
        // -- console: bare-node deployment (Deploy a Node wizard) --
        .route("/api/v1/ecosystem/nodes", post(crate::nodes::provision_node).get(crate::nodes::list_nodes))
        .route("/api/v1/ecosystem/nodes/:provider/:id", delete(crate::nodes::destroy_node))
        .route("/api/v1/ecosystem/providers", get(crate::nodes::list_providers))
        // -- console: live data --
        .route("/api/v1/ecosystem/projects/:id/telemetry", post(post_telemetry))
        .route("/api/v1/ecosystem/projects/:id/diagnosis", post(post_diagnosis))
        .route("/api/v1/ecosystem/projects/:id/approvals", post(post_approval))
        .route("/api/v1/ecosystem/projects/:id/readings", get(get_readings))
        .route("/api/v1/ecosystem/projects/:id/diagnoses", get(get_diagnoses))
        .route("/api/v1/ecosystem/projects/:id/approvals", get(get_approvals))
        // -- console: AI analytics --
        .route("/api/v1/ecosystem/projects/:id/ask", post(ask_project))
        .route("/api/v1/ecosystem/projects/:id/analytics/dossier", get(get_dossier))
        // -- console: sandbox / simulation (spec v1.0 §6.3) --
        .route("/api/v1/ecosystem/projects/:id/simulate/backfill", post(simulate_backfill))
        // -- console: scheduled reports (spec v1.0 §6.4) --
        .route("/api/v1/ecosystem/projects/:id/report-schedule", put(put_report_schedule))
        .route("/api/v1/ecosystem/projects/:id/reports", post(generate_report_now).get(list_reports))
        // -- console: grants --
        .route("/api/v1/ecosystem/projects/:id/grants", post(create_grant).get(list_grants))
        .route("/api/v1/ecosystem/grants/:gid/keys", post(mint_grant_key))
        .route("/api/v1/ecosystem/grants/:gid", delete(revoke_grant_route))
        .route("/api/v1/ecosystem/grants/:gid/export-schedule", put(put_export_schedule))
        .route("/api/v1/ecosystem/grants/:gid/billing", get(get_billing_statement))
        .route("/api/v1/ecosystem/grants/:gid/billing/close", post(close_billing_statement))
        // -- stakeholder gateway (bearer token or EIP-191 wallet signature) --
        .route("/api/v1/ecosystem/stakeholder/overview", get(sh_overview))
        .route("/api/v1/ecosystem/stakeholder/readings", get(sh_readings))
        .route("/api/v1/ecosystem/stakeholder/diagnoses", get(sh_diagnoses))
        .route("/api/v1/ecosystem/stakeholder/approvals", get(sh_approvals))
        .route("/api/v1/ecosystem/stakeholder/stream", get(sh_stream))
        .route("/api/v1/ecosystem/stakeholder/ask", post(sh_ask))
        .route("/api/v1/ecosystem/stakeholder/graphql", post(sh_graphql))
        .route("/api/v1/ecosystem/stakeholder/exports", get(sh_list_exports))
        .route("/api/v1/ecosystem/stakeholder/exports/:filename", get(sh_download_export))
        // -- public directory (dcscan.io) + QR/NFC tag resolution --
        .route("/api/v1/ecosystem/public/projects", get(public_projects))
        .route("/api/v1/ecosystem/public/projects/:id", get(public_project))
        .route("/api/v1/ecosystem/public/tags/:tag_id", get(resolve_tag))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Errors & auth helpers
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct ApiError(StatusCode, String);

impl ApiError {
    /// Public-in-crate constructor so sibling modules (e.g. `nodes`)
    /// can build the same JSON error shape without duplicating the
    /// tuple layout.
    pub(crate) fn new(status: StatusCode, msg: String) -> Self {
        Self(status, msg)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}
fn forbidden(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, msg.into())
}
fn unauthorized(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::UNAUTHORIZED, msg.into())
}

/// Console caller identity.
///
/// Resolution order (first match wins):
///
/// 1. **Session token** (`X-Edc-Session`) minted by `POST
///    /api/v1/ecosystem/auth/session` after an EIP-191 console sign-in.
///    This is the path the hosted console UI uses.
/// 2. **Per-request EIP-191 signature** - `X-Edc-Address` +
///    `X-Edc-Timestamp` + `X-Edc-Signature` over the
///    `EDC-CONSOLE-AUTH` domain message (for scripted/API callers that
///    prefer not to hold a session).
/// 3. **Bare `X-Edc-Wallet` header** - only when
///    `EDC_CONSOLE_REQUIRE_SIGNATURE` is not enabled. Suitable for
///    self-hosted single-operator instances; MUST be disabled on any
///    publicly reachable console.
///
/// Independently of the above, when `EDC_CONSOLE_TOKEN` is set every
/// console request must additionally carry it in `X-Edc-Console-Token`.
pub(crate) fn console_wallet(headers: &HeaderMap) -> Result<String, ApiError> {
    if let Ok(expected) = std::env::var("EDC_CONSOLE_TOKEN") {
        if !expected.is_empty() {
            let presented = headers
                .get("x-edc-console-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            // Constant-time compare.
            let ok = presented.len() == expected.len()
                && presented
                    .bytes()
                    .zip(expected.bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0;
            if !ok {
                return Err(unauthorized("missing or invalid console token"));
            }
        }
    }

    // 1. Session token from a prior EIP-191 sign-in.
    if let Some(token) = headers.get("x-edc-session").and_then(|v| v.to_str().ok()) {
        return session::resolve(token, now_ts())
            .ok_or_else(|| unauthorized("session expired or unknown - sign in again"));
    }

    // 2. Per-request EIP-191 signature on the console domain.
    if let (Some(address), Some(ts), Some(sig)) = (
        headers.get("x-edc-address").and_then(|v| v.to_str().ok()),
        headers.get("x-edc-timestamp").and_then(|v| v.to_str().ok()),
        headers.get("x-edc-signature").and_then(|v| v.to_str().ok()),
    ) {
        let ts: i64 = ts
            .parse()
            .map_err(|_| unauthorized("X-Edc-Timestamp must be unix seconds"))?;
        return walletsig::verify_domain(walletsig::DOMAIN_CONSOLE, address, ts, sig, now_ts())
            .map_err(|e| unauthorized(format!("console signature rejected: {e}")));
    }

    // 3. Bare wallet header - allowed only when signatures are not enforced.
    if console_signature_required() {
        return Err(unauthorized(
            "this console requires wallet-signature sign-in (X-Edc-Session or \
             X-Edc-Address/X-Edc-Timestamp/X-Edc-Signature)",
        ));
    }
    headers
        .get("x-edc-wallet")
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.starts_with("0x") && s.len() == 42)
        .map(|s| s.to_lowercase())
        .ok_or_else(|| unauthorized("X-Edc-Wallet header (0x… address) required"))
}

/// `EDC_CONSOLE_REQUIRE_SIGNATURE=1|true|yes|on` disables the bare
/// `X-Edc-Wallet` fallback. Mandatory on publicly hosted consoles.
fn console_signature_required() -> bool {
    std::env::var("EDC_CONSOLE_REQUIRE_SIGNATURE")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

#[derive(Deserialize)]
struct CreateSessionBody {
    /// The wallet claiming the sign-in (`0x…`). EIP-191 path.
    #[serde(default)]
    address: Option<String>,
    /// Unix seconds, must be within ±300 s of server time. EIP-191 path.
    #[serde(default)]
    timestamp: Option<i64>,
    /// 65-byte hex EIP-191 signature over
    /// `EDC-CONSOLE-AUTH\n{address_lowercase}\n{timestamp}`. EIP-191 path.
    #[serde(default)]
    signature: Option<String>,
    /// Datachain ID (id.datachain.network) bearer token - the Datawallet+
    /// credential sign-in path. Verified server-side against the identity
    /// gateway's introspection endpoint; the session is issued for the
    /// account's bound primary on-chain address.
    #[serde(default)]
    datachain_id_token: Option<String>,
}

/// Datachain ID gateway base URL (override with `EDC_IDP_URL`).
fn idp_url() -> String {
    std::env::var("EDC_IDP_URL").unwrap_or_else(|_| "https://id.datachain.network".to_string())
}

fn idp_http() -> &'static reqwest::Client {
    static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

/// Verify a Datachain ID bearer token via `POST {idp}/v1/auth/introspect`
/// and return the account's bound primary on-chain address. The console
/// keys every project on a wallet, so an account without a bound wallet
/// cannot hold a console session yet.
async fn verify_datachain_id_token(token: &str) -> Result<String, ApiError> {
    let url = format!("{}/v1/auth/introspect", idp_url());
    let res = idp_http()
        .post(&url)
        .json(&json!({ "token": token }))
        .send()
        .await
        .map_err(|e| {
            ApiError(
                StatusCode::BAD_GATEWAY,
                format!("identity gateway unreachable: {e}"),
            )
        })?;
    let body: serde_json::Value = res.json().await.map_err(|e| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            format!("identity gateway returned malformed response: {e}"),
        )
    })?;
    if body.get("active").and_then(|v| v.as_bool()) != Some(true) {
        return Err(unauthorized(
            "Datachain ID token is expired or invalid; sign in again",
        ));
    }
    let address = body
        .get("primary_address")
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with("0x") && s.len() == 42)
        .map(|s| s.to_lowercase());
    address.ok_or_else(|| {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "your Datawallet+ account has no on-chain wallet bound yet - bind a wallet in \
             Datawallet+, or connect an EVM wallet (e.g. MetaMask) to sign in"
                .to_string(),
        )
    })
}

/// `POST /api/v1/ecosystem/auth/session` - exchange either an EIP-191
/// console signature or a verified Datachain ID (Datawallet+) token for
/// a session token. The signature domain is distinct from the
/// stakeholder gateway, so a captured stakeholder signature can never
/// be replayed here (and vice versa).
async fn create_session(
    headers: HeaderMap,
    Json(body): Json<CreateSessionBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // The shared console token, when configured, gates sign-in too.
    if let Ok(expected) = std::env::var("EDC_CONSOLE_TOKEN") {
        if !expected.is_empty() {
            let presented = headers
                .get("x-edc-console-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let ok = presented.len() == expected.len()
                && presented
                    .bytes()
                    .zip(expected.bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0;
            if !ok {
                return Err(unauthorized("missing or invalid console token"));
            }
        }
    }

    let wallet = if let Some(token) = body.datachain_id_token.as_deref().filter(|t| !t.is_empty())
    {
        verify_datachain_id_token(token).await?
    } else {
        let (address, timestamp, signature) =
            match (&body.address, body.timestamp, &body.signature) {
                (Some(a), Some(t), Some(s)) => (a, t, s),
                _ => {
                    return Err(unauthorized(
                        "provide either address+timestamp+signature (EIP-191) or \
                         datachain_id_token (Datawallet+ sign-in)",
                    ))
                }
            };
        walletsig::verify_domain(walletsig::DOMAIN_CONSOLE, address, timestamp, signature, now_ts())
            .map_err(|e| unauthorized(format!("console signature rejected: {e}")))?
    };

    let (token, expires_at) = session::create(&wallet, now_ts());
    Ok(Json(json!({
        "session_token": token,
        "wallet": wallet,
        "expires_at": expires_at,
        "note": "send this value in the X-Edc-Session header on every console request",
    })))
}

/// `DELETE /api/v1/ecosystem/auth/session` - sign out (revokes the
/// presented session token).
async fn delete_session(headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let token = headers
        .get("x-edc-session")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| unauthorized("X-Edc-Session header required"))?;
    let revoked = session::revoke(token);
    Ok(Json(json!({ "revoked": revoked })))
}

/// Require the caller to hold a mutating role on the project.
fn require_mutator(project: &Project, wallet: &str) -> Result<Role, ApiError> {
    match project.role_of(wallet) {
        Some(r) if r.can_mutate() => Ok(r),
        Some(_) => Err(forbidden("role cannot modify this project")),
        None => Err(forbidden("wallet is not a member of this project")),
    }
}

/// Require any team membership (read access to the console).
fn require_member(project: &Project, wallet: &str) -> Result<Role, ApiError> {
    project
        .role_of(wallet)
        .ok_or_else(|| forbidden("wallet is not a member of this project"))
}

/// Stakeholder auth → (grant, project, sandbox).
///
/// Two authentication paths (spec v1.0 §6.3):
///
/// 1. **Bearer token** minted from a grant (`Authorization: Bearer edc_…`).
///    Sandbox keys (`edc_sbx_…`) resolve with `sandbox = true` and are
///    served from the deterministic synthetic stream, unmetered.
/// 2. **EIP-191 wallet signature** - `X-Edc-Address`, `X-Edc-Timestamp`
///    (unix seconds, ±300 s freshness), and `X-Edc-Signature` (65-byte
///    hex signature over `walletsig::auth_message`). Resolves to the
///    most recent usable grant naming that wallet.
fn stakeholder_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(AccessGrant, Project, bool), ApiError> {
    let (grant, sandbox) = if let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    {
        state
            .registry
            .authorize_token(token)
            .ok_or_else(|| unauthorized("token not valid for any active grant"))?
    } else if let (Some(address), Some(ts), Some(sig)) = (
        headers.get("x-edc-address").and_then(|v| v.to_str().ok()),
        headers.get("x-edc-timestamp").and_then(|v| v.to_str().ok()),
        headers.get("x-edc-signature").and_then(|v| v.to_str().ok()),
    ) {
        let ts: i64 = ts
            .parse()
            .map_err(|_| unauthorized("X-Edc-Timestamp must be unix seconds"))?;
        let verified = walletsig::verify(address, ts, sig, now_ts())
            .map_err(|e| unauthorized(format!("wallet signature rejected: {e}")))?;
        let grant = state
            .registry
            .authorize_wallet(&verified)
            .ok_or_else(|| unauthorized("no active grant names this wallet"))?;
        (grant, false)
    } else {
        return Err(unauthorized(
            "Bearer token or X-Edc-Address/X-Edc-Timestamp/X-Edc-Signature headers required",
        ));
    };
    let project = state
        .registry
        .get_project(&grant.project_id)
        .ok_or_else(not_found)?;
    Ok((grant, project, sandbox))
}

/// The reading source for a stakeholder session: the live journal for
/// production credentials, the deterministic synthetic stream for
/// sandbox keys (spec v1.0 §6.3 - sandbox never touches live data).
fn session_readings(
    state: &AppState,
    project: &Project,
    sandbox: bool,
) -> Vec<TelemetryReading> {
    if sandbox {
        simulation::synth_history(project, 120, now_ts())
    } else {
        let store = state.registry.live_store(&project.id);
        let s = store.read();
        s.readings.clone()
    }
}

fn asset_category<'a>(project: &'a Project, asset_id: &str) -> &'a str {
    project
        .inventory
        .assets
        .iter()
        .find(|a| a.id == asset_id)
        .map(|a| a.category.as_str())
        .unwrap_or("")
}

// ---------------------------------------------------------------------------
// Console: projects & wizard
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateProjectBody {
    name: String,
    /// Sandbox project (spec v1.0 §6.3): synthetic telemetry, no KYB
    /// gate at deploy, no cloud provisioning, excluded from the real
    /// public directory.
    #[serde(default)]
    simulation: bool,
    /// Optional scenario template: `den_haag_escalators` or `agri_estate`
    /// (pre-populates definition + inventory so the community can test
    /// immediately).
    #[serde(default)]
    template: String,
}

async fn create_project(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<CreateProjectBody>,
) -> Result<impl IntoResponse, ApiError> {
    let wallet = console_wallet(&headers)?;
    if body.name.trim().is_empty() {
        return Err(bad("project name required"));
    }
    let mut project = Project::new(body.name.trim(), &wallet);
    project.simulation = body.simulation;
    if !body.template.is_empty() {
        if !simulation::apply_template(&mut project, &body.template) {
            return Err(bad(format!(
                "unknown template '{}' - available: den_haag_escalators, agri_estate",
                body.template
            )));
        }
    }
    state.registry.insert_project(project.clone());
    Ok((StatusCode::CREATED, Json(project)))
}

async fn list_projects(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let mut mine: Vec<Project> = state
        .registry
        .list_projects()
        .into_iter()
        .filter(|p| p.role_of(&wallet).is_some())
        .collect();
    // Newest first - the console always surfaces the latest added project
    // at the top of the sidebar list.
    mine.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    Ok(Json(json!({ "count": mine.len(), "projects": mine })))
}

async fn get_project(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Project>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    Ok(Json(project))
}

macro_rules! wizard_step {
    ($fn_name:ident, $ty:ty, $apply:expr) => {
        async fn $fn_name(
            State(state): State<SharedState>,
            headers: HeaderMap,
            Path(id): Path<String>,
            Json(body): Json<$ty>,
        ) -> Result<Json<Project>, ApiError> {
            let wallet = console_wallet(&headers)?;
            let project = state.registry.get_project(&id).ok_or_else(not_found)?;
            require_mutator(&project, &wallet)?;
            let apply: fn(&mut Project, $ty) = $apply;
            let updated = state
                .registry
                .update_project(&id, move |p| apply(p, body))
                .ok_or_else(not_found)?;
            Ok(Json(updated))
        }
    };
}

wizard_step!(put_identity, IdentityInfo, |p, b| p.identity = Some(b));
wizard_step!(put_definition, ProjectDefinition, |p, b| p.definition = Some(b));
wizard_step!(put_crypto, CryptoAssetConfig, |p, b| p.crypto = b);
wizard_step!(put_policy, MutabilityPolicy, |p, b| p.mutability_policy = b);

async fn put_team(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(team): Json<Vec<TeamMember>>,
) -> Result<Json<Project>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    if !team.iter().any(|m| matches!(m.role, Role::Owner)) {
        return Err(bad("team must retain at least one Owner"));
    }
    let updated = state
        .registry
        .update_project(&id, |p| p.team = team)
        .ok_or_else(not_found)?;
    Ok(Json(updated))
}

// -- inventory ---------------------------------------------------------------

macro_rules! inventory_append {
    ($fn_name:ident, $ty:ty, $field:ident) => {
        async fn $fn_name(
            State(state): State<SharedState>,
            headers: HeaderMap,
            Path(id): Path<String>,
            Json(items): Json<Vec<$ty>>,
        ) -> Result<Json<serde_json::Value>, ApiError> {
            let wallet = console_wallet(&headers)?;
            let project = state.registry.get_project(&id).ok_or_else(not_found)?;
            require_mutator(&project, &wallet)?;
            let added = items.len();
            state
                .registry
                .update_project(&id, |p| {
                    for item in items {
                        // Upsert by id.
                        if let Some(existing) =
                            p.inventory.$field.iter_mut().find(|x| x.id == item.id)
                        {
                            *existing = item;
                        } else {
                            p.inventory.$field.push(item);
                        }
                    }
                })
                .ok_or_else(not_found)?;
            Ok(Json(json!({ "added_or_updated": added })))
        }
    };
}

inventory_append!(post_assets, AssetRecord, assets);
inventory_append!(post_sensors, SensorRecord, sensors);
inventory_append!(post_mesh, MeshNode, mesh_nodes);
inventory_append!(post_external, ExternalSource, external_sources);
inventory_append!(post_agents, AiAgentConfig, ai_agents);

async fn import_assets_csv(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: String,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    let (assets, report) = csv_import::import_assets(&body);
    state
        .registry
        .update_project(&id, |p| {
            for mut a in assets {
                // Derive the asset wallet from project + asset id.
                a.wallet = crate::types::project_wallet(&format!("{id}:{}", a.id));
                if let Some(existing) =
                    p.inventory.assets.iter_mut().find(|x| x.id == a.id)
                {
                    *existing = a;
                } else {
                    p.inventory.assets.push(a);
                }
            }
        })
        .ok_or_else(not_found)?;
    Ok(Json(serde_json::to_value(&report).unwrap_or_default()))
}

async fn import_sensors_csv(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: String,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    let (sensors, report) = csv_import::import_sensors(&body);
    state
        .registry
        .update_project(&id, |p| {
            for s in sensors {
                if let Some(existing) =
                    p.inventory.sensors.iter_mut().find(|x| x.id == s.id)
                {
                    *existing = s;
                } else {
                    p.inventory.sensors.push(s);
                }
            }
        })
        .ok_or_else(not_found)?;
    Ok(Json(serde_json::to_value(&report).unwrap_or_default()))
}

// -- node plan & deploy -------------------------------------------------------

#[derive(Deserialize)]
struct NodePlanQuery {
    #[serde(default)]
    jurisdictions: Option<usize>,
    #[serde(default)]
    validator: Option<bool>,
}

async fn get_node_plan(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<NodePlanQuery>,
) -> Result<Json<NodePlan>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let plan = NodePlan::recommend(
        project.inventory.assets.len(),
        project.inventory.events_per_hour(),
        q.jurisdictions.unwrap_or(1),
        q.validator.unwrap_or(false),
    );
    Ok(Json(plan))
}

#[derive(Deserialize, Default)]
struct DeployBody {
    #[serde(default)]
    jurisdictions: Option<usize>,
    #[serde(default)]
    validator: Option<bool>,
    /// Public base URL of the stakeholder dashboard (defaults to this node).
    #[serde(default)]
    stakeholder_url: Option<String>,
}

/// Step 8/9 - confirm & deploy: freeze the node plan, provision the
/// nodes via rope-deployer, open the project's on-chain string, anchor
/// the genesis + public card, flip status to Live.
///
/// Simulation projects skip the KYB gate and cloud provisioning
/// (spec v1.0 §6.3) - they go Live on synthetic streams immediately.
async fn deploy_project(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<DeployBody>,
) -> Result<Json<Project>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;

    if project.identity.is_none() && !project.simulation {
        return Err(bad("step 1 (identity) incomplete"));
    }
    if project.definition.as_ref().map(|d| d.name.is_empty()).unwrap_or(true) {
        return Err(bad("step 2 (definition) incomplete"));
    }
    if project.inventory.sensors.is_empty() {
        return Err(bad("step 5 (inventory) has no sensors - nothing to monitor"));
    }

    let plan = NodePlan::recommend(
        project.inventory.assets.len(),
        project.inventory.events_per_hour(),
        body.jurisdictions.unwrap_or(1),
        body.validator.unwrap_or(false),
    );

    let stakeholder_url = body.stakeholder_url.unwrap_or_else(|| {
        std::env::var("EDC_PUBLIC_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:9095".to_string())
    });

    // Provision the sovereign nodes (spec v1.0 §5). Mark the project
    // Deploying while the cloud calls are in flight so a concurrent
    // console reader sees the true state.
    state
        .registry
        .update_project(&id, |p| p.status = ProjectStatus::Deploying)
        .ok_or_else(not_found)?;
    let nodes = provision::provision_nodes(&state.deployer, &project, &plan).await;

    // Anchor genesis on the project's own string (best-effort: local
    // persistence is the primary record; chain anchoring is replayable).
    let genesis = state
        .registry
        .anchor(
            &project.wallet,
            "EcosystemProjectGenesis",
            format!(
                "Ecosystem project '{}' deployed via EDC - archetype {:?}, {} assets, {} sensors, tier {:?}",
                project.name(),
                project.definition.as_ref().map(|d| d.archetype).unwrap(),
                project.inventory.assets.len(),
                project.inventory.sensors.len(),
                plan.tier,
            ),
            json!({
                "project_id": project.id,
                "owner": wallet,
                "node_plan": serde_json::to_value(&plan).unwrap_or_default(),
            }),
        )
        .await;

    let updated = state
        .registry
        .update_project(&id, |p| {
            p.node_plan = Some(plan);
            p.provisioned_nodes = nodes;
            p.status = ProjectStatus::Live;
            p.stakeholder_url = stakeholder_url;
            if let Some(g) = &genesis {
                p.genesis_anchor = g.clone();
            }
        })
        .ok_or_else(not_found)?;

    // Anchor the public card on the registry wallet for dcscan
    // auto-listing. Simulation projects stay out of the real directory
    // (spec v1.0 §6.3).
    let updated = if updated.simulation {
        updated
    } else {
        let card_anchor = state.registry.anchor_public_card(&updated).await;
        state
            .registry
            .update_project(&id, |p| {
                if let Some(a) = &card_anchor {
                    p.registry_anchor = a.clone();
                }
            })
            .ok_or_else(not_found)?
    };

    Ok(Json(updated))
}

#[derive(Deserialize)]
struct StatusBody {
    status: ProjectStatus,
}

async fn put_status(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<StatusBody>,
) -> Result<Json<Project>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    let role = require_mutator(&project, &wallet)?;
    if matches!(body.status, ProjectStatus::Decommissioned) && !role.is_owner() {
        return Err(forbidden("only the Owner may decommission a project"));
    }
    let updated = state
        .registry
        .update_project(&id, |p| p.status = body.status)
        .ok_or_else(not_found)?;
    // Lifecycle anchor + refresh the public card.
    state
        .registry
        .anchor(
            &updated.wallet,
            "EcosystemProjectStatusChanged",
            format!("Project '{}' status → {:?}", updated.name(), updated.status),
            json!({ "project_id": updated.id, "status": updated.status, "by": wallet }),
        )
        .await;
    state.registry.anchor_public_card(&updated).await;
    Ok(Json(updated))
}

// ---------------------------------------------------------------------------
// Console: live data ingestion & queries
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TelemetryBody {
    sensor_id: String,
    value: f64,
    #[serde(default)]
    ts: Option<i64>,
}

async fn post_telemetry(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(items): Json<Vec<TelemetryBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    // Operators and AI agents may ingest; Auditors may not.
    let role = require_member(&project, &wallet)?;
    if matches!(role, Role::Auditor) {
        return Err(forbidden("auditors have read-only access"));
    }
    let mut accepted = 0usize;
    let mut rejected = Vec::new();
    for item in items {
        let Some(sensor) = project
            .inventory
            .sensors
            .iter()
            .find(|s| s.id == item.sensor_id)
        else {
            rejected.push(json!({ "sensor_id": item.sensor_id, "reason": "unknown sensor" }));
            continue;
        };
        let band = classify_band(sensor, item.value);
        state.registry.push_reading(TelemetryReading {
            project_id: id.clone(),
            asset_id: sensor.parent_asset_id.clone(),
            sensor_id: sensor.id.clone(),
            parameter: sensor.parameter.clone(),
            value: item.value,
            unit: sensor.unit.clone(),
            ts: item.ts.unwrap_or_else(now_ts),
            band: band.to_string(),
            anchor: String::new(),
        });
        accepted += 1;
    }
    Ok(Json(json!({ "accepted": accepted, "rejected": rejected })))
}

#[derive(Deserialize)]
struct DiagnosisBody {
    asset_id: String,
    agent_id: String,
    diagnosis: String,
    recommendation: String,
    confidence: f64,
}

async fn post_diagnosis(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<DiagnosisBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    let role = require_member(&project, &wallet)?;
    if matches!(role, Role::Auditor) {
        return Err(forbidden("auditors have read-only access"));
    }
    let ev = DiagnosisEvent {
        project_id: id.clone(),
        asset_id: body.asset_id,
        agent_id: body.agent_id,
        diagnosis: body.diagnosis,
        recommendation: body.recommendation,
        confidence: body.confidence.clamp(0.0, 1.0),
        ts: now_ts(),
        anchor: String::new(),
    };
    state.registry.push_diagnosis(ev.clone());
    // Diagnoses are anchored on the project string (audit trail).
    let anchor = state
        .registry
        .anchor(
            &project.wallet,
            "MaintenanceDiagnosis",
            format!(
                "Agent {} on asset {}: {} (confidence {:.2})",
                ev.agent_id, ev.asset_id, ev.diagnosis, ev.confidence
            ),
            serde_json::to_value(&ev).unwrap_or_default(),
        )
        .await;
    Ok(Json(json!({ "recorded": true, "anchor": anchor })))
}

#[derive(Deserialize)]
struct ApprovalBody {
    subject: String,
    note: String,
}

async fn post_approval(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ApprovalBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    let role = require_mutator(&project, &wallet)?;
    let ev = ApprovalEvent {
        project_id: id.clone(),
        subject: body.subject,
        approved_by: wallet,
        role,
        note: body.note,
        ts: now_ts(),
        anchor: String::new(),
    };
    state.registry.push_approval(ev.clone());
    let anchor = state
        .registry
        .anchor(
            &project.wallet,
            "GovernanceApproval",
            format!("Approval of '{}' by {:?}", ev.subject, ev.role),
            serde_json::to_value(&ev).unwrap_or_default(),
        )
        .await;
    Ok(Json(json!({ "recorded": true, "anchor": anchor })))
}

#[derive(Deserialize, Default)]
struct ReadingsQuery {
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    parameter: Option<String>,
    #[serde(default)]
    asset_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

fn filter_readings(
    readings: &[TelemetryReading],
    q: &ReadingsQuery,
) -> Vec<TelemetryReading> {
    let limit = q.limit.unwrap_or(1_000).min(5_000);
    readings
        .iter()
        .filter(|r| q.since.map(|s| r.ts >= s).unwrap_or(true))
        .filter(|r| {
            q.parameter
                .as_ref()
                .map(|p| &r.parameter == p)
                .unwrap_or(true)
        })
        .filter(|r| {
            q.asset_id
                .as_ref()
                .map(|a| &r.asset_id == a)
                .unwrap_or(true)
        })
        .rev()
        .take(limit)
        .cloned()
        .collect()
}

async fn get_readings(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ReadingsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let store = state.registry.live_store(&id);
    let readings = filter_readings(&store.read().readings, &q);
    Ok(Json(json!({ "count": readings.len(), "readings": readings })))
}

async fn get_diagnoses(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let store = state.registry.live_store(&id);
    let items: Vec<DiagnosisEvent> =
        store.read().diagnoses.iter().rev().take(200).cloned().collect();
    Ok(Json(json!({ "count": items.len(), "diagnoses": items })))
}

async fn get_approvals(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let store = state.registry.live_store(&id);
    let items: Vec<ApprovalEvent> =
        store.read().approvals.iter().rev().take(200).cloned().collect();
    Ok(Json(json!({ "count": items.len(), "approvals": items })))
}

// ---------------------------------------------------------------------------
// Console: AI analytics
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AskBody {
    question: String,
    #[serde(default)]
    parameter: Option<String>,
    #[serde(default)]
    asset_id: Option<String>,
    #[serde(default)]
    since: Option<i64>,
}

async fn ask_project(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AskBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let store = state.registry.live_store(&id);
    let readings = filter_readings(
        &store.read().readings,
        &ReadingsQuery {
            since: body.since,
            parameter: body.parameter.clone(),
            asset_id: body.asset_id.clone(),
            limit: Some(5_000),
        },
    );
    let answer = state
        .ai
        .ask(&body.question, &readings, &project.inventory.sensors, now_ts())
        .await;
    Ok(Json(serde_json::to_value(&answer).unwrap_or_default()))
}

async fn get_dossier(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ReadingsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let store = state.registry.live_store(&id);
    let readings = filter_readings(&store.read().readings, &q);
    let dossier = crate::ai::build_dossier(&readings, &project.inventory.sensors, now_ts());
    let charts = crate::ai::deterministic_charts(&readings, &dossier);
    Ok(Json(json!({
        "readings_in_scope": readings.len(),
        "grounding": dossier,
        "charts": charts,
    })))
}

// ---------------------------------------------------------------------------
// Console: grants
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateGrantBody {
    grantee: Grantee,
    stakeholder_class: StakeholderClass,
    #[serde(default)]
    scope: GrantScope,
    #[serde(default)]
    starts_at: i64,
    #[serde(default)]
    expires_at: i64,
    #[serde(default)]
    price: GrantPrice,
    #[serde(default)]
    delivery: Vec<String>,
}

async fn create_grant(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CreateGrantBody>,
) -> Result<impl IntoResponse, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    let mut grant = AccessGrant::new(
        &id,
        body.grantee,
        body.stakeholder_class,
        body.scope,
        body.starts_at,
        body.expires_at,
        body.price,
        if body.delivery.is_empty() {
            vec!["rest".to_string()]
        } else {
            body.delivery
        },
        &wallet,
        timelock_delay_secs(),
    );
    // Anchor the grant issuance on the project string.
    let anchor = state
        .registry
        .anchor(
            &project.wallet,
            "AccessGrantIssued",
            format!(
                "Grant {} to {:?} '{}' ({:?}) on project '{}'",
                grant.id, grant.grantee.kind, grant.grantee.value,
                grant.stakeholder_class, project.name()
            ),
            serde_json::to_value(&grant).unwrap_or_default(),
        )
        .await;
    if let Some(a) = anchor {
        grant.anchor_knot = a;
    }
    state.registry.insert_grant(grant.clone());
    Ok((StatusCode::CREATED, Json(grant)))
}

async fn list_grants(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let grants = state.registry.grants_for_project(&id);
    Ok(Json(json!({ "count": grants.len(), "grants": grants })))
}

#[derive(Deserialize)]
struct MintKeyBody {
    #[serde(default)]
    label: String,
    /// Sandbox key (spec v1.0 §6.3): served from the deterministic
    /// synthetic stream, never metered, `edc_sbx_` prefix.
    #[serde(default)]
    sandbox: bool,
}

async fn mint_grant_key(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(gid): Path<String>,
    Json(body): Json<MintKeyBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let grant = state.registry.get_grant(&gid).ok_or_else(not_found)?;
    let project = state
        .registry
        .get_project(&grant.project_id)
        .ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    let (record, token) = mint_key(&gid, &body.label, body.sandbox);
    state.registry.insert_key(record.clone());
    // The plaintext token is returned exactly once and never persisted.
    Ok(Json(json!({
        "key_id": record.id,
        "grant_id": gid,
        "token": token,
        "sandbox": record.sandbox,
        "note": "store this token now - it cannot be retrieved again",
    })))
}

async fn revoke_grant_route(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(gid): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let grant = state.registry.get_grant(&gid).ok_or_else(not_found)?;
    let project = state
        .registry
        .get_project(&grant.project_id)
        .ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    let revoked = state.registry.revoke_grant(&gid).ok_or_else(not_found)?;
    state
        .registry
        .anchor(
            &project.wallet,
            "AccessGrantRevoked",
            format!("Grant {} revoked", revoked.id),
            json!({ "grant_id": revoked.id, "by": wallet }),
        )
        .await;
    Ok(Json(json!({ "revoked": true, "grant_id": gid })))
}

// ---------------------------------------------------------------------------
// Stakeholder gateway
// ---------------------------------------------------------------------------

fn scope_readings(
    project: &Project,
    grant: &AccessGrant,
    readings: &[TelemetryReading],
) -> Vec<TelemetryReading> {
    readings
        .iter()
        .filter(|r| {
            grant
                .scope
                .allows_asset(&r.asset_id, asset_category(project, &r.asset_id))
        })
        .cloned()
        .collect()
}

async fn sh_overview(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (grant, project, sandbox) = stakeholder_auth(&state, &headers)?;
    let readings = session_readings(&state, &project, sandbox);
    let scoped = scope_readings(&project, &grant, &readings);
    let ok = scoped.iter().filter(|r| r.band == "ok").count();
    let warn = scoped.iter().filter(|r| r.band == "warning").count();
    let crit = scoped.iter().filter(|r| r.band == "critical").count();
    Ok(Json(json!({
        "project": project.public_card(),
        "grant": {
            "id": grant.id,
            "stakeholder_class": grant.stakeholder_class,
            "scope": grant.scope,
            "expires_at": grant.expires_at,
        },
        "sandbox": sandbox,
        "readings_in_scope": scoped.len(),
        "bands": { "ok": ok, "warning": warn, "critical": crit },
        "latest_ts": scoped.iter().map(|r| r.ts).max(),
    })))
}

async fn sh_readings(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(q): Query<ReadingsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (grant, project, sandbox) = stakeholder_auth(&state, &headers)?;
    if !grant.scope.allows_facet("readings") {
        return Err(forbidden("grant scope does not include readings"));
    }
    if !grant.allows_delivery("rest") {
        return Err(forbidden("grant does not allow REST delivery"));
    }
    let readings = session_readings(&state, &project, sandbox);
    let scoped = scope_readings(&project, &grant, &readings);
    let out = filter_readings(&scoped, &q);
    Ok(Json(json!({ "count": out.len(), "sandbox": sandbox, "readings": out })))
}

async fn sh_diagnoses(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (grant, project, sandbox) = stakeholder_auth(&state, &headers)?;
    if !grant.scope.allows_facet("diagnoses") {
        return Err(forbidden("grant scope does not include diagnoses"));
    }
    if sandbox {
        // Diagnoses are produced by real agents against real telemetry;
        // the sandbox has none by construction.
        return Ok(Json(json!({ "count": 0, "sandbox": true, "diagnoses": [] })));
    }
    let store = state.registry.live_store(&project.id);
    let items: Vec<DiagnosisEvent> = store
        .read()
        .diagnoses
        .iter()
        .filter(|d| {
            grant
                .scope
                .allows_asset(&d.asset_id, asset_category(&project, &d.asset_id))
        })
        .rev()
        .take(200)
        .cloned()
        .collect();
    Ok(Json(json!({ "count": items.len(), "diagnoses": items })))
}

async fn sh_approvals(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (grant, project, sandbox) = stakeholder_auth(&state, &headers)?;
    if !grant.scope.allows_facet("approvals") {
        return Err(forbidden("grant scope does not include approvals"));
    }
    if sandbox {
        return Ok(Json(json!({ "count": 0, "sandbox": true, "approvals": [] })));
    }
    let store = state.registry.live_store(&project.id);
    let items: Vec<ApprovalEvent> =
        store.read().approvals.iter().rev().take(200).cloned().collect();
    Ok(Json(json!({ "count": items.len(), "approvals": items })))
}

/// SSE live stream: pushes new in-scope readings every 2 s.
/// Requires `stream` delivery on the grant.
async fn sh_stream(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let (grant, project, sandbox) = stakeholder_auth(&state, &headers)?;
    if !grant.scope.allows_facet("readings") {
        return Err(forbidden("grant scope does not include readings"));
    }
    if !grant.allows_delivery("stream") {
        return Err(forbidden("grant does not allow stream delivery"));
    }
    let registry = state.registry.clone();
    let expires_at = grant.expires_at;

    // Start from the current tail so the consumer only sees new data.
    let initial_cursor = if sandbox {
        now_ts()
    } else {
        let store = registry.live_store(&project.id);
        let ts = store.read().readings.last().map(|r| r.ts).unwrap_or(0);
        ts
    };

    // The cursor (max ts already delivered) is the unfold state, so each
    // poll turn resumes exactly where the previous event left off.
    let stream = futures::stream::unfold(initial_cursor, move |cursor| {
        let registry = registry.clone();
        let project = project.clone();
        let grant = grant.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                // Grant expiry ends the stream mid-flight.
                if expires_at > 0 && now_ts() >= expires_at {
                    return None;
                }
                let fresh: Vec<TelemetryReading> = if sandbox {
                    // Sandbox: the deterministic synthetic tick IS the
                    // live stream - same generator as REST/GraphQL.
                    let now = now_ts();
                    if now <= cursor {
                        continue;
                    }
                    simulation::synth_tick(&project, now)
                        .into_iter()
                        .filter(|r| {
                            grant.scope.allows_asset(
                                &r.asset_id,
                                asset_category(&project, &r.asset_id),
                            )
                        })
                        .collect()
                } else {
                    let store = registry.live_store(&project.id);
                    let s = store.read();
                    s.readings
                        .iter()
                        .filter(|r| r.ts > cursor)
                        .filter(|r| {
                            grant.scope.allows_asset(
                                &r.asset_id,
                                asset_category(&project, &r.asset_id),
                            )
                        })
                        .cloned()
                        .collect()
                };
                if !fresh.is_empty() {
                    let next_cursor =
                        fresh.iter().map(|r| r.ts).max().unwrap_or(cursor);
                    let payload =
                        serde_json::to_string(&fresh).unwrap_or_else(|_| "[]".into());
                    let ev: Result<Event, Infallible> =
                        Ok(Event::default().event("readings").data(payload));
                    return Some((ev, next_cursor));
                }
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn sh_ask(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<AskBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (grant, project, sandbox) = stakeholder_auth(&state, &headers)?;
    if !grant.scope.allows_facet("readings") {
        return Err(forbidden("grant scope does not include readings"));
    }
    let readings = session_readings(&state, &project, sandbox);
    let scoped = scope_readings(&project, &grant, &readings);
    let filtered = filter_readings(
        &scoped,
        &ReadingsQuery {
            since: body.since,
            parameter: body.parameter.clone(),
            asset_id: body.asset_id.clone(),
            limit: Some(5_000),
        },
    );
    let answer = state
        .ai
        .ask(&body.question, &filtered, &project.inventory.sensors, now_ts())
        .await;
    Ok(Json(serde_json::to_value(&answer).unwrap_or_default()))
}

// ---------------------------------------------------------------------------
// Console: sandbox / simulation (spec v1.0 §6.3)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct BackfillBody {
    /// Synthetic points per declared sensor (default 96 ≈ one day at
    /// 15-minute cadence). Capped at 500.
    #[serde(default)]
    points_per_sensor: Option<usize>,
}

/// Push deterministic synthetic history into a simulation project's live
/// store so the console dashboard, dossier, and `/ask` all have data to
/// work on. Only valid for `simulation = true` projects - live projects
/// must never receive synthetic readings.
async fn simulate_backfill(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<BackfillBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    if !project.simulation {
        return Err(forbidden(
            "synthetic backfill is only allowed on simulation projects",
        ));
    }
    if project.inventory.sensors.is_empty() {
        return Err(bad("project has no sensors to simulate"));
    }
    let points = body.points_per_sensor.unwrap_or(96).clamp(1, 500);
    let readings = simulation::synth_history(&project, points, now_ts());
    let count = readings.len();
    for r in readings {
        state.registry.push_reading(r);
    }
    Ok(Json(json!({ "generated": count, "points_per_sensor": points })))
}

// ---------------------------------------------------------------------------
// Console: scheduled reports (spec v1.0 §6.4)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ReportScheduleBody {
    /// `hourly` | `daily` | `weekly` | `monthly` | `""` (off).
    schedule: String,
}

async fn put_report_schedule(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ReportScheduleBody>,
) -> Result<Json<Project>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    if !body.schedule.is_empty() && reports::cadence_secs(&body.schedule).is_none() {
        return Err(bad(
            "schedule must be hourly, daily, weekly, monthly, or empty to disable",
        ));
    }
    let updated = state
        .registry
        .update_project(&id, |p| {
            p.report_schedule = body.schedule;
            // Restart the cadence clock so the first period begins now.
            p.last_report_at = 0;
        })
        .ok_or_else(not_found)?;
    Ok(Json(updated))
}

#[derive(Deserialize, Default)]
struct GenerateReportBody {
    /// Trailing window covered by the on-demand report (default 24h).
    #[serde(default)]
    period_secs: Option<i64>,
}

async fn generate_report_now(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<GenerateReportBody>,
) -> Result<Json<ReportRecord>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let now = now_ts();
    let period = body.period_secs.unwrap_or(86_400).clamp(60, 365 * 86_400);
    let report =
        reports::generate(&state.registry, &project, "on_demand", now - period, now);
    let report = reports::persist_and_anchor(&state.registry, &project, report).await;
    Ok(Json(report))
}

async fn list_reports(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    let store = state.registry.live_store(&id);
    let items: Vec<ReportRecord> =
        store.read().reports.iter().rev().take(100).cloned().collect();
    Ok(Json(json!({ "count": items.len(), "reports": items })))
}

// ---------------------------------------------------------------------------
// Console: export scheduling + billing (spec v1.0 §6.3)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ExportScheduleBody {
    /// `hourly` | `daily` | `weekly` | `""` (off).
    schedule: String,
}

async fn put_export_schedule(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(gid): Path<String>,
    Json(body): Json<ExportScheduleBody>,
) -> Result<Json<AccessGrant>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let grant = state.registry.get_grant(&gid).ok_or_else(not_found)?;
    let project = state
        .registry
        .get_project(&grant.project_id)
        .ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;
    if !body.schedule.is_empty() && export::schedule_secs(&body.schedule).is_none() {
        return Err(bad("schedule must be hourly, daily, weekly, or empty to disable"));
    }
    if !body.schedule.is_empty() && !grant.allows_delivery("export") {
        return Err(bad("grant delivery methods do not include 'export'"));
    }
    let updated = state
        .registry
        .update_grant(&gid, |g| {
            g.export_schedule = body.schedule;
            g.last_export_at = 0;
        })
        .ok_or_else(not_found)?;
    Ok(Json(updated))
}

/// The open (not yet invoiced) billing statement for a grant.
async fn get_billing_statement(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(gid): Path<String>,
) -> Result<Json<billing::BillingStatement>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let grant = state.registry.get_grant(&gid).ok_or_else(not_found)?;
    let project = state
        .registry
        .get_project(&grant.project_id)
        .ok_or_else(not_found)?;
    require_member(&project, &wallet)?;
    Ok(Json(billing::statement_for(&grant, now_ts())))
}

/// Close (invoice) the open statement: anchor it on the project string
/// and advance the grant's invoiced watermark so the next window starts
/// clean. The anchored knot hash is the invoice reference for
/// settlement (FAT / project-token / fiat).
async fn close_billing_statement(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(gid): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let grant = state.registry.get_grant(&gid).ok_or_else(not_found)?;
    let project = state
        .registry
        .get_project(&grant.project_id)
        .ok_or_else(not_found)?;
    require_mutator(&project, &wallet)?;

    let statement = billing::statement_for(&grant, now_ts());
    let anchor = state
        .registry
        .anchor(
            &project.wallet,
            "BillingStatementClosed",
            format!(
                "Billing statement for grant {} ({}): {} {} due for {}–{}",
                grant.id,
                statement.price_model,
                statement.amount_due,
                statement.currency,
                statement.period_start,
                statement.period_end
            ),
            serde_json::to_value(&statement).unwrap_or_default(),
        )
        .await;

    let (billed_calls, last_billed_at) = billing::closed_watermark(&grant, &statement);
    state
        .registry
        .update_grant(&gid, |g| {
            g.billed_calls = billed_calls;
            g.last_billed_at = last_billed_at;
        })
        .ok_or_else(not_found)?;

    Ok(Json(json!({
        "statement": statement,
        "anchor": anchor,
        "note": "settle against the anchored statement; the knot hash is the invoice reference",
    })))
}

// ---------------------------------------------------------------------------
// Stakeholder: GraphQL + bulk exports (spec v1.0 §6.3)
// ---------------------------------------------------------------------------

async fn sh_graphql(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(request): Json<async_graphql::Request>,
) -> Result<Json<async_graphql::Response>, ApiError> {
    let (grant, project, sandbox) = stakeholder_auth(&state, &headers)?;
    let session = graphql::GqlSession {
        registry: state.registry.clone(),
        grant,
        project,
        sandbox,
    };
    Ok(Json(graphql::execute(session, request).await))
}

async fn sh_list_exports(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (grant, _project, sandbox) = stakeholder_auth(&state, &headers)?;
    if sandbox {
        return Err(forbidden("bulk exports are a production-credential feature"));
    }
    if !grant.allows_delivery("export") {
        return Err(forbidden("grant does not allow export delivery"));
    }
    let files = export::list_exports(&state.registry, &grant.id);
    Ok(Json(json!({
        "count": files.len(),
        "schedule": grant.export_schedule,
        "exports": files,
    })))
}

async fn sh_download_export(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let (grant, _project, sandbox) = stakeholder_auth(&state, &headers)?;
    if sandbox {
        return Err(forbidden("bulk exports are a production-credential feature"));
    }
    if !grant.allows_delivery("export") {
        return Err(forbidden("grant does not allow export delivery"));
    }
    // Path-traversal guard: exports are flat files named by the scheduler.
    if filename.contains('/') || filename.contains("..") {
        return Err(bad("invalid export filename"));
    }
    let bytes = export::read_export(&state.registry, &grant.id, &filename)
        .ok_or_else(not_found)?;
    Ok((
        [
            ("content-type", "text/csv; charset=utf-8".to_string()),
            (
                "content-disposition",
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    ))
}

// ---------------------------------------------------------------------------
// Public: QR/NFC tag resolution (spec v1.0 §4.5.1)
// ---------------------------------------------------------------------------

/// Resolve a physical QR/NFC tag to its asset + project. Tags are
/// registered by setting `AssetRecord.tag_id` in the inventory step;
/// scanning the tag in the field lands here and returns the public
/// subset of the asset record (no serials, no endpoints).
async fn resolve_tag(
    State(state): State<SharedState>,
    Path(tag_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tag = tag_id.trim();
    if tag.is_empty() {
        return Err(bad("tag id required"));
    }
    for project in state.registry.list_projects() {
        if matches!(project.status, ProjectStatus::Draft) {
            continue;
        }
        if let Some(asset) = project.inventory.assets.iter().find(|a| a.tag_id == tag) {
            let sensors: Vec<serde_json::Value> = project
                .inventory
                .sensors
                .iter()
                .filter(|s| s.parent_asset_id == asset.id)
                .map(|s| {
                    json!({
                        "id": s.id,
                        "parameter": s.parameter,
                        "unit": s.unit,
                        "cadence": s.cadence,
                    })
                })
                .collect();
            return Ok(Json(json!({
                "tag_id": tag,
                "project": project.public_card(),
                "asset": {
                    "id": asset.id,
                    "name": asset.name,
                    "category": asset.category,
                    "sub_type": asset.sub_type,
                    "gps": asset.gps,
                    "manufacturer": asset.manufacturer,
                    "model": asset.model,
                    "commissioning_date": asset.commissioning_date,
                    "health_score": asset.health_score,
                    "last_seen_at": asset.last_seen_at,
                    "wallet": asset.wallet,
                },
                "sensors": sensors,
                "stakeholder_api": format!(
                    "{}/api/v1/ecosystem/stakeholder",
                    project.stakeholder_url
                ),
            })));
        }
    }
    Err(not_found())
}

// ---------------------------------------------------------------------------
// Public directory (dcscan.io)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct PublicProjectsQuery {
    /// `1` includes sandbox projects (community test directory); the
    /// default directory lists real deployments only (spec v1.0 §6.3).
    #[serde(default)]
    include_simulation: Option<u8>,
}

async fn public_projects(
    State(state): State<SharedState>,
    Query(q): Query<PublicProjectsQuery>,
) -> Json<serde_json::Value> {
    let include_sim = q.include_simulation.unwrap_or(0) == 1;
    let cards: Vec<serde_json::Value> = state
        .registry
        .list_projects()
        .into_iter()
        .filter(|p| {
            matches!(p.status, ProjectStatus::Live | ProjectStatus::Suspended)
        })
        .filter(|p| include_sim || !p.simulation)
        .map(|p| p.public_card())
        .collect();
    Json(json!({ "count": cards.len(), "projects": cards }))
}

async fn public_project(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let project = state.registry.get_project(&id).ok_or_else(not_found)?;
    if matches!(project.status, ProjectStatus::Draft) {
        return Err(not_found());
    }
    // The public detail card also lists the *public-facet* grants so a
    // regulator/investor landing from dcscan knows how to request access.
    let public_grants: Vec<serde_json::Value> = state
        .registry
        .grants_for_project(&id)
        .into_iter()
        .filter(|g| g.grantee.kind == "public" && g.is_usable(now_ts()))
        .map(|g| {
            json!({
                "id": g.id,
                "stakeholder_class": g.stakeholder_class,
                "scope": g.scope,
                "price": g.price,
                "delivery": g.delivery,
            })
        })
        .collect();
    Ok(Json(json!({
        "project": project.public_card(),
        "public_grants": public_grants,
        "stakeholder_api": format!("{}/api/v1/ecosystem/stakeholder", project.stakeholder_url),
    })))
}
