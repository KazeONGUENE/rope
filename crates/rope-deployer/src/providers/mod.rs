//! Cloud provider abstraction.
//!
//! Each provider (Exoscale, DigitalOcean, …) is implemented as a struct that
//! implements [`CloudProvider`] and is registered into the `ProviderRegistry`.
//! A `local` provider (no-op, in-memory) is always present and is what is
//! exercised in tests and CI.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use crate::types::{InstanceInfo, Provider, ProvisionRequest, ProvisionResponse};

pub mod digitalocean;
pub mod exoscale;
pub mod local;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider {0:?} not configured")]
    NotConfigured(Provider),

    #[error("upstream API error: {0}")]
    Upstream(String),

    #[error("invalid request: {0}")]
    Invalid(String),
}

/// Adapter trait implemented by every cloud provider.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    fn name(&self) -> Provider;

    /// Whether the adapter has credentials and may issue real API calls.
    fn is_live(&self) -> bool;

    async fn provision(&self, req: &ProvisionRequest) -> Result<ProvisionResponse, ProviderError>;

    async fn list(&self, tenant_did: &str) -> Result<Vec<InstanceInfo>, ProviderError>;

    async fn stop(&self, tenant_did: &str, instance_id: &str) -> Result<(), ProviderError>;

    async fn destroy(&self, tenant_did: &str, instance_id: &str) -> Result<(), ProviderError>;
}

/// Registry of all configured providers.
#[derive(Clone)]
pub struct ProviderRegistry {
    inner: Arc<RwLock<HashMap<Provider, Arc<dyn CloudProvider>>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn register(&self, provider: Arc<dyn CloudProvider>) {
        self.inner.write().insert(provider.name(), provider);
    }

    pub fn get(&self, name: Provider) -> Option<Arc<dyn CloudProvider>> {
        self.inner.read().get(&name).cloned()
    }

    /// Snapshot of the registry, used by the `GET /providers` endpoint.
    pub fn snapshot(&self) -> Vec<(Provider, bool)> {
        self.inner
            .read()
            .iter()
            .map(|(name, p)| (*name, p.is_live()))
            .collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}
