//! Wire types shared between the HTTP API, the CLI, and the provider adapters.

use serde::{Deserialize, Serialize};

/// Cloud provider that a tenant can target.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// In-process dry-run provider, used for tests and CI.
    Local,
    /// Exoscale (Switzerland-based, EU sovereign cloud).
    Exoscale,
    /// DigitalOcean.
    Digitalocean,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Local => "local",
            Provider::Exoscale => "exoscale",
            Provider::Digitalocean => "digitalocean",
        }
    }
}

/// What flavor of Rope node the tenant wants to deploy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    /// Lightweight Quipu Canon knot witness (no Reth datadir).
    Witness,
    /// Full RPC node with Reth + rope-node (master-node candidate).
    Rpc,
    /// Federation seed node (peers + relay).
    Seeder,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Witness => "witness",
            NodeKind::Rpc => "rpc",
            NodeKind::Seeder => "seeder",
        }
    }
}

/// Provisioning request submitted by a tenant via CLI or the
/// `Submit Your Project` modal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionRequest {
    /// Tenant Datawallet+ DID (`did:dwp:...`) — also the billing identity.
    pub tenant_did: String,
    /// Tenant ONCHAINID address (0x..) used for compliance checks.
    pub tenant_onchainid: String,
    /// Human-readable project / federation name.
    pub project_name: String,
    /// Cloud provider to target.
    pub provider: Provider,
    /// Provider-specific zone / region (e.g. "ch-gva-2", "fra1").
    pub zone: String,
    /// Provider instance size (e.g. "standard.medium", "s-2vcpu-4gb").
    pub instance_size: String,
    /// What kind of Rope node to launch.
    pub node_kind: NodeKind,
    /// SSH public key authorised on the new instance (OpenSSH format).
    pub ssh_pubkey: String,
    /// Optional extra metadata stored as tags / labels on the instance.
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
}

/// Provisioning response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionResponse {
    pub instance: InstanceInfo,
    /// True when the call did not actually hit the cloud API
    /// (no creds, or `--dry-run`).
    pub dry_run: bool,
    /// Human-readable diagnostic line useful for the CLI.
    pub note: String,
}

/// Lightweight description of a deployed instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceInfo {
    /// Cloud-provider instance id (uuid for Exoscale, integer for DO).
    pub id: String,
    /// DNS-friendly hostname.
    pub hostname: String,
    /// Provider name (mirrors `ProvisionRequest.provider`).
    pub provider: Provider,
    /// Provider zone (mirrors request).
    pub zone: String,
    /// Public IPv4 (may be empty until provisioning completes).
    pub ipv4: Option<String>,
    /// Tenant DID that owns this instance.
    pub tenant_did: String,
    /// What kind of Rope node runs on this instance.
    pub node_kind: NodeKind,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    /// Free-form status (`provisioning`, `running`, `stopped`, …).
    pub status: String,
}
