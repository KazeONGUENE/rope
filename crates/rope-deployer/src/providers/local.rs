//! In-memory `local` provider used for tests, CI, and offline demos.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use super::{CloudProvider, ProviderError};
use crate::types::{InstanceInfo, ProvisionRequest, ProvisionResponse, Provider};

#[derive(Default)]
pub struct LocalProvider {
    state: Arc<RwLock<BTreeMap<String, InstanceInfo>>>,
}

impl LocalProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CloudProvider for LocalProvider {
    fn name(&self) -> Provider {
        Provider::Local
    }

    fn is_live(&self) -> bool {
        true
    }

    async fn provision(
        &self,
        req: &ProvisionRequest,
    ) -> Result<ProvisionResponse, ProviderError> {
        let id = uuid::Uuid::new_v4().to_string();
        let info = InstanceInfo {
            id: id.clone(),
            hostname: format!("rope-{}-{}", req.node_kind.as_str(), &id[..8]),
            provider: Provider::Local,
            zone: req.zone.clone(),
            ipv4: Some("127.0.0.1".to_string()),
            tenant_did: req.tenant_did.clone(),
            node_kind: req.node_kind,
            created_at: chrono::Utc::now().to_rfc3339(),
            status: "running".to_string(),
        };
        self.state.write().insert(id.clone(), info.clone());
        Ok(ProvisionResponse {
            instance: info,
            dry_run: false,
            note: "local provider — no real cloud resources allocated".to_string(),
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
