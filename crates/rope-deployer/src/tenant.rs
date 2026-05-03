//! Tenant metadata and per-tenant quotas.
//!
//! For the MVP, tenant state is held in memory. A future revision will
//! persist this to the same Postgres / sled store the rest of `rope-node`
//! uses, and fetch ONCHAINID claims (KYC, AML, FATF country codes) before
//! provisioning anything.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Quota enforced per tenant DID across all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantQuota {
    pub max_instances: u32,
    pub max_vcpu_total: u32,
    pub max_ram_gb_total: u32,
    pub max_disk_gb_total: u32,
}

impl Default for TenantQuota {
    fn default() -> Self {
        // Conservative defaults until billing / KYC is wired in.
        Self {
            max_instances: 3,
            max_vcpu_total: 8,
            max_ram_gb_total: 16,
            max_disk_gb_total: 200,
        }
    }
}

/// Per-tenant record kept in memory.
#[derive(Debug, Clone, Default)]
pub struct TenantRecord {
    pub did: String,
    pub onchainid: String,
    pub project_name: String,
    pub quota: TenantQuota,
    /// Provider → instance ids currently provisioned.
    pub instances: BTreeMap<String, Vec<String>>,
}
