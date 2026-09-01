//! DigitalOcean adapter - live v2 Droplets API.
//!
//! Design:
//! * A single reusable `reqwest::Client` with a 30 s timeout.
//! * Bearer-token auth via `Authorization: Bearer $DIGITALOCEAN_TOKEN`.
//!   The env can also be spelled `DIGITALOCEAN_API_KEY` or
//!   `DIGITALOCEAN_API` (the .env in this repo uses the latter forms),
//!   so we accept all three.
//! * Every droplet we create is tagged with `rope-deployer` and
//!   `tenant:<did-slug>` so `list()` can reliably scope droplets to a
//!   tenant without us having to maintain a stateful mirror of DO
//!   account contents. A local JSON file at
//!   `$DEPLOYER_STATE_DIR/digitalocean/instances.json` is used as a
//!   secondary cache (metadata like `tenant_did`, `node_kind`, and the
//!   node-identity public key that we cannot recover from DO alone).
//! * `provision()` also generates an Ed25519 node-identity keypair on
//!   the deployer and injects the private half into cloud-init user
//!   data. The public half is returned in the response so the console
//!   can display it and the Foundation can pin it into the master-node
//!   registry when the tenant onboards as a witness. The exact cloud-
//!   init script and identity handling are shared with the Exoscale
//!   adapter via `providers::bootstrap` - see that module for the
//!   locked-in shape.
//! * When no token is configured we fall back to the same in-memory
//!   dry-run behaviour as the local provider, so integration tests
//!   keep working offline.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::bootstrap::{
    build_cloud_init, build_hostname, standard_tags, tenant_tag_for, NodeIdentity,
};
use super::{CloudProvider, ProviderError};
use crate::types::{InstanceInfo, NodeKind, Provider, ProvisionRequest, ProvisionResponse};

/// Base URL of the DigitalOcean v2 API. Overridable for tests via
/// `DIGITALOCEAN_API_BASE`.
fn api_base() -> String {
    std::env::var("DIGITALOCEAN_API_BASE")
        .unwrap_or_else(|_| "https://api.digitalocean.com".to_string())
}

/// Read the DO token from any of the three accepted spellings.
fn read_token() -> Option<String> {
    for key in [
        "DIGITALOCEAN_TOKEN",
        "DIGITALOCEAN_API_KEY",
        "DIGITALOCEAN_API",
    ] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Where the local state cache lives.
fn state_path() -> PathBuf {
    let base = std::env::var("DEPLOYER_STATE_DIR")
        .unwrap_or_else(|_| "/var/lib/rope-deployer".to_string());
    PathBuf::from(base).join("digitalocean").join("instances.json")
}

/// Extra metadata we track alongside every droplet - DO doesn't
/// remember which tenant DID owns a droplet by itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalRecord {
    instance: InstanceInfo,
    /// Base64-encoded Ed25519 verifying key of the node identity.
    identity_public_key_b64: String,
}

pub struct DigitalOceanProvider {
    api_token: Option<String>,
    default_region: String,
    ssh_key_ids: Vec<u64>,
    image_slug: String,
    state: Arc<RwLock<BTreeMap<String, LocalRecord>>>,
    http: Option<reqwest::Client>,
}

impl DigitalOceanProvider {
    pub fn from_env() -> Self {
        let api_token = read_token();
        let default_region = std::env::var("DIGITALOCEAN_DEFAULT_REGION")
            .unwrap_or_else(|_| "fra1".to_string());
        let image_slug = std::env::var("DIGITALOCEAN_IMAGE_SLUG")
            .unwrap_or_else(|_| "ubuntu-24-04-x64".to_string());
        let ssh_key_ids = std::env::var("DIGITALOCEAN_SSH_KEY_IDS")
            .ok()
            .map(|s| {
                s.split(',')
                    .filter_map(|s| s.trim().parse::<u64>().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let http = api_token.as_ref().map(|_| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent("rope-deployer/1.0 (+https://console.datachain.network)")
                .build()
                .expect("build reqwest client")
        });

        let state = Arc::new(RwLock::new(load_state()));

        Self {
            api_token,
            default_region,
            ssh_key_ids,
            image_slug,
            state,
            http,
        }
    }

    fn resolve_region(&self, req_zone: &str) -> String {
        if req_zone.is_empty() {
            self.default_region.clone()
        } else {
            req_zone.to_string()
        }
    }

    fn require_client(&self) -> Result<(&reqwest::Client, &str), ProviderError> {
        match (&self.http, &self.api_token) {
            (Some(c), Some(t)) => Ok((c, t.as_str())),
            _ => Err(ProviderError::NotConfigured(Provider::Digitalocean)),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ProviderError> {
        let (client, token) = self.require_client()?;
        let url = format!("{}{}", api_base(), path);
        let mut req = client
            .request(method.clone(), &url)
            .bearer_auth(token)
            .header("content-type", "application/json");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.map_err(|e| {
            ProviderError::Upstream(format!("digitalocean: {method} {url} failed: {e}"))
        })?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::Upstream(format!(
                "digitalocean: {method} {path} -> HTTP {status}: {text}"
            )));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| {
            ProviderError::Upstream(format!(
                "digitalocean: {method} {path} returned non-JSON body: {e} ({text})"
            ))
        })
    }
}

#[async_trait]
impl CloudProvider for DigitalOceanProvider {
    fn name(&self) -> Provider {
        Provider::Digitalocean
    }

    fn is_live(&self) -> bool {
        self.http.is_some() && self.api_token.is_some()
    }

    async fn provision(
        &self,
        req: &ProvisionRequest,
    ) -> Result<ProvisionResponse, ProviderError> {
        let region = self.resolve_region(&req.zone);
        let tenant_tag = tenant_tag_for(&req.tenant_did);
        let identity = NodeIdentity::generate();
        let hostname = build_hostname(&req.tenant_did, req.node_kind, &region);
        // Normalise the request so the enrolment record matches the
        // droplet's real region (not the empty zone the caller may
        // have sent).
        let mut req_for_init = req.clone();
        req_for_init.zone = region.clone();
        let user_data = build_cloud_init(&req_for_init, &identity);

        if !self.is_live() {
            let now = chrono::Utc::now().to_rfc3339();
            let id = format!("dryrun-{}", uuid::Uuid::new_v4().simple());
            let info = InstanceInfo {
                id: id.clone(),
                hostname,
                provider: Provider::Digitalocean,
                zone: region,
                ipv4: None,
                tenant_did: req.tenant_did.clone(),
                node_kind: req.node_kind,
                created_at: now,
                status: "dry_run".to_string(),
            };
            self.state.write().insert(
                id.clone(),
                LocalRecord {
                    instance: info.clone(),
                    identity_public_key_b64: identity.public_b64(),
                },
            );
            persist_state(&self.state.read());
            return Ok(ProvisionResponse {
                instance: info,
                dry_run: true,
                note: "digitalocean: DIGITALOCEAN_TOKEN not set - dry-run record persisted"
                    .into(),
            });
        }

        let mut body = json!({
            "name": hostname,
            "region": region,
            "size": req.instance_size,
            "image": self.image_slug,
            "backups": false,
            "ipv6": true,
            "monitoring": true,
            "user_data": user_data,
            "tags": standard_tags(&tenant_tag, req.node_kind, &req.labels),
        });
        if !self.ssh_key_ids.is_empty() {
            body["ssh_keys"] = json!(self.ssh_key_ids);
        }

        let created = self
            .call(reqwest::Method::POST, "/v2/droplets", Some(body))
            .await?;
        let droplet = created.get("droplet").ok_or_else(|| {
            ProviderError::Upstream(
                "digitalocean: create response missing 'droplet' field".into(),
            )
        })?;
        let id = droplet
            .get("id")
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string())
            .ok_or_else(|| {
                ProviderError::Upstream(
                    "digitalocean: create response missing droplet.id".into(),
                )
            })?;
        let created_at = droplet
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status = droplet
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("new")
            .to_string();

        let info = InstanceInfo {
            id: id.clone(),
            hostname,
            provider: Provider::Digitalocean,
            zone: region.clone(),
            ipv4: droplet
                .get("networks")
                .and_then(|n| n.get("v4"))
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter().find(|e| {
                        e.get("type").and_then(|t| t.as_str()) == Some("public")
                    })
                })
                .and_then(|e| e.get("ip_address"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            tenant_did: req.tenant_did.clone(),
            node_kind: req.node_kind,
            created_at,
            status,
        };
        self.state.write().insert(
            id.clone(),
            LocalRecord {
                instance: info.clone(),
                identity_public_key_b64: identity.public_b64(),
            },
        );
        persist_state(&self.state.read());

        Ok(ProvisionResponse {
            instance: info,
            dry_run: false,
            note: format!(
                "digitalocean: droplet {id} created in {region}, node identity public key {}",
                identity.public_b64()
            ),
        })
    }

    async fn list(&self, tenant_did: &str) -> Result<Vec<InstanceInfo>, ProviderError> {
        if self.is_live() {
            let tag = tenant_tag_for(tenant_did);
            let mut out = Vec::new();
            let mut page: u32 = 1;
            loop {
                let path = format!(
                    "/v2/droplets?tag_name={tag}&per_page=200&page={page}"
                );
                let res = self.call(reqwest::Method::GET, &path, None).await?;
                let droplets = res
                    .get("droplets")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if droplets.is_empty() {
                    break;
                }
                for d in &droplets {
                    if let Some(info) = droplet_to_info(d, tenant_did) {
                        if let Some(rec) = self.state.write().get_mut(&info.id) {
                            rec.instance = info.clone();
                        }
                        out.push(info);
                    }
                }
                let more = res
                    .get("links")
                    .and_then(|v| v.get("pages"))
                    .and_then(|v| v.get("next"))
                    .is_some();
                if !more || page >= 20 {
                    break;
                }
                page += 1;
            }
            persist_state(&self.state.read());
            return Ok(out);
        }
        Ok(self
            .state
            .read()
            .values()
            .filter(|r| r.instance.tenant_did == tenant_did)
            .map(|r| r.instance.clone())
            .collect())
    }

    async fn stop(
        &self,
        tenant_did: &str,
        instance_id: &str,
    ) -> Result<(), ProviderError> {
        require_tenant_owns(&self.state.read(), tenant_did, instance_id)?;
        if self.is_live() {
            self.call(
                reqwest::Method::POST,
                &format!("/v2/droplets/{instance_id}/actions"),
                Some(json!({ "type": "shutdown" })),
            )
            .await?;
        }
        if let Some(rec) = self.state.write().get_mut(instance_id) {
            rec.instance.status = "off".to_string();
        }
        persist_state(&self.state.read());
        Ok(())
    }

    async fn destroy(
        &self,
        tenant_did: &str,
        instance_id: &str,
    ) -> Result<(), ProviderError> {
        require_tenant_owns(&self.state.read(), tenant_did, instance_id)?;
        if self.is_live() {
            self.call(
                reqwest::Method::DELETE,
                &format!("/v2/droplets/{instance_id}"),
                None,
            )
            .await?;
        }
        self.state.write().remove(instance_id);
        persist_state(&self.state.read());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers - DO-specific mapping and state persistence.
// ---------------------------------------------------------------------------

fn droplet_to_info(d: &Value, tenant_did: &str) -> Option<InstanceInfo> {
    let id = d.get("id").and_then(|v| v.as_u64()).map(|v| v.to_string())?;
    let name = d
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let region = d
        .get("region")
        .and_then(|v| v.get("slug"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let status = d
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let created_at = d
        .get("created_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ipv4 = d
        .get("networks")
        .and_then(|n| n.get("v4"))
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("public"))
        })
        .and_then(|e| e.get("ip_address"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let kind = d
        .get("tags")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .find(|s| s.starts_with("kind:"))
        })
        .and_then(|s| s.strip_prefix("kind:"))
        .and_then(NodeKind::from_tag)
        .unwrap_or(NodeKind::Rpc);
    Some(InstanceInfo {
        id,
        hostname: name,
        provider: Provider::Digitalocean,
        zone: region,
        ipv4,
        tenant_did: tenant_did.to_string(),
        node_kind: kind,
        created_at,
        status,
    })
}

fn require_tenant_owns(
    state: &BTreeMap<String, LocalRecord>,
    tenant_did: &str,
    instance_id: &str,
) -> Result<(), ProviderError> {
    match state.get(instance_id) {
        Some(rec) if rec.instance.tenant_did == tenant_did => Ok(()),
        Some(_) => Err(ProviderError::Invalid(format!(
            "tenant {tenant_did} does not own instance {instance_id}"
        ))),
        None => Ok(()),
    }
}

fn load_state() -> BTreeMap<String, LocalRecord> {
    let path = state_path();
    if !path.exists() {
        return BTreeMap::new();
    }
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => BTreeMap::new(),
    }
}

fn persist_state(state: &BTreeMap<String, LocalRecord>) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            tracing::warn!("digitalocean state dir {parent:?} not writable: {e}");
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = match serde_json::to_vec_pretty(state) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("digitalocean state serialise failed: {e}");
            return;
        }
    };
    if let Err(e) = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(())
    })() {
        tracing::warn!("digitalocean state persist failed: {e}");
    }
}

impl NodeKind {
    /// Parse a `kind:*` tag value back into the enum. This is
    /// duplicated in the exoscale adapter (labels use the same
    /// convention), but consolidating it here as a helper on the enum
    /// keeps the tag <-> kind mapping in one place.
    fn from_tag(s: &str) -> Option<Self> {
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

    #[test]
    fn droplet_to_info_maps_do_response() {
        let d = json!({
            "id": 12345678u64,
            "name": "rope-rpc-abc-fra1",
            "region": {"slug": "fra1"},
            "status": "active",
            "created_at": "2026-08-30T20:00:00Z",
            "tags": ["rope-deployer", "tenant:did-dwp-0xabc", "kind:rpc"],
            "networks": {
                "v4": [
                    {"ip_address": "10.0.0.1", "type": "private"},
                    {"ip_address": "203.0.113.42", "type": "public"}
                ]
            }
        });
        let info = droplet_to_info(&d, "did:dwp:0xabc").expect("map ok");
        assert_eq!(info.id, "12345678");
        assert_eq!(info.zone, "fra1");
        assert_eq!(info.status, "active");
        assert_eq!(info.ipv4.as_deref(), Some("203.0.113.42"));
        assert_eq!(info.node_kind, NodeKind::Rpc);
    }

    #[tokio::test]
    async fn provision_without_token_is_dry_run_and_persists_locally() {
        // Isolate the state file to a temp path so the test doesn't
        // touch /var/lib/rope-deployer.
        let tmp = std::env::temp_dir().join(format!(
            "rope-deployer-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::env::set_var("DEPLOYER_STATE_DIR", &tmp);
        std::env::remove_var("DIGITALOCEAN_TOKEN");
        std::env::remove_var("DIGITALOCEAN_API_KEY");
        std::env::remove_var("DIGITALOCEAN_API");

        let p = DigitalOceanProvider::from_env();
        assert!(!p.is_live(), "provider should be offline without a token");
        let req = ProvisionRequest {
            tenant_did: "did:dwp:0xowner".into(),
            tenant_onchainid: "0xowner".into(),
            project_name: "T".into(),
            provider: Provider::Digitalocean,
            zone: "fra1".into(),
            instance_size: "s-1vcpu-1gb".into(),
            node_kind: NodeKind::Rpc,
            ssh_pubkey: String::new(),
            labels: BTreeMap::new(),
        };
        let resp = p.provision(&req).await.expect("provision ok");
        assert!(resp.dry_run, "expected dry-run without a token");
        assert!(resp.instance.id.starts_with("dryrun-"));
        let listed = p.list("did:dwp:0xowner").await.expect("list ok");
        assert_eq!(listed.len(), 1);

        // Round-trip: a new provider instance should re-load state.
        drop(p);
        let p2 = DigitalOceanProvider::from_env();
        let listed2 = p2.list("did:dwp:0xowner").await.expect("list ok");
        assert_eq!(listed2.len(), 1);

        // Cleanup.
        let _ = fs::remove_dir_all(&tmp);
        std::env::remove_var("DEPLOYER_STATE_DIR");
    }
}
