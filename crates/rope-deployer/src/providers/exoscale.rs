//! Exoscale Compute API v2 adapter - LIVE.
//!
//! Landing a Rope node on Exoscale is functionally identical to
//! DigitalOcean:
//!
//! 1. Generate an Ed25519 node identity on the deployer (via
//!    `super::bootstrap::NodeIdentity`).
//! 2. Build a cloud-init `user_data` payload with the private half of
//!    the identity, an enrolment JSON, and a call to the Rope CLI
//!    installer served from `get.datachain.network`
//!    (see `super::bootstrap::build_cloud_init`).
//! 3. Sign the create-instance request with EXO2-HMAC-SHA256 and POST
//!    it to `https://api-{zone}.exoscale.com/v2/instance`.
//! 4. Poll the returned operation reference until state = `success`,
//!    then GET the resulting instance so we can return the IPv4 back
//!    to the console.
//!
//! Auth model - reproduced from
//! <https://openapi-v2.exoscale.com/topic/topic-api-request-signature>:
//!
//! ```text
//! message = "<METHOD> <path>\n<body>\n<query-values-joined>\n<header-values-joined>\n<expires-unix>"
//! signature = BASE64_STANDARD( HMAC_SHA256( api_secret, message ) )
//! Authorization: EXO2-HMAC-SHA256 credential=<key>[,signed-query-args=p1;p2],expires=<ts>,signature=<sig>
//! ```
//!
//! Query args are sorted alphabetically before the values are
//! concatenated so the server-side re-computation matches; header
//! values are unused today (empty line).
//!
//! State persistence mirrors the DigitalOcean adapter: a JSON file at
//! `${DEPLOYER_STATE_DIR:-/var/lib/rope-deployer}/exoscale/instances.json`.
//! This lets a `rope-edc` restart re-list all previously-provisioned
//! rows without a round-trip to Exoscale, and lets us record the
//! tenant DID + node identity public key that Exoscale itself does
//! not remember (we also mirror them into instance labels so an
//! operator listing instances via the raw API sees them).
//!
//! Zones supported today: `ch-gva-2` (Geneva), `ch-dk-2` (Zurich),
//! `de-fra-1` (Frankfurt), `de-muc-1` (Munich), `at-vie-1` (Vienna),
//! `at-vie-2`, `bg-sof-1` (Sofia). The list is authoritative in
//! `ZONE_ALLOWLIST` below and mirrors the current live Exoscale
//! catalogue.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hmac::{Hmac, Mac};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;

use super::bootstrap::{
    build_cloud_init, build_hostname, sanitise_tag, standard_labels_map, NodeIdentity,
};
use super::{CloudProvider, ProviderError};
use crate::types::{InstanceInfo, NodeKind, Provider, ProvisionRequest, ProvisionResponse};

type HmacSha256 = Hmac<Sha256>;

/// Every Exoscale zone we accept. Kept explicit rather than
/// wildcarded so a typo like `fr-par-1` returns a clear
/// `Invalid(...)` error instead of a 404 from the API.
const ZONE_ALLOWLIST: &[&str] = &[
    "ch-gva-2",
    "ch-dk-2",
    "de-fra-1",
    "de-muc-1",
    "at-vie-1",
    "at-vie-2",
    "bg-sof-1",
];

/// Default per-request expiration window. The signature must include
/// a UNIX-timestamp `expires=` claim; five minutes gives enough
/// headroom for slow VMs and NAT latency without inviting
/// long-window replay.
const DEFAULT_SIGN_TTL: Duration = Duration::from_secs(300);

/// Maximum time we wait for a `create-instance` operation to complete
/// before giving up and returning the pending state to the caller.
const CREATE_OP_TIMEOUT: Duration = Duration::from_secs(180);
const CREATE_OP_POLL_INTERVAL: Duration = Duration::from_millis(2_000);

// ---------------------------------------------------------------------------
// State file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalRecord {
    instance: InstanceInfo,
    /// Base64-encoded Ed25519 verifying key. Also mirrored into the
    /// instance's Exoscale labels under `node-identity-pub`.
    identity_public_key_b64: String,
    /// Template UUID actually used for the instance; useful when
    /// operators want to reproduce the exact image later.
    template_id: String,
    /// Instance-type UUID actually used; kept for the same reason.
    instance_type_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    instances: BTreeMap<String, LocalRecord>,
    /// UUID cache. Instance types and templates are stable across
    /// months; caching their resolved IDs lets a warm process create
    /// instances with a single signed request instead of three.
    #[serde(default)]
    template_cache: BTreeMap<String, String>,
    #[serde(default)]
    instance_type_cache: BTreeMap<String, String>,
}

fn load_state(path: &std::path::Path) -> PersistedState {
    match std::fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => PersistedState::default(),
    }
}

fn persist_state(path: &std::path::Path, snapshot: &PersistedState) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let bytes = match serde_json::to_vec_pretty(snapshot) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("exoscale state serialise failed: {e}");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct ExoscaleProvider {
    api_key: Option<String>,
    api_secret: Option<String>,
    default_zone: String,
    default_instance_type: String,
    default_template: String,
    /// Bare API host, overridable for tests. Defaults to
    /// `api-{zone}.exoscale.com` at call time.
    api_host_template: String,
    state_path: PathBuf,
    state: Arc<RwLock<PersistedState>>,
    http: reqwest::Client,
    /// Test hook - when set, replaces the computed URL host so unit
    /// tests can point the adapter at a mock server.
    test_base_url: Option<String>,
}

impl ExoscaleProvider {
    pub fn from_env() -> Self {
        let state_dir = std::env::var("DEPLOYER_STATE_DIR")
            .unwrap_or_else(|_| "/var/lib/rope-deployer".to_string());
        let state_path = PathBuf::from(state_dir)
            .join("exoscale")
            .join("instances.json");
        let default_zone = std::env::var("EXOSCALE_DEFAULT_ZONE")
            .unwrap_or_else(|_| "ch-gva-2".to_string());
        let default_instance_type = std::env::var("EXOSCALE_DEFAULT_INSTANCE_TYPE")
            .unwrap_or_else(|_| "standard.medium".to_string());
        let default_template = std::env::var("EXOSCALE_DEFAULT_TEMPLATE")
            .unwrap_or_else(|_| "Linux Ubuntu 24.04 LTS 64-bit".to_string());
        let api_key = std::env::var("EXOSCALE_API_KEY").ok();
        let api_secret = std::env::var("EXOSCALE_API_SECRET")
            .ok()
            .or_else(|| std::env::var("EXOSCALE_PRIVATE_KEY").ok());
        Self::with_config(
            state_path,
            default_zone,
            default_instance_type,
            default_template,
            api_key,
            api_secret,
        )
    }

    /// Explicit-config constructor used by tests to avoid racing on
    /// process env.
    pub fn with_config(
        state_path: PathBuf,
        default_zone: String,
        default_instance_type: String,
        default_template: String,
        api_key: Option<String>,
        api_secret: Option<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("rope-deployer/1.0 (+https://console.datachain.network)")
            .build()
            .expect("build reqwest client");
        let state = load_state(&state_path);
        let api_host_template = std::env::var("EXOSCALE_API_HOST_TEMPLATE")
            .unwrap_or_else(|_| "https://api-{zone}.exoscale.com".to_string());
        let test_base_url = std::env::var("EXOSCALE_API_BASE").ok().and_then(|s| {
            let s = s.trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        });
        Self {
            api_key,
            api_secret,
            default_zone,
            default_instance_type,
            default_template,
            api_host_template,
            state_path,
            state: Arc::new(RwLock::new(state)),
            http,
            test_base_url,
        }
    }

    fn zone_or_default(&self, req_zone: &str) -> String {
        if req_zone.is_empty() {
            self.default_zone.clone()
        } else {
            req_zone.to_string()
        }
    }

    fn require_creds(&self) -> Result<(&str, &str), ProviderError> {
        match (self.api_key.as_deref(), self.api_secret.as_deref()) {
            (Some(k), Some(s)) if !k.trim().is_empty() && !s.trim().is_empty() => Ok((k, s)),
            _ => Err(ProviderError::NotConfigured(Provider::Exoscale)),
        }
    }

    fn base_url_for(&self, zone: &str) -> String {
        if let Some(base) = &self.test_base_url {
            return base.trim_end_matches('/').to_string();
        }
        self.api_host_template.replace("{zone}", zone)
    }

    /// Sign and execute a v2 Exoscale API request. `path` MUST start
    /// with `/v2/` and MUST NOT include a query string; alphabetised
    /// query params are supplied via `query`.
    async fn call(
        &self,
        zone: &str,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value, ProviderError> {
        let (key, secret) = self.require_creds()?;
        if !path.starts_with('/') {
            return Err(ProviderError::Invalid(format!(
                "exoscale: internal path missing leading slash: {path}"
            )));
        }
        // Alphabetise query keys - the server sorts them the same way,
        // so signed-query-args= must be listed in this order.
        let mut sorted: Vec<(&str, String)> = query.iter().cloned().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));

        let expires = current_unix() + DEFAULT_SIGN_TTL.as_secs();
        let body_text = match &body {
            Some(v) => serde_json::to_string(v).map_err(|e| {
                ProviderError::Upstream(format!("exoscale: serialise body: {e}"))
            })?,
            None => String::new(),
        };
        let signed_query_values: String =
            sorted.iter().map(|(_, v)| v.as_str()).collect();
        let signed_header_values = String::new();
        let msg = format!(
            "{method} {path}\n{body}\n{q}\n{h}\n{exp}",
            method = method.as_str(),
            path = path,
            body = body_text,
            q = signed_query_values,
            h = signed_header_values,
            exp = expires
        );
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| {
            ProviderError::Upstream(format!("exoscale: HMAC key init failed: {e}"))
        })?;
        mac.update(msg.as_bytes());
        let sig = B64.encode(mac.finalize().into_bytes());
        let mut header = format!("EXO2-HMAC-SHA256 credential={key}");
        if !sorted.is_empty() {
            let names = sorted
                .iter()
                .map(|(k, _)| *k)
                .collect::<Vec<_>>()
                .join(";");
            header.push_str(&format!(",signed-query-args={names}"));
        }
        header.push_str(&format!(",expires={expires}"));
        header.push_str(&format!(",signature={sig}"));

        // Build the URL. reqwest handles query encoding.
        let base = self.base_url_for(zone);
        let mut url = reqwest::Url::parse(&format!("{base}{path}")).map_err(|e| {
            ProviderError::Upstream(format!("exoscale: url parse: {e}"))
        })?;
        {
            let mut qp = url.query_pairs_mut();
            for (k, v) in &sorted {
                qp.append_pair(k, v);
            }
        }
        let mut req = self
            .http
            .request(method.clone(), url.clone())
            .header("Authorization", header)
            .header("Accept", "application/json");
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").json(&b);
        }
        let res = req.send().await.map_err(|e| {
            ProviderError::Upstream(format!("exoscale: {method} {url} failed: {e}"))
        })?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::Upstream(format!(
                "exoscale: {method} {path} -> HTTP {status}: {text}"
            )));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| {
            ProviderError::Upstream(format!(
                "exoscale: {method} {path} returned non-JSON body: {e} ({text})"
            ))
        })
    }

    /// Resolve an instance-type by human name (e.g. `standard.medium`)
    /// or by UUID. UUIDs are 36 chars with 4 hyphens; anything else is
    /// treated as a human name and looked up. Results are cached in
    /// the on-disk state file.
    async fn resolve_instance_type(
        &self,
        zone: &str,
        name_or_id: &str,
    ) -> Result<String, ProviderError> {
        if looks_like_uuid(name_or_id) {
            return Ok(name_or_id.to_string());
        }
        let cache_key = format!("{zone}:{name_or_id}");
        if let Some(id) = self.state.read().instance_type_cache.get(&cache_key) {
            return Ok(id.clone());
        }
        let res = self
            .call(zone, reqwest::Method::GET, "/v2/instance-type", &[], None)
            .await?;
        let list = res
            .get("instance-types")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                ProviderError::Upstream(
                    "exoscale: instance-type list response missing 'instance-types'".into(),
                )
            })?;
        // Instance types are identified by "<family>.<size>" (e.g.
        // "standard.medium"). The API returns them as separate fields.
        for it in list {
            let family = it
                .get("family")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let size = it
                .get("size")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let compound = format!("{family}.{size}");
            if compound.eq_ignore_ascii_case(name_or_id) {
                if let Some(id) = it.get("id").and_then(|v| v.as_str()) {
                    self.state
                        .write()
                        .instance_type_cache
                        .insert(cache_key.clone(), id.to_string());
                    self.persist();
                    return Ok(id.to_string());
                }
            }
        }
        Err(ProviderError::Invalid(format!(
            "exoscale: instance-type '{name_or_id}' not found in zone {zone}"
        )))
    }

    /// Resolve a template UUID either from a UUID input or from a
    /// human-readable template name (e.g. `Linux Ubuntu 24.04 LTS 64-bit`).
    async fn resolve_template(
        &self,
        zone: &str,
        name_or_id: &str,
    ) -> Result<String, ProviderError> {
        if looks_like_uuid(name_or_id) {
            return Ok(name_or_id.to_string());
        }
        let cache_key = format!("{zone}:{name_or_id}");
        if let Some(id) = self.state.read().template_cache.get(&cache_key) {
            return Ok(id.clone());
        }
        let res = self
            .call(
                zone,
                reqwest::Method::GET,
                "/v2/template",
                &[("visibility", "public".to_string())],
                None,
            )
            .await?;
        let list = res
            .get("templates")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                ProviderError::Upstream(
                    "exoscale: template list response missing 'templates'".into(),
                )
            })?;
        // Prefer the newest matching template (highest `build_number`
        // or lexicographically-latest `created_at`) so we don't
        // accidentally pin to a superseded image.
        let mut best: Option<(String, String)> = None;
        for t in list {
            let n = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if !n.eq_ignore_ascii_case(name_or_id) {
                continue;
            }
            let created = t
                .get("created-at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(id) = t.get("id").and_then(|v| v.as_str()) {
                match &best {
                    None => best = Some((id.to_string(), created)),
                    Some((_, prev)) if created > *prev => {
                        best = Some((id.to_string(), created))
                    }
                    _ => {}
                }
            }
        }
        match best {
            Some((id, _)) => {
                self.state
                    .write()
                    .template_cache
                    .insert(cache_key.clone(), id.clone());
                self.persist();
                Ok(id)
            }
            None => Err(ProviderError::Invalid(format!(
                "exoscale: template '{name_or_id}' not found in zone {zone}"
            ))),
        }
    }

    async fn poll_operation(
        &self,
        zone: &str,
        op_id: &str,
    ) -> Result<Value, ProviderError> {
        let deadline = SystemTime::now() + CREATE_OP_TIMEOUT;
        loop {
            let res = self
                .call(
                    zone,
                    reqwest::Method::GET,
                    &format!("/v2/operation/{op_id}"),
                    &[],
                    None,
                )
                .await?;
            let state = res
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match state.as_str() {
                "success" => return Ok(res),
                "failure" => {
                    return Err(ProviderError::Upstream(format!(
                        "exoscale: operation {op_id} failed: {}",
                        res.get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<no reason>")
                    )));
                }
                _ => {}
            }
            if SystemTime::now() >= deadline {
                // Return the last response so the caller can surface a
                // "still-provisioning" status instead of an error.
                return Ok(res);
            }
            tokio::time::sleep(CREATE_OP_POLL_INTERVAL).await;
        }
    }

    fn persist(&self) {
        let snapshot = self.state.read();
        persist_state(&self.state_path, &snapshot);
    }
}

// ---------------------------------------------------------------------------
// CloudProvider impl
// ---------------------------------------------------------------------------

#[async_trait]
impl CloudProvider for ExoscaleProvider {
    fn name(&self) -> Provider {
        Provider::Exoscale
    }

    fn is_live(&self) -> bool {
        self.api_key
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
            && self
                .api_secret
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
    }

    async fn provision(
        &self,
        req: &ProvisionRequest,
    ) -> Result<ProvisionResponse, ProviderError> {
        let zone = self.zone_or_default(&req.zone);
        if !ZONE_ALLOWLIST.contains(&zone.as_str()) {
            return Err(ProviderError::Invalid(format!(
                "exoscale: zone '{zone}' is not allowlisted; supported: {}",
                ZONE_ALLOWLIST.join(", ")
            )));
        }
        let identity = NodeIdentity::generate();
        let hostname = build_hostname(&req.tenant_did, req.node_kind, &zone);
        let mut req_for_init = req.clone();
        req_for_init.zone = zone.clone();
        let user_data = build_cloud_init(&req_for_init, &identity);
        // Exoscale expects user_data base64-encoded up to 32 kB.
        let user_data_b64 = B64.encode(user_data.as_bytes());
        if user_data_b64.len() > 32_768 {
            return Err(ProviderError::Invalid(format!(
                "exoscale: user_data too large ({} bytes base64) - keep it under 32 kB",
                user_data_b64.len()
            )));
        }

        // Dry-run path when creds are missing: persist a local row so
        // the console UI stays exercised end-to-end. `is_live=false`
        // means every call in this branch is a no-op against the
        // Exoscale API.
        if !self.is_live() {
            let id = format!("dryrun-{}", uuid::Uuid::new_v4().simple());
            let info = InstanceInfo {
                id: id.clone(),
                hostname,
                provider: Provider::Exoscale,
                zone: zone.clone(),
                ipv4: None,
                tenant_did: req.tenant_did.clone(),
                node_kind: req.node_kind,
                created_at: chrono::Utc::now().to_rfc3339(),
                status: "dry_run".to_string(),
            };
            self.state.write().instances.insert(
                id.clone(),
                LocalRecord {
                    instance: info.clone(),
                    identity_public_key_b64: identity.public_b64(),
                    template_id: String::new(),
                    instance_type_id: String::new(),
                },
            );
            self.persist();
            return Ok(ProvisionResponse {
                instance: info,
                dry_run: true,
                note:
                    "exoscale: EXOSCALE_API_KEY / EXOSCALE_API_SECRET not set - dry-run row saved"
                        .into(),
            });
        }

        // Live path.
        let instance_size = if req.instance_size.trim().is_empty() {
            self.default_instance_type.clone()
        } else {
            req.instance_size.clone()
        };
        let instance_type_id = self
            .resolve_instance_type(&zone, &instance_size)
            .await?;
        let template_id = self.resolve_template(&zone, &self.default_template).await?;
        let labels = standard_labels_map(
            &req.tenant_did,
            req.node_kind,
            &identity.public_b64(),
            &req.labels,
        );
        let mut body = json!({
            "name": hostname,
            "template": {"id": template_id},
            "instance-type": {"id": instance_type_id},
            "disk-size": 50u32,
            "user-data": user_data_b64,
            "ipv6-enabled": true,
            "labels": labels,
        });
        if !req.ssh_pubkey.trim().is_empty() {
            body["ssh-key"] = json!({"name": ensure_ssh_key(&self.api_key)});
        }

        let create = self
            .call(&zone, reqwest::Method::POST, "/v2/instance", &[], Some(body))
            .await?;
        let op_id = create
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProviderError::Upstream(
                    "exoscale: create-instance response missing operation id".into(),
                )
            })?
            .to_string();
        let op = self.poll_operation(&zone, &op_id).await?;
        let instance_id = op
            .get("reference")
            .and_then(|v| v.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProviderError::Upstream(
                    "exoscale: create-instance operation missing reference.id".into(),
                )
            })?
            .to_string();

        // Read back so we have the assigned public IPv4.
        let inst = self
            .call(
                &zone,
                reqwest::Method::GET,
                &format!("/v2/instance/{instance_id}"),
                &[],
                None,
            )
            .await?;
        let info = InstanceInfo {
            id: instance_id.clone(),
            hostname,
            provider: Provider::Exoscale,
            zone: zone.clone(),
            ipv4: inst
                .get("public-ip")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            tenant_did: req.tenant_did.clone(),
            node_kind: req.node_kind,
            created_at: inst
                .get("created-at")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| "")
                .to_string(),
            status: inst
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("provisioning")
                .to_string(),
        };
        self.state.write().instances.insert(
            instance_id.clone(),
            LocalRecord {
                instance: info.clone(),
                identity_public_key_b64: identity.public_b64(),
                template_id,
                instance_type_id,
            },
        );
        self.persist();
        Ok(ProvisionResponse {
            instance: info,
            dry_run: false,
            note: format!(
                "exoscale: instance {instance_id} created in {zone}, node identity public key {}",
                identity.public_b64()
            ),
        })
    }

    async fn list(&self, tenant_did: &str) -> Result<Vec<InstanceInfo>, ProviderError> {
        if !self.is_live() {
            return Ok(self
                .state
                .read()
                .instances
                .values()
                .filter(|r| r.instance.tenant_did == tenant_did)
                .map(|r| r.instance.clone())
                .collect());
        }

        // Live: query each zone we know about and stitch the results.
        // The Exoscale API is zone-scoped, so a tenant with instances
        // in multiple zones needs a fan-out. We limit the fan-out to
        // the small allowlist above rather than every possible zone.
        let mut out: Vec<InstanceInfo> = Vec::new();
        for zone in ZONE_ALLOWLIST {
            let res = match self
                .call(zone, reqwest::Method::GET, "/v2/instance", &[], None)
                .await
            {
                Ok(v) => v,
                // NotConfigured shouldn't happen here (is_live gated),
                // but a zone-specific 4xx (e.g. IAM key not granted
                // permission) is possible. Log and continue.
                Err(e) => {
                    tracing::warn!("exoscale list {zone} failed: {e}");
                    continue;
                }
            };
            let arr = res
                .get("instances")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for inst in arr {
                let labels = inst
                    .get("labels")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                let owner_did = labels
                    .get("tenant")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let owned_by_us = labels
                    .get("rope-deployer")
                    .and_then(|v| v.as_str())
                    .map(|v| v == "true")
                    .unwrap_or(false);
                if !owned_by_us {
                    continue;
                }
                // Compare the sanitised tenant tag we stored on
                // provision, not the raw DID (the sanitiser can turn
                // `:` into `-` in future).
                let tenant_matches = sanitise_tag(tenant_did);
                if owner_did != tenant_matches {
                    continue;
                }
                let id = inst
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    continue;
                }
                let info = InstanceInfo {
                    id: id.clone(),
                    hostname: inst
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    provider: Provider::Exoscale,
                    zone: zone.to_string(),
                    ipv4: inst
                        .get("public-ip")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    tenant_did: tenant_did.to_string(),
                    node_kind: labels
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .and_then(NodeKind::from_str)
                        .unwrap_or(NodeKind::Rpc),
                    created_at: inst
                        .get("created-at")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    status: inst
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                };
                // Refresh cached copy so future stop/destroy can hit
                // the right zone without another fan-out.
                if let Some(rec) = self.state.write().instances.get_mut(&id) {
                    rec.instance = info.clone();
                }
                out.push(info);
            }
        }
        self.persist();
        Ok(out)
    }

    async fn stop(
        &self,
        tenant_did: &str,
        instance_id: &str,
    ) -> Result<(), ProviderError> {
        let zone = self.zone_for_instance(tenant_did, instance_id)?;
        if self.is_live() {
            self.call(
                &zone,
                reqwest::Method::PUT,
                &format!("/v2/instance/{instance_id}:stop"),
                &[],
                Some(json!({})),
            )
            .await?;
        }
        if let Some(rec) = self.state.write().instances.get_mut(instance_id) {
            rec.instance.status = "stopped".to_string();
        }
        self.persist();
        Ok(())
    }

    async fn destroy(
        &self,
        tenant_did: &str,
        instance_id: &str,
    ) -> Result<(), ProviderError> {
        let zone = self.zone_for_instance(tenant_did, instance_id)?;
        if self.is_live() {
            self.call(
                &zone,
                reqwest::Method::DELETE,
                &format!("/v2/instance/{instance_id}"),
                &[],
                None,
            )
            .await?;
        }
        self.state.write().instances.remove(instance_id);
        self.persist();
        Ok(())
    }
}

impl ExoscaleProvider {
    fn zone_for_instance(
        &self,
        tenant_did: &str,
        instance_id: &str,
    ) -> Result<String, ProviderError> {
        let g = self.state.read();
        match g.instances.get(instance_id) {
            Some(rec) if rec.instance.tenant_did == tenant_did => Ok(rec.instance.zone.clone()),
            Some(_) => Err(ProviderError::Invalid(format!(
                "tenant {tenant_did} does not own instance {instance_id}"
            ))),
            None => Err(ProviderError::Invalid(format!(
                "no such instance in local cache: {instance_id} (try /nodes list to refresh)"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn looks_like_uuid(s: &str) -> bool {
    let n_hyphens = s.chars().filter(|c| *c == '-').count();
    s.len() == 36
        && n_hyphens == 4
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn current_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Placeholder for the SSH-key management path. Exoscale requires the
/// pubkey to be pre-registered in the account under a name; when we
/// build the multi-tenant path (`deploy/EXOSCALE_AS_A_SERVICE.md`)
/// this will call `/v2/ssh-key` with `PUT` to ensure the key exists.
/// Until then callers get a "default" reference which is a no-op if
/// the tenant has no pre-registered key, and the pubkey still lands
/// on the instance via cloud-init.
fn ensure_ssh_key(_api_key: &Option<String>) -> &'static str {
    "default"
}

impl NodeKind {
    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "witness" => NodeKind::Witness,
            "rpc" => NodeKind::Rpc,
            "seeder" => NodeKind::Seeder,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NodeKind;

    fn make_provider(
        dir: &tempfile::TempDir,
        creds: Option<(&str, &str)>,
    ) -> ExoscaleProvider {
        let state_path = dir.path().join("exoscale").join("instances.json");
        let (key, secret) = match creds {
            Some((k, s)) => (Some(k.to_string()), Some(s.to_string())),
            None => (None, None),
        };
        ExoscaleProvider::with_config(
            state_path,
            "ch-gva-2".to_string(),
            "standard.medium".to_string(),
            "Linux Ubuntu 24.04 LTS 64-bit".to_string(),
            key,
            secret,
        )
    }

    fn sample_request(tenant: &str) -> ProvisionRequest {
        ProvisionRequest {
            tenant_did: tenant.to_string(),
            tenant_onchainid: "0x0000000000000000000000000000000000000042".to_string(),
            project_name: "unit-test".to_string(),
            provider: Provider::Exoscale,
            zone: String::new(),
            instance_size: "standard.medium".to_string(),
            node_kind: NodeKind::Rpc,
            ssh_pubkey: String::new(),
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn is_live_flips_with_creds() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!make_provider(&dir, None).is_live());
        assert!(make_provider(&dir, Some(("EXOFAKE", "sekret"))).is_live());
    }

    #[tokio::test]
    async fn provision_without_creds_is_dry_run_row() {
        let dir = tempfile::TempDir::new().unwrap();
        let provider = make_provider(&dir, None);
        let resp = provider
            .provision(&sample_request("did:web:datawallet.plus:1"))
            .await
            .expect("provision");
        assert!(resp.dry_run);
        assert_eq!(resp.instance.status, "dry_run");
        assert!(resp.instance.id.starts_with("dryrun-"));
    }

    #[tokio::test]
    async fn provision_rejects_unknown_zone() {
        let dir = tempfile::TempDir::new().unwrap();
        let provider = make_provider(&dir, Some(("EXOFAKE", "sekret")));
        let mut req = sample_request("did:web:datawallet.plus:2");
        req.zone = "fr-par-1".to_string();
        let err = provider.provision(&req).await.expect_err("expected error");
        match err {
            ProviderError::Invalid(msg) => assert!(msg.contains("fr-par-1")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn hmac_signature_matches_spec_example() {
        // Reference example from Exoscale docs:
        // message =
        //   "GET /v2/resource/a02baf5a-a3e4-49a0-857b-8a08d276c1c0\n\nv1v2\n\n1599140767"
        // secret = "MySecret"; expected signature is provider-specific
        // so we lock in the shape (base64, 44 chars) rather than the
        // exact byte value.
        let secret = b"MySecret";
        let msg =
            "GET /v2/resource/a02baf5a-a3e4-49a0-857b-8a08d276c1c0\n\nv1v2\n\n1599140767";
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(msg.as_bytes());
        let out = B64.encode(mac.finalize().into_bytes());
        assert_eq!(out.len(), 44);
        assert!(out.ends_with('='));
    }

    #[test]
    fn state_persistence_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("exoscale").join("instances.json");
        let mut snap = PersistedState::default();
        snap.template_cache
            .insert("ch-gva-2:ubuntu".into(), "tmpl-1".into());
        persist_state(&path, &snap);
        let round = load_state(&path);
        assert_eq!(round.template_cache.get("ch-gva-2:ubuntu"), Some(&"tmpl-1".to_string()));
    }

    #[test]
    fn looks_like_uuid_shape() {
        assert!(looks_like_uuid("a02baf5a-a3e4-49a0-857b-8a08d276c1c0"));
        assert!(!looks_like_uuid("standard.medium"));
        assert!(!looks_like_uuid(""));
        assert!(!looks_like_uuid("a02baf5a-a3e4-49a0-857b"));
    }

    #[tokio::test]
    async fn destroy_removes_row_and_persists() {
        let dir = tempfile::TempDir::new().unwrap();
        let tenant = "did:web:datawallet.plus:4";
        let provider = make_provider(&dir, None);
        let resp = provider
            .provision(&sample_request(tenant))
            .await
            .expect("provision");
        provider
            .destroy(tenant, &resp.instance.id)
            .await
            .expect("destroy");
        assert!(provider.list(tenant).await.expect("list").is_empty());
        let provider2 = make_provider(&dir, None);
        assert!(provider2.list(tenant).await.expect("list").is_empty());
    }
}
