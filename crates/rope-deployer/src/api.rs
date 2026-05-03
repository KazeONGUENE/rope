//! HTTP API for `rope-deployer`.
//!
//! Exposed routes:
//!
//! | Method | Path                              | Purpose                                       |
//! |--------|-----------------------------------|-----------------------------------------------|
//! | GET    | `/health`                         | Liveness probe                                |
//! | GET    | `/providers`                      | List configured providers + their live state  |
//! | POST   | `/v1/instances`                   | Provision a new instance                      |
//! | GET    | `/v1/instances/:tenant_did`       | List instances owned by a tenant              |
//! | POST   | `/v1/instances/:tenant_did/:id/stop`    | Stop an instance                        |
//! | DELETE | `/v1/instances/:tenant_did/:id`         | Destroy an instance                     |
//!
//! The API surface is deliberately tiny — a follow-up PR will plug it
//! into `axum` + auth middleware. Keeping the handler bodies pure
//! functions makes them trivially unit-testable today.

use std::sync::Arc;

use crate::providers::{ProviderError, ProviderRegistry};
use crate::types::{InstanceInfo, ProvisionRequest, ProvisionResponse};

/// Shared state injected into every handler.
#[derive(Clone)]
pub struct AppState {
    pub providers: ProviderRegistry,
}

impl AppState {
    pub fn new(providers: ProviderRegistry) -> Self {
        Self { providers }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("provider not found")]
    ProviderNotFound,
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// `GET /health`
pub fn health() -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "service": "rope-deployer",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// `GET /providers`
pub fn providers(state: &AppState) -> serde_json::Value {
    let snapshot: Vec<_> = state
        .providers
        .snapshot()
        .into_iter()
        .map(|(name, live)| {
            serde_json::json!({
                "name": name.as_str(),
                "live": live,
            })
        })
        .collect();
    serde_json::json!({ "providers": snapshot })
}

/// `POST /v1/instances`
pub async fn provision(
    state: Arc<AppState>,
    req: ProvisionRequest,
) -> Result<ProvisionResponse, ApiError> {
    let provider = state
        .providers
        .get(req.provider)
        .ok_or(ApiError::ProviderNotFound)?;
    Ok(provider.provision(&req).await?)
}

/// `GET /v1/instances/:tenant_did`
pub async fn list_instances(
    state: Arc<AppState>,
    tenant_did: &str,
) -> Result<Vec<InstanceInfo>, ApiError> {
    let mut all = Vec::new();
    for (name, _live) in state.providers.snapshot() {
        if let Some(p) = state.providers.get(name) {
            all.extend(p.list(tenant_did).await?);
        }
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::local::LocalProvider;
    use crate::types::{NodeKind, Provider};

    fn test_state() -> Arc<AppState> {
        let registry = ProviderRegistry::new();
        registry.register(Arc::new(LocalProvider::new()));
        Arc::new(AppState::new(registry))
    }

    #[test]
    fn health_payload() {
        let v = health();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["service"], "rope-deployer");
    }

    #[test]
    fn providers_payload_lists_local() {
        let state = test_state();
        let v = providers(&state);
        let arr = v["providers"].as_array().unwrap();
        assert!(arr.iter().any(|p| p["name"] == "local"));
    }

    #[tokio::test]
    async fn provision_and_list_local() {
        let state = test_state();
        let req = ProvisionRequest {
            tenant_did: "did:dwp:test".to_string(),
            tenant_onchainid: "0x0000000000000000000000000000000000000000".to_string(),
            project_name: "alpha".to_string(),
            provider: Provider::Local,
            zone: "ch-gva-2".to_string(),
            instance_size: "standard.medium".to_string(),
            node_kind: NodeKind::Witness,
            ssh_pubkey: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5...".to_string(),
            labels: Default::default(),
        };
        let resp = provision(state.clone(), req).await.unwrap();
        assert!(!resp.dry_run);
        assert_eq!(resp.instance.tenant_did, "did:dwp:test");
        assert_eq!(resp.instance.provider, Provider::Local);

        let listed = list_instances(state, "did:dwp:test").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, resp.instance.id);
    }
}
