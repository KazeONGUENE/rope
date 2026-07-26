//! Node provisioning — spec v1.0 §5 / v2.0 §4: the deploy step turns the
//! frozen `NodePlan` into actual sovereign nodes via `rope-deployer`.
//!
//! Provider selection:
//!
//! * `EDC_CLOUD_PROVIDER=exoscale|digitalocean|local` pins the target.
//! * Unset: the first provider with live credentials wins
//!   (Exoscale, then DigitalOcean), falling back to the in-process
//!   `local` provider — which still walks the full provisioning path and
//!   records every node, so an owner running the console before wiring a
//!   cloud account gets `status = "dry_run"` nodes they can re-provision
//!   later instead of silent nothing.
//!
//! Role → node-kind mapping: nodes carrying `federation_validator` deploy
//! as Quipu Canon witnesses; everything else (ingestion gateway, storage
//! ledger, AI-agent host) deploys as a full RPC node, because those roles
//! all need the rope-node write path locally.
//!
//! Simulation projects never provision — the sandbox runs on synthetic
//! streams inside this process (spec v1.0 §6.3).

use std::sync::Arc;

use rope_deployer::providers::{digitalocean::DigitalOceanProvider, exoscale::ExoscaleProvider, local::LocalProvider};
use rope_deployer::{NodeKind, Provider, ProviderRegistry, ProvisionRequest};

use crate::types::{now_ts, NodePlan, Project, ProvisionedNode};

/// Build the provider registry from the environment (idempotent, cheap).
pub fn provider_registry() -> ProviderRegistry {
    let registry = ProviderRegistry::new();
    registry.register(Arc::new(LocalProvider::new()));
    registry.register(Arc::new(ExoscaleProvider::from_env()));
    registry.register(Arc::new(DigitalOceanProvider::from_env()));
    registry
}

/// Resolve the target provider per the policy above.
pub fn resolve_provider(registry: &ProviderRegistry) -> Provider {
    if let Ok(pinned) = std::env::var("EDC_CLOUD_PROVIDER") {
        match pinned.to_lowercase().as_str() {
            "exoscale" => return Provider::Exoscale,
            "digitalocean" => return Provider::Digitalocean,
            "local" => return Provider::Local,
            other => {
                tracing::warn!("EDC_CLOUD_PROVIDER={other} not recognized; auto-selecting");
            }
        }
    }
    for candidate in [Provider::Exoscale, Provider::Digitalocean] {
        if let Some(p) = registry.get(candidate) {
            if p.is_live() {
                return candidate;
            }
        }
    }
    Provider::Local
}

/// Instance size per tier — conservative defaults that an operator can
/// override per provider via env.
fn instance_size(plan: &NodePlan, provider: Provider) -> String {
    let var = match provider {
        Provider::Exoscale => "EDC_EXOSCALE_SIZE",
        Provider::Digitalocean => "EDC_DO_SIZE",
        Provider::Local => "EDC_LOCAL_SIZE",
    };
    if let Ok(v) = std::env::var(var) {
        if !v.is_empty() {
            return v;
        }
    }
    use crate::types::ScaleTier::*;
    match (provider, plan.tier) {
        (Provider::Digitalocean, Pilot | Standard) => "s-2vcpu-4gb".into(),
        (Provider::Digitalocean, Growth) => "s-4vcpu-8gb".into(),
        (Provider::Digitalocean, _) => "s-8vcpu-16gb".into(),
        (_, Pilot | Standard) => "standard.medium".into(),
        (_, Growth) => "standard.large".into(),
        (_, _) => "standard.extra-large".into(),
    }
}

fn zone_for(provider: Provider) -> String {
    let var = match provider {
        Provider::Exoscale => "EDC_EXOSCALE_ZONE",
        Provider::Digitalocean => "EDC_DO_ZONE",
        Provider::Local => "EDC_LOCAL_ZONE",
    };
    std::env::var(var).unwrap_or_else(|_| match provider {
        Provider::Exoscale => "ch-gva-2".into(),
        Provider::Digitalocean => "fra1".into(),
        Provider::Local => "local".into(),
    })
}

fn node_kind_for(roles: &[String]) -> NodeKind {
    if roles.iter().any(|r| r == "federation_validator") {
        NodeKind::Witness
    } else {
        NodeKind::Rpc
    }
}

/// Provision every node in the plan. Returns one `ProvisionedNode` per
/// planned node — failures surface as `status = "failed: …"` entries so
/// the console shows exactly which slots need a retry, never a silent
/// partial deploy.
pub async fn provision_nodes(project: &Project, plan: &NodePlan) -> Vec<ProvisionedNode> {
    if project.simulation {
        return Vec::new();
    }

    let registry = provider_registry();
    let provider = resolve_provider(&registry);
    let adapter = registry
        .get(provider)
        .expect("resolved provider is always registered");
    let zone = zone_for(provider);
    let size = instance_size(plan, provider);
    let ssh_pubkey = std::env::var("EDC_SSH_PUBKEY").unwrap_or_default();

    let (tenant_did, tenant_onchainid) = project
        .identity
        .as_ref()
        .map(|i| (i.did.clone(), i.onchainid.clone()))
        .unwrap_or_default();

    let mut out = Vec::with_capacity(plan.role_layout.len());
    for (i, roles) in plan.role_layout.iter().enumerate() {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert("edc_project".to_string(), project.id.clone());
        labels.insert("edc_node_index".to_string(), i.to_string());
        labels.insert("edc_roles".to_string(), roles.join(","));

        let req = ProvisionRequest {
            tenant_did: tenant_did.clone(),
            tenant_onchainid: tenant_onchainid.clone(),
            project_name: project.name(),
            provider,
            zone: zone.clone(),
            instance_size: size.clone(),
            node_kind: node_kind_for(roles),
            ssh_pubkey: ssh_pubkey.clone(),
            labels,
        };

        match adapter.provision(&req).await {
            Ok(resp) => {
                out.push(ProvisionedNode {
                    instance_id: resp.instance.id,
                    provider: provider.as_str().to_string(),
                    zone: resp.instance.zone,
                    hostname: resp.instance.hostname,
                    ipv4: resp.instance.ipv4.unwrap_or_default(),
                    roles: roles.clone(),
                    status: if resp.dry_run {
                        "dry_run".to_string()
                    } else {
                        resp.instance.status
                    },
                    created_at: now_ts(),
                });
            }
            Err(e) => {
                tracing::error!(
                    "provisioning node {i} for project {} failed: {e}",
                    project.id
                );
                out.push(ProvisionedNode {
                    instance_id: format!("pending:{}", uuid::Uuid::new_v4().simple()),
                    provider: provider.as_str().to_string(),
                    zone: zone.clone(),
                    hostname: format!("edc-{}-{i}", &project.id),
                    ipv4: String::new(),
                    roles: roles.clone(),
                    status: format!("failed: {e}"),
                    created_at: now_ts(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NodePlan;

    #[tokio::test]
    async fn local_provider_provisions_full_plan() {
        // Force the deterministic in-process provider for the test.
        std::env::set_var("EDC_CLOUD_PROVIDER", "local");
        let mut project = Project::new("Provision test", "0xowner");
        crate::simulation::apply_template(&mut project, "den_haag_escalators");
        let plan = NodePlan::recommend(
            project.inventory.assets.len(),
            project.inventory.events_per_hour(),
            1,
            false,
        );
        let nodes = provision_nodes(&project, &plan).await;
        assert_eq!(nodes.len(), plan.node_count as usize);
        for n in &nodes {
            assert!(!n.instance_id.is_empty());
            assert!(!n.roles.is_empty());
            assert!(!n.status.starts_with("failed"));
        }
        std::env::remove_var("EDC_CLOUD_PROVIDER");
    }

    #[tokio::test]
    async fn simulation_projects_never_provision() {
        let mut project = Project::new("Sandbox", "0xowner");
        project.simulation = true;
        crate::simulation::apply_template(&mut project, "agri_estate");
        let plan = NodePlan::recommend(10, 100.0, 1, false);
        let nodes = provision_nodes(&project, &plan).await;
        assert!(nodes.is_empty());
    }
}
