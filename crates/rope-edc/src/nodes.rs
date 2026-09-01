//! Bare-node deployment API.
//!
//! Exposes `POST /api/v1/ecosystem/nodes` (and friends) so a console
//! caller can deploy a single sovereign Rope node without creating a
//! full nine-step Project first. This closes the gap flagged by the
//! operator: the console `+ New Project` button provisions nodes *as
//! part of* a project, but engineers coming from the CLI/console
//! expected a simple "deploy a node" surface too.
//!
//! Every route:
//!
//!  * Authenticates the caller via the same `console_wallet` helper the
//!    project routes use (session token, EIP-191 signature, or bare
//!    `X-Edc-Wallet` when `EDC_CONSOLE_REQUIRE_SIGNATURE` is off).
//!  * Scopes state to a tenant DID derived from the caller wallet
//!    (`did:dwp:<lowercase-address>`). A wallet only ever sees droplets
//!    it owns, even when the DO API returns a superset via the shared
//!    account token.
//!  * Delegates to the shared `rope_deployer::ProviderRegistry` built
//!    once at startup in `main.rs`, so a live DigitalOcean API call
//!    path and the local dry-run path share the same on-disk cache
//!    (`$DEPLOYER_STATE_DIR/<provider>/instances.json`).
//!
//! No stubs: when live credentials are configured for a provider, the
//! adapter really provisions a droplet and returns its id + IPv4 + the
//! Ed25519 node-identity public key. When credentials are not
//! configured, the response carries `dry_run: true` and a human-readable
//! `note` explaining why, but the request is still persisted so it can
//! be re-provisioned later against real credentials.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use rope_deployer::{NodeKind, Provider, ProvisionRequest};

use crate::api::{console_wallet, ApiError, SharedState};

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

/// Body accepted by `POST /api/v1/ecosystem/nodes`.
///
/// Every field except `provider` is optional; the server fills in
/// production defaults (Ubuntu 24.04 image, 2 vCPU / 4 GB droplet in
/// `fra1` for DigitalOcean, or `standard.medium` in `ch-gva-2` for
/// Exoscale) so a caller can start with a one-line curl and refine
/// later.
#[derive(Debug, Deserialize)]
pub struct DeployNodeRequest {
    /// `local`, `exoscale`, or `digitalocean`. Case-insensitive.
    pub provider: String,
    /// Optional provider zone / region. Defaults to
    /// `EDC_DO_ZONE` / `EDC_EXOSCALE_ZONE` / `fra1` / `ch-gva-2`.
    #[serde(default)]
    pub zone: Option<String>,
    /// Provider instance size (e.g. `s-2vcpu-4gb`, `standard.medium`).
    /// Defaults to `EDC_DO_SIZE` / `EDC_EXOSCALE_SIZE` / a conservative
    /// per-provider fallback.
    #[serde(default)]
    pub instance_size: Option<String>,
    /// `witness`, `rpc`, or `seeder`. Defaults to `rpc`.
    #[serde(default)]
    pub node_kind: Option<String>,
    /// OpenSSH pubkey authorised on the droplet (`ssh-ed25519 AAAA…`
    /// or `ssh-rsa AAAA…`). Optional: when empty, only the DO
    /// account-level SSH keys and the console-provisioned
    /// `authorized_keys` from cloud-init apply.
    #[serde(default)]
    pub ssh_pubkey: Option<String>,
    /// Human-readable project / federation label; surfaces on the
    /// droplet tag `edc_project:<slug>`. Defaults to `bare-node`.
    #[serde(default)]
    pub project_name: Option<String>,
    /// Optional tenant ONCHAINID (0x…) for the deployed node's
    /// compliance metadata. When omitted, the caller wallet is used.
    #[serde(default)]
    pub tenant_onchainid: Option<String>,
    /// Optional extra tags stored on the droplet. Values are
    /// sanitised into DO's `[A-Za-z0-9:_-]` charset.
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
}

/// Query params for `GET /api/v1/ecosystem/nodes`.
#[derive(Debug, Deserialize, Default)]
pub struct ListNodesQuery {
    /// Optional filter: only return nodes on the named provider.
    #[serde(default)]
    pub provider: Option<String>,
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// `POST /api/v1/ecosystem/nodes` - deploy a single Rope node.
pub(crate) async fn provision_node(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<DeployNodeRequest>,
) -> Result<Json<Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let provider = parse_provider(&body.provider)?;
    let adapter = state
        .deployer
        .get(provider)
        .ok_or_else(|| bad(format!("provider {} is not registered", provider.as_str())))?;

    let node_kind = parse_node_kind(body.node_kind.as_deref())?;
    let tenant_did = wallet_to_did(&wallet);
    let tenant_onchainid = body
        .tenant_onchainid
        .clone()
        .unwrap_or_else(|| wallet.clone());
    let project_name = body
        .project_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "bare-node".to_string());

    let zone = body
        .zone
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_zone(provider));
    let instance_size = body
        .instance_size
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_size(provider, node_kind));
    let ssh_pubkey = body
        .ssh_pubkey
        .clone()
        .unwrap_or_else(|| std::env::var("EDC_SSH_PUBKEY").unwrap_or_default());

    let mut labels = body.labels.clone();
    labels.insert("edc_source".to_string(), "console-bare-node".to_string());
    labels.insert("edc_owner".to_string(), wallet.clone());

    let req = ProvisionRequest {
        tenant_did: tenant_did.clone(),
        tenant_onchainid,
        project_name,
        provider,
        zone,
        instance_size,
        node_kind,
        ssh_pubkey,
        labels,
    };

    match adapter.provision(&req).await {
        Ok(resp) => Ok(Json(json!({
            "ok": true,
            "instance": resp.instance,
            "dry_run": resp.dry_run,
            "note": resp.note,
            "provider": provider.as_str(),
            "tenant_did": tenant_did,
        }))),
        Err(e) => Err(upstream(format!(
            "provision failed on {}: {e}",
            provider.as_str()
        ))),
    }
}

/// `GET /api/v1/ecosystem/nodes` - list nodes owned by the caller.
///
/// Returns nodes across all providers by default; filterable to a
/// single provider via `?provider=digitalocean`. Ordering is
/// provider-declared (typically most-recent-first for DO).
pub(crate) async fn list_nodes(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(q): Query<ListNodesQuery>,
) -> Result<Json<Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let tenant_did = wallet_to_did(&wallet);

    let providers: Vec<Provider> = match q.provider.as_deref() {
        Some(name) => vec![parse_provider(name)?],
        None => state
            .deployer
            .snapshot()
            .into_iter()
            .map(|(p, _)| p)
            .collect(),
    };

    let mut instances = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    for provider in providers {
        let adapter = match state.deployer.get(provider) {
            Some(a) => a,
            None => continue,
        };
        match adapter.list(&tenant_did).await {
            Ok(mut list) => instances.append(&mut list),
            Err(e) => {
                errors.push(json!({
                    "provider": provider.as_str(),
                    "error": format!("{e}"),
                }));
            }
        }
    }

    Ok(Json(json!({
        "ok": true,
        "tenant_did": tenant_did,
        "instances": instances,
        "errors": errors,
    })))
}

/// `DELETE /api/v1/ecosystem/nodes/:provider/:id` - destroy a node.
///
/// The provider adapter enforces tenant ownership on top of the local
/// cache; DO additionally rejects with 404/403 if the caller's tag
/// does not match the droplet. The response documents whether the
/// call actually reached the cloud API (`dry_run: false`) or whether
/// it only removed a dry-run cache entry.
pub(crate) async fn destroy_node(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((provider_str, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let wallet = console_wallet(&headers)?;
    let tenant_did = wallet_to_did(&wallet);
    let provider = parse_provider(&provider_str)?;
    let adapter = state
        .deployer
        .get(provider)
        .ok_or_else(|| bad(format!("provider {} is not registered", provider.as_str())))?;

    match adapter.destroy(&tenant_did, &id).await {
        Ok(()) => Ok(Json(json!({
            "ok": true,
            "instance_id": id,
            "provider": provider.as_str(),
            "dry_run": !adapter.is_live(),
        }))),
        Err(e) => Err(upstream(format!(
            "destroy failed on {}: {e}",
            provider.as_str()
        ))),
    }
}

/// `GET /api/v1/ecosystem/providers` - list registered providers and
/// whether each one has live credentials.
///
/// This is what the console UI polls to decide whether to render the
/// "DigitalOcean (live)" badge or the "DigitalOcean (dry-run)" badge
/// next to each provider option.
pub(crate) async fn list_providers(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    // Console auth: we do not leak the provider list to unauthenticated
    // callers, since the presence of credentials is itself sensitive.
    console_wallet(&headers)?;

    let providers: Vec<Value> = state
        .deployer
        .snapshot()
        .into_iter()
        .map(|(p, live)| {
            json!({
                "name": p.as_str(),
                "live": live,
                "default_zone": default_zone(p),
                "default_size_rpc": default_size(p, NodeKind::Rpc),
                "default_size_witness": default_size(p, NodeKind::Witness),
            })
        })
        .collect();

    Ok(Json(json!({
        "ok": true,
        "providers": providers,
    })))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a wallet address into the tenant DID used by rope-deployer.
///
/// We normalise to lowercase-hex so the same wallet, whether spelled
/// with EIP-55 checksum casing or not, resolves to the same tenant
/// scope. This matches how `console_wallet` returns addresses.
fn wallet_to_did(wallet: &str) -> String {
    format!("did:dwp:{}", wallet.to_lowercase())
}

fn parse_provider(s: &str) -> Result<Provider, ApiError> {
    match s.trim().to_lowercase().as_str() {
        "local" => Ok(Provider::Local),
        "exoscale" => Ok(Provider::Exoscale),
        "digitalocean" | "do" => Ok(Provider::Digitalocean),
        other => Err(bad(format!(
            "unknown provider '{other}': expected local|exoscale|digitalocean"
        ))),
    }
}

fn parse_node_kind(s: Option<&str>) -> Result<NodeKind, ApiError> {
    let raw = s.unwrap_or("rpc").trim().to_lowercase();
    match raw.as_str() {
        "" | "rpc" => Ok(NodeKind::Rpc),
        "witness" => Ok(NodeKind::Witness),
        "seeder" => Ok(NodeKind::Seeder),
        other => Err(bad(format!(
            "unknown node_kind '{other}': expected witness|rpc|seeder"
        ))),
    }
}

fn default_zone(provider: Provider) -> String {
    let var = match provider {
        Provider::Digitalocean => "EDC_DO_ZONE",
        Provider::Exoscale => "EDC_EXOSCALE_ZONE",
        Provider::Local => "EDC_LOCAL_ZONE",
    };
    std::env::var(var).unwrap_or_else(|_| match provider {
        Provider::Digitalocean => "fra1".to_string(),
        Provider::Exoscale => "ch-gva-2".to_string(),
        Provider::Local => "local".to_string(),
    })
}

fn default_size(provider: Provider, kind: NodeKind) -> String {
    // Explicit per-provider env override wins.
    let var = match provider {
        Provider::Digitalocean => "EDC_DO_SIZE",
        Provider::Exoscale => "EDC_EXOSCALE_SIZE",
        Provider::Local => "EDC_LOCAL_SIZE",
    };
    if let Ok(v) = std::env::var(var) {
        if !v.trim().is_empty() {
            return v;
        }
    }
    match (provider, kind) {
        // Witness nodes are lighter than RPC nodes (no Reth datadir).
        (Provider::Digitalocean, NodeKind::Witness) => "s-1vcpu-2gb".into(),
        (Provider::Digitalocean, _) => "s-2vcpu-4gb".into(),
        (Provider::Exoscale, NodeKind::Witness) => "standard.small".into(),
        (Provider::Exoscale, _) => "standard.medium".into(),
        (Provider::Local, _) => "local".into(),
    }
}

// ---------------------------------------------------------------------------
// Local error helpers - we cannot import `bad` / `upstream` from the
// api module because they are private. Duplicate the minimal shape.
// ---------------------------------------------------------------------------

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, msg.into())
}

fn upstream(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, msg.into())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallet_to_did_is_case_insensitive() {
        assert_eq!(
            wallet_to_did("0xABCDEF0123456789ABCDEF0123456789ABCDEF01"),
            "did:dwp:0xabcdef0123456789abcdef0123456789abcdef01"
        );
        assert_eq!(
            wallet_to_did("0xabcdef0123456789abcdef0123456789abcdef01"),
            wallet_to_did("0xABCDEF0123456789ABCDEF0123456789ABCDEF01")
        );
    }

    #[test]
    fn parse_provider_accepts_aliases_and_rejects_unknown() {
        assert_eq!(parse_provider("digitalocean").unwrap(), Provider::Digitalocean);
        assert_eq!(parse_provider("DigitalOcean").unwrap(), Provider::Digitalocean);
        assert_eq!(parse_provider("do").unwrap(), Provider::Digitalocean);
        assert_eq!(parse_provider(" exoscale ").unwrap(), Provider::Exoscale);
        assert_eq!(parse_provider("local").unwrap(), Provider::Local);
        assert!(parse_provider("aws").is_err());
    }

    #[test]
    fn parse_node_kind_defaults_to_rpc() {
        assert_eq!(parse_node_kind(None).unwrap(), NodeKind::Rpc);
        assert_eq!(parse_node_kind(Some("")).unwrap(), NodeKind::Rpc);
        assert_eq!(parse_node_kind(Some("witness")).unwrap(), NodeKind::Witness);
        assert_eq!(parse_node_kind(Some("SEEDER")).unwrap(), NodeKind::Seeder);
        assert!(parse_node_kind(Some("archive")).is_err());
    }

    #[test]
    fn default_zone_falls_back_to_provider_default() {
        std::env::remove_var("EDC_DO_ZONE");
        std::env::remove_var("EDC_EXOSCALE_ZONE");
        assert_eq!(default_zone(Provider::Digitalocean), "fra1");
        assert_eq!(default_zone(Provider::Exoscale), "ch-gva-2");
    }

    #[test]
    fn default_size_distinguishes_witness_from_rpc() {
        std::env::remove_var("EDC_DO_SIZE");
        std::env::remove_var("EDC_EXOSCALE_SIZE");
        assert_eq!(default_size(Provider::Digitalocean, NodeKind::Rpc), "s-2vcpu-4gb");
        assert_eq!(default_size(Provider::Digitalocean, NodeKind::Witness), "s-1vcpu-2gb");
        assert_eq!(default_size(Provider::Exoscale, NodeKind::Rpc), "standard.medium");
        assert_eq!(default_size(Provider::Exoscale, NodeKind::Witness), "standard.small");
    }
}
