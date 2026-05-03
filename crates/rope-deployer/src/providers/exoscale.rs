//! Exoscale Compute API adapter.
//!
//! API reference: <https://openapi-v2.exoscale.com/>
//!
//! Auth model: every request to `https://api-{zone}.exoscale.com/v2/...` is
//! signed by an HMAC over the request line + headers + body, using an
//! Exoscale IAM key/secret pair issued to the Datachain Foundation
//! account. Per-tenant isolation is achieved by minting one IAM key per
//! tenant DID with a CEL policy that scopes it to the tenant's private
//! network and resource tags. See `deploy/EXOSCALE_AS_A_SERVICE.md`.
//!
//! This MVP intentionally does **not** sign live requests. When
//! `EXOSCALE_API_KEY` is missing it returns a deterministic dry-run
//! response so the CLI flow and HTTP API can be exercised end-to-end
//! offline. Real signing is a follow-up (`exoscale-rs` crate or hand-rolled
//! HMAC in `client.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use super::{CloudProvider, ProviderError};
use crate::types::{InstanceInfo, ProvisionRequest, ProvisionResponse, Provider};

/// Exoscale provider state.
pub struct ExoscaleProvider {
    api_key: Option<String>,
    api_secret: Option<String>,
    /// Default zone for tenants that don't pick one explicitly.
    default_zone: String,
    /// In-memory mirror of provisioned instances. Populated either from
    /// dry-run calls or (in a future revision) from `GET /v2/instance`.
    state: Arc<RwLock<BTreeMap<String, InstanceInfo>>>,
}

impl ExoscaleProvider {
    pub fn from_env() -> Self {
        Self {
            api_key: std::env::var("EXOSCALE_API_KEY").ok(),
            api_secret: std::env::var("EXOSCALE_API_SECRET").ok(),
            default_zone: std::env::var("EXOSCALE_DEFAULT_ZONE")
                .unwrap_or_else(|_| "ch-gva-2".to_string()),
            state: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    fn has_creds(&self) -> bool {
        self.api_key.is_some() && self.api_secret.is_some()
    }

    /// Resolve the zone for a request, falling back to the configured default.
    fn resolve_zone(&self, req_zone: &str) -> String {
        if req_zone.is_empty() {
            self.default_zone.clone()
        } else {
            req_zone.to_string()
        }
    }
}

#[async_trait]
impl CloudProvider for ExoscaleProvider {
    fn name(&self) -> Provider {
        Provider::Exoscale
    }

    fn is_live(&self) -> bool {
        self.has_creds()
    }

    async fn provision(
        &self,
        req: &ProvisionRequest,
    ) -> Result<ProvisionResponse, ProviderError> {
        let zone = self.resolve_zone(&req.zone);

        // Generate the deterministic identifiers we'd send to the
        // Compute API. In the live path, these go in the request body
        // alongside `template`, `disk-size`, `ssh-key`, `user-data`.
        let id = uuid::Uuid::new_v4().to_string();
        let hostname = format!(
            "rope-{}-{}-{}",
            req.node_kind.as_str(),
            &id[..8],
            zone.replace('-', "")
        );

        let info = InstanceInfo {
            id: id.clone(),
            hostname,
            provider: Provider::Exoscale,
            zone: zone.clone(),
            ipv4: None, // assigned asynchronously by Exoscale
            tenant_did: req.tenant_did.clone(),
            node_kind: req.node_kind,
            created_at: chrono::Utc::now().to_rfc3339(),
            status: if self.has_creds() {
                "provisioning".to_string()
            } else {
                "dry-run".to_string()
            },
        };

        self.state.write().insert(id.clone(), info.clone());

        if !self.has_creds() {
            return Ok(ProvisionResponse {
                instance: info,
                dry_run: true,
                note: "EXOSCALE_API_KEY/SECRET not set — dry-run only. \
                       Set them on the rope-deployer host to issue real \
                       Compute API calls."
                    .to_string(),
            });
        }

        // ---- Live path placeholder ----
        // The real implementation will:
        //   1. Mint a per-tenant IAM key with a CEL policy scoped to the
        //      tenant private network + tags (see EXOSCALE_AS_A_SERVICE.md).
        //   2. Build a `cloud-init` user-data blob that bootstraps
        //      reth + rope-node from the baked template.
        //   3. POST /v2/instance with the template id, ssh-key, user-data,
        //      and labels = { tenant_did, project_name, node_kind }.
        //   4. Poll the returned operation id until the instance is
        //      `running`, then update `info.ipv4` and `info.status`.
        //
        // Until that lands, return the in-memory record so the API
        // contract stays stable.
        Ok(ProvisionResponse {
            instance: info,
            dry_run: false,
            note: "exoscale: provisioning enqueued (live API path not yet implemented; \
                   see deploy/EXOSCALE_AS_A_SERVICE.md)"
                .to_string(),
        })
    }

    async fn list(&self, tenant_did: &str) -> Result<Vec<InstanceInfo>, ProviderError> {
        Ok(self
            .state
            .read()
            .values()
            .filter(|i| i.tenant_did == tenant_did)
            .cloned()
            .collect())
    }

    async fn stop(
        &self,
        tenant_did: &str,
        instance_id: &str,
    ) -> Result<(), ProviderError> {
        let mut g = self.state.write();
        match g.get_mut(instance_id) {
            Some(info) if info.tenant_did == tenant_did => {
                info.status = "stopped".to_string();
                Ok(())
            }
            Some(_) => Err(ProviderError::Invalid(
                "instance does not belong to tenant".to_string(),
            )),
            None => Err(ProviderError::Invalid(format!(
                "no such instance: {instance_id}"
            ))),
        }
    }

    async fn destroy(
        &self,
        tenant_did: &str,
        instance_id: &str,
    ) -> Result<(), ProviderError> {
        let mut g = self.state.write();
        match g.get(instance_id) {
            Some(info) if info.tenant_did == tenant_did => {
                g.remove(instance_id);
                Ok(())
            }
            Some(_) => Err(ProviderError::Invalid(
                "instance does not belong to tenant".to_string(),
            )),
            None => Err(ProviderError::Invalid(format!(
                "no such instance: {instance_id}"
            ))),
        }
    }
}
