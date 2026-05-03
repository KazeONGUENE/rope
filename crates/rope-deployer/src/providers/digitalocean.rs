//! DigitalOcean adapter — Phase E placeholder.
//!
//! Mirrors the Exoscale adapter shape so the CLI flow already supports
//! `--provider digitalocean` end-to-end. Until live calls are
//! implemented, behaves like a dry-run provider regardless of credential
//! state. The shape of the eventual live path will use the v2 Droplets
//! API: <https://docs.digitalocean.com/reference/api/api-reference/>.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use super::{CloudProvider, ProviderError};
use crate::types::{InstanceInfo, ProvisionRequest, ProvisionResponse, Provider};

pub struct DigitalOceanProvider {
    api_token: Option<String>,
    default_region: String,
    state: Arc<RwLock<BTreeMap<String, InstanceInfo>>>,
}

impl DigitalOceanProvider {
    pub fn from_env() -> Self {
        Self {
            api_token: std::env::var("DIGITALOCEAN_TOKEN").ok(),
            default_region: std::env::var("DIGITALOCEAN_DEFAULT_REGION")
                .unwrap_or_else(|_| "fra1".to_string()),
            state: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    fn resolve_region(&self, req_zone: &str) -> String {
        if req_zone.is_empty() {
            self.default_region.clone()
        } else {
            req_zone.to_string()
        }
    }
}

#[async_trait]
impl CloudProvider for DigitalOceanProvider {
    fn name(&self) -> Provider {
        Provider::Digitalocean
    }

    fn is_live(&self) -> bool {
        // Phase E: even with a token, no live calls are made yet.
        false
    }

    async fn provision(
        &self,
        req: &ProvisionRequest,
    ) -> Result<ProvisionResponse, ProviderError> {
        let region = self.resolve_region(&req.zone);
        let id = uuid::Uuid::new_v4().to_string();
        let info = InstanceInfo {
            id: id.clone(),
            hostname: format!(
                "rope-{}-{}-{}",
                req.node_kind.as_str(),
                &id[..8],
                region
            ),
            provider: Provider::Digitalocean,
            zone: region,
            ipv4: None,
            tenant_did: req.tenant_did.clone(),
            node_kind: req.node_kind,
            created_at: chrono::Utc::now().to_rfc3339(),
            status: "dry-run".to_string(),
        };
        self.state.write().insert(id.clone(), info.clone());

        Ok(ProvisionResponse {
            instance: info,
            dry_run: true,
            note: if self.api_token.is_some() {
                "digitalocean: token detected but live provisioning not yet \
                 implemented (Phase E)"
                    .to_string()
            } else {
                "digitalocean: dry-run (DIGITALOCEAN_TOKEN not set)".to_string()
            },
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
        _tenant_did: &str,
        instance_id: &str,
    ) -> Result<(), ProviderError> {
        let mut g = self.state.write();
        if let Some(info) = g.get_mut(instance_id) {
            info.status = "stopped".to_string();
            Ok(())
        } else {
            Err(ProviderError::Invalid(format!(
                "no such instance: {instance_id}"
            )))
        }
    }

    async fn destroy(
        &self,
        _tenant_did: &str,
        instance_id: &str,
    ) -> Result<(), ProviderError> {
        if self.state.write().remove(instance_id).is_some() {
            Ok(())
        } else {
            Err(ProviderError::Invalid(format!(
                "no such instance: {instance_id}"
            )))
        }
    }
}
