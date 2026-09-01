//! Core data model for the Ecosystem Deployment Console.
//!
//! Every structure here maps one-to-one onto a section of the EDC
//! specification v2.0 (`docs/ECOSYSTEM_DEPLOYMENT_CONSOLE_SPEC_V2.md`):
//!
//! | Type | Spec section |
//! |------|--------------|
//! | [`Project`] + wizard sub-structs | §3 (nine-step wizard) |
//! | [`AssetRecord`] / [`SensorRecord`] / [`MeshNode`] / [`ExternalSource`] / [`AiAgentConfig`] | v1.0 §4.5.1–4.5.5 |
//! | [`NodePlan`] / [`ScaleTier`] | §4 (node sizing) |
//! | [`MutabilityPolicy`] | v1.0 §8 |
//! | [`TelemetryReading`] / [`DiagnosisEvent`] / [`ApprovalEvent`] | live console facets |

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Unix timestamp seconds (UTC).
pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Derive a deterministic synthetic wallet address for a project string.
/// `0x` + first 20 bytes of `blake3("edc-project:" || project_id)`.
pub fn project_wallet(project_id: &str) -> String {
    let hash = blake3::hash(format!("edc-project:{project_id}").as_bytes());
    format!("0x{}", hex::encode(&hash.as_bytes()[..20]))
}

// ---------------------------------------------------------------------------
// Project archetypes and lifecycle
// ---------------------------------------------------------------------------

/// The two supported archetypes plus the hybrid mode (spec v1.0 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectArchetype {
    PredictiveMaintenance,
    EnvironmentalMonitoring,
    Hybrid,
}

/// Project lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    /// Wizard in progress - steps may be saved and resumed.
    Draft,
    /// Owner confirmed; node provisioning in flight.
    Deploying,
    /// On-chain string open, nodes provisioned, ingestion live.
    Live,
    /// Temporarily halted by the owner or governance body.
    Suspended,
    /// Permanently retired (Timelock-gated action).
    Decommissioned,
}

// ---------------------------------------------------------------------------
// Step 1 - Identity & Compliance (v1.0 §4.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityInfo {
    /// `individual` (KYC) or `organization` (KYB).
    pub kind: String,
    /// Datawallet+ DID (`did:dwp:...`) of the owner entity.
    pub did: String,
    /// ONCHAINID address carrying the KYC/KYB claim.
    pub onchainid: String,
    /// Legal name (person or organization).
    pub legal_name: String,
    /// Country of incorporation / residence (ISO 3166-1 alpha-2).
    pub country: String,
    /// Whether sanctions / PEP screening passed (recorded by the claim issuer).
    #[serde(default)]
    pub screening_passed: bool,
}

// ---------------------------------------------------------------------------
// Step 2 - Project definition (v1.0 §4.2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDefinition {
    pub name: String,
    pub archetype: ProjectArchetype,
    /// Sub-vertical tags from the IoT taxonomy (Cities, Environment, Energy, …).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Country (ISO 3166-1 alpha-2).
    pub country: String,
    /// Optional region / city label.
    #[serde(default)]
    pub region: String,
    /// GPS centre point `[lat, lon]` (or centroid of the boundary polygon).
    #[serde(default)]
    pub gps: Option<[f64; 2]>,
    /// Optional boundary polygon (list of `[lat, lon]` pairs) for area projects.
    #[serde(default)]
    pub boundary: Vec<[f64; 2]>,
    /// Narrative description and objectives.
    #[serde(default)]
    pub description: String,
    /// Target KPIs, free-form label → target.
    #[serde(default)]
    pub kpis: BTreeMap<String, String>,
    /// Expected asset count band, e.g. "50-500".
    #[serde(default)]
    pub expected_assets_band: String,
    /// Funding / ownership structure narrative.
    #[serde(default)]
    pub ownership_structure: String,
}

// ---------------------------------------------------------------------------
// Step 3 - Crypto asset decision (v1.0 §4.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoAssetKind {
    /// Project runs purely on FAT-denominated gas; no token created.
    None,
    /// DCR-20 utility token.
    Dcr20,
    /// ERC-3643 / T-REX security token.
    Erc3643,
    /// One DCNFT certificate per physical asset.
    Dcnft,
    /// One DCNFT per asset + fractional ERC-3643 pool (Tanastok model).
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoAssetConfig {
    pub kind: CryptoAssetKind,
    /// `fixed`, `elastic`, or `fractional`.
    #[serde(default)]
    pub supply_model: String,
    #[serde(default)]
    pub initial_supply: u64,
    #[serde(default)]
    pub max_supply: u64,
    #[serde(default)]
    pub decimals: u8,
    /// Timelock-gated mint authority description.
    #[serde(default)]
    pub mint_authority: String,
    /// Percentage of project revenue distributed to holders (0–100).
    #[serde(default)]
    pub revenue_share_pct: f64,
    /// `monthly`, `quarterly`, `on_sale`, …
    #[serde(default)]
    pub distribution_frequency: String,
    /// Voting weight per unit, free-form scope description.
    #[serde(default)]
    pub governance_rights: String,
    /// Redemption / buy-back terms, if any.
    #[serde(default)]
    pub redemption_terms: String,
    /// KYC gating, jurisdiction allowlists, lock-ups.
    #[serde(default)]
    pub transfer_restrictions: Vec<String>,
    /// `private_pool`, `dcswap`, or `hybrid`.
    #[serde(default)]
    pub distribution_channel: String,
}

impl Default for CryptoAssetConfig {
    fn default() -> Self {
        Self {
            kind: CryptoAssetKind::None,
            supply_model: String::new(),
            initial_supply: 0,
            max_supply: 0,
            decimals: 0,
            mint_authority: String::new(),
            revenue_share_pct: 0.0,
            distribution_frequency: String::new(),
            governance_rights: String::new(),
            redemption_terms: String::new(),
            transfer_restrictions: Vec::new(),
            distribution_channel: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Step 4 - Team & governance (v1.0 §4.4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Administrator,
    Operator,
    Auditor,
    AiAgent,
}

impl Role {
    /// Whether this role may perform project-mutating console actions.
    pub fn can_mutate(&self) -> bool {
        matches!(self, Role::Owner | Role::Administrator)
    }

    /// Whether this role may perform sensitive actions (grants touching
    /// regulators/public, erasure, decommission, mint) without a Timelock.
    /// Only the Owner short-circuits; Administrators go through the delay.
    pub fn is_owner(&self) -> bool {
        matches!(self, Role::Owner)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    /// EVM wallet address (0x…).
    pub wallet: String,
    /// Optional Datawallet+ DID.
    #[serde(default)]
    pub did: String,
    pub role: Role,
    /// Human label ("Field team lead", "ESG auditor", "MaintenanceAgent-1").
    #[serde(default)]
    pub label: String,
    /// For Operator / AiAgent roles: the asset ids this member is scoped to.
    /// Empty = all assets (only meaningful for Owner/Administrator/Auditor).
    #[serde(default)]
    pub scoped_assets: Vec<String>,
}

// ---------------------------------------------------------------------------
// Step 5 - Inventory (v1.0 §4.5.1–4.5.5)
// ---------------------------------------------------------------------------

/// 4.5.1 Physical / IoT asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub id: String,
    pub name: String,
    /// IoT taxonomy category (Cities, Environment, Energy, Agriculture, …).
    pub category: String,
    /// street light, bench, soil probe, HVAC unit, …
    #[serde(default)]
    pub sub_type: String,
    #[serde(default)]
    pub gps: Option<[f64; 2]>,
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub serial_number: String,
    /// ISO-8601 date.
    #[serde(default)]
    pub commissioning_date: String,
    /// Design life / warranty expiry, ISO-8601 date.
    #[serde(default)]
    pub warranty_expiry: String,
    /// municipality, private, cooperative, shared.
    #[serde(default)]
    pub ownership: String,
    /// Immutable | OwnerErasable | TimeBound | GDPRCompliant | ConditionalErasure
    #[serde(default = "default_mutability")]
    pub mutability_class: String,
    /// Datawallet address once provisioned (derived from project + asset id).
    #[serde(default)]
    pub wallet: String,
    /// Optional QR/NFC tag identifier linking the physical tag to this record.
    #[serde(default)]
    pub tag_id: String,
    /// Health score 0–100, updated by the ingestion/diagnosis pipeline.
    #[serde(default = "default_health")]
    pub health_score: f64,
    #[serde(default)]
    pub last_seen_at: i64,
}

fn default_mutability() -> String {
    "OwnerErasable".to_string()
}
fn default_health() -> f64 {
    100.0
}

/// 4.5.2 Sensor / telemetry channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorRecord {
    pub id: String,
    pub parent_asset_id: String,
    /// lux, power draw, vibration, temperature, pH, N/P/K, …
    pub parameter: String,
    pub unit: String,
    /// Human cadence label ("6min", "hourly", "daily", "event").
    #[serde(default)]
    pub cadence: String,
    /// Expected readings per hour (drives node sizing). 0 for event-driven.
    #[serde(default)]
    pub readings_per_hour: f64,
    /// Operating range [min, max].
    #[serde(default)]
    pub range: Option<[f64; 2]>,
    /// Optimal band [min, max] (the Plage/Optimum model).
    #[serde(default)]
    pub optimum: Option<[f64; 2]>,
    /// Warning band [min, max]; readings outside optimum but inside warning
    /// are flagged; outside warning is critical.
    #[serde(default)]
    pub warning: Option<[f64; 2]>,
    /// MQTT, CoAP, LoRaWAN, Modbus, OPC-UA, HTTP, …
    #[serde(default)]
    pub protocol: String,
    /// REST / GraphQL / gRPC endpoint metadata for pull-based sources.
    #[serde(default)]
    pub endpoint: String,
    /// `private`, `stakeholder:<name>`, or `open`.
    #[serde(default)]
    pub sharing_policy: String,
    /// `direct` (sensor signs its own appends) or `gateway`.
    #[serde(default)]
    pub write_path: String,
}

/// 4.5.3 Mesh / network infrastructure node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshNode {
    pub id: String,
    /// gateway, relay, coordinator, edge-compute.
    pub role: String,
    /// LoRaWAN, Zigbee, Thread, Wi-Fi mesh, private cellular.
    pub technology: String,
    #[serde(default)]
    pub firmware_version: String,
    /// mains, solar, battery.
    #[serde(default)]
    pub power_source: String,
    /// Ethernet, cellular, satellite.
    #[serde(default)]
    pub backhaul: String,
    #[serde(default)]
    pub management_endpoint: String,
    #[serde(default)]
    pub associated_assets: Vec<String>,
}

/// 4.5.4 External data source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalSource {
    pub id: String,
    pub name: String,
    /// satellite, weather, gis, utility, traffic, marine, seismic, open_data, iot_platform.
    pub source_type: String,
    #[serde(default)]
    pub provider: String,
    /// REST, SFTP, WMS/WFS, chainlink, manual.
    #[serde(default)]
    pub access_method: String,
    #[serde(default)]
    pub endpoint: String,
    /// api_key, oauth2, ip_allowlist, none.
    #[serde(default)]
    pub auth_method: String,
    /// realtime, hourly, daily, per_pass, on_demand.
    #[serde(default)]
    pub update_frequency: String,
    /// open, commercial, restricted.
    #[serde(default)]
    pub license: String,
    /// chainlink_attestation, provenance_signature, manual_notarization.
    #[serde(default)]
    pub verification_method: String,
}

/// 4.5.5 AI / analysis agent registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiAgentConfig {
    pub id: String,
    /// maintenance_diagnosis, environmental_anomaly, procurement, compliance_scoring.
    pub agent_type: String,
    /// in_house, claude, openai, alteros, third_party.
    #[serde(default)]
    pub provider: String,
    /// Asset/sensor ids the agent may read. Empty = whole project.
    #[serde(default)]
    pub input_scope: Vec<String>,
    /// Asset ids the agent may write recommendations to. Empty = whole project.
    #[serde(default)]
    pub output_scope: Vec<String>,
    /// Confidence below which a human must review (0.0–1.0).
    #[serde(default = "default_escalation")]
    pub escalation_threshold: f64,
}

fn default_escalation() -> f64 {
    0.8
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    #[serde(default)]
    pub assets: Vec<AssetRecord>,
    #[serde(default)]
    pub sensors: Vec<SensorRecord>,
    #[serde(default)]
    pub mesh_nodes: Vec<MeshNode>,
    #[serde(default)]
    pub external_sources: Vec<ExternalSource>,
    #[serde(default)]
    pub ai_agents: Vec<AiAgentConfig>,
}

impl Inventory {
    /// Aggregate expected event rate across all sensors, in readings/hour.
    pub fn events_per_hour(&self) -> f64 {
        self.sensors.iter().map(|s| s.readings_per_hour).sum()
    }
}

// ---------------------------------------------------------------------------
// Step 6 - Sovereign node sizing (spec v2.0 §4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleTier {
    Pilot,
    Standard,
    Growth,
    LargeScale,
    Sovereign,
}

impl ScaleTier {
    pub fn databox_tier(&self) -> &'static str {
        match self {
            ScaleTier::Pilot => "Personal",
            ScaleTier::Standard => "Professional",
            ScaleTier::Growth => "Business",
            ScaleTier::LargeScale => "Enterprise",
            ScaleTier::Sovereign => "Sovereign Edition",
        }
    }

    pub fn redundancy(&self) -> &'static str {
        match self {
            ScaleTier::Pilot => "Single node, IPFS snapshot backup",
            ScaleTier::Standard => "Active/passive pair",
            ScaleTier::Growth => "Active/active, two regions",
            ScaleTier::LargeScale => {
                "Active/active, multi-region, dedicated ingestion gateway"
            }
            ScaleTier::Sovereign => {
                "Full regional mesh, one cluster per jurisdiction, optional federation validator seat"
            }
        }
    }

    pub fn node_count(&self) -> u32 {
        match self {
            ScaleTier::Pilot => 1,
            ScaleTier::Standard => 2,
            ScaleTier::Growth => 3,
            ScaleTier::LargeScale => 6,
            ScaleTier::Sovereign => 8,
        }
    }
}

/// Node roles within a deployment (v1.0 §5).
pub const NODE_ROLES: [&str; 4] = [
    "ingestion_gateway",
    "storage_ledger",
    "ai_agent_host",
    "federation_validator",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePlan {
    pub tier: ScaleTier,
    pub node_count: u32,
    pub databox_tier: String,
    pub redundancy: String,
    /// Which of the four roles each node hosts; at Pilot/Standard a single
    /// Databox hosts several roles.
    pub role_layout: Vec<Vec<String>>,
    /// Inputs the recommendation was computed from, for the review screen.
    pub basis_assets: usize,
    pub basis_events_per_hour: f64,
    pub basis_jurisdictions: usize,
    /// Whether the owner explicitly wants a federation validator seat.
    pub wants_validator: bool,
}

impl NodePlan {
    /// The sizing function from spec v2.0 §4: tier = max over the three axes.
    pub fn recommend(
        asset_count: usize,
        events_per_hour: f64,
        jurisdictions: usize,
        wants_validator: bool,
    ) -> Self {
        let by_assets = match asset_count {
            0..=50 => ScaleTier::Pilot,
            51..=500 => ScaleTier::Standard,
            501..=5_000 => ScaleTier::Growth,
            5_001..=50_000 => ScaleTier::LargeScale,
            _ => ScaleTier::Sovereign,
        };
        let by_events = if events_per_hour <= 600.0 {
            ScaleTier::Pilot
        } else if events_per_hour <= 6_000.0 {
            ScaleTier::Standard
        } else if events_per_hour <= 60_000.0 {
            ScaleTier::Growth
        } else if events_per_hour <= 600_000.0 {
            ScaleTier::LargeScale
        } else {
            ScaleTier::Sovereign
        };
        let by_jurisdiction = if jurisdictions >= 2 {
            ScaleTier::Sovereign
        } else {
            ScaleTier::Pilot
        };

        let tier = by_assets.max(by_events).max(by_jurisdiction);
        let node_count = tier.node_count();

        // Lay the four roles out across the recommended node count.
        let mut role_layout: Vec<Vec<String>> = Vec::new();
        let mut roles: Vec<String> = NODE_ROLES[..3].iter().map(|s| s.to_string()).collect();
        if wants_validator {
            roles.push(NODE_ROLES[3].to_string());
        }
        for i in 0..node_count as usize {
            if node_count == 1 {
                role_layout.push(roles.clone());
            } else {
                // Round-robin the roles; every node keeps storage_ledger.
                let mut node_roles = vec!["storage_ledger".to_string()];
                let extra = &roles[i % roles.len()];
                if extra != "storage_ledger" && !node_roles.contains(extra) {
                    node_roles.push(extra.clone());
                }
                role_layout.push(node_roles);
            }
        }

        Self {
            tier,
            node_count,
            databox_tier: tier.databox_tier().to_string(),
            redundancy: tier.redundancy().to_string(),
            role_layout,
            basis_assets: asset_count,
            basis_events_per_hour: events_per_hour,
            basis_jurisdictions: jurisdictions,
            wants_validator,
        }
    }
}

/// A node the project actually provisioned (result of Step 8 deploy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionedNode {
    /// Cloud instance id (or `pending:<uuid>` when awaiting operator action).
    pub instance_id: String,
    pub provider: String,
    pub zone: String,
    pub hostname: String,
    #[serde(default)]
    pub ipv4: String,
    pub roles: Vec<String>,
    pub status: String,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// Step 7 - Data governance & mutability policy (v1.0 §8)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutabilityPolicy {
    /// Data type → mutability class. Defaults follow spec v1.0 §8.
    pub classes: BTreeMap<String, String>,
    /// Retention window (days) for TimeBound telemetry.
    pub telemetry_retention_days: u32,
}

impl Default for MutabilityPolicy {
    fn default() -> Self {
        let mut classes = BTreeMap::new();
        classes.insert("telemetry".to_string(), "TimeBound".to_string());
        classes.insert("diagnosis".to_string(), "OwnerErasable".to_string());
        classes.insert("compliance".to_string(), "Immutable".to_string());
        classes.insert("personal".to_string(), "GDPRCompliant".to_string());
        Self {
            classes,
            telemetry_retention_days: 365,
        }
    }
}

// ---------------------------------------------------------------------------
// The project aggregate
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub status: ProjectStatus,
    pub created_at: i64,
    pub updated_at: i64,
    /// Synthetic wallet backing the project's on-chain string.
    pub wallet: String,
    /// Sandbox / simulation project (spec v1.0 §6.3). Simulation projects
    /// skip KYB screening and cloud provisioning, run on synthetic
    /// telemetry, and never appear in the real dcscan.io directory.
    #[serde(default)]
    pub simulation: bool,

    #[serde(default)]
    pub identity: Option<IdentityInfo>,
    #[serde(default)]
    pub definition: Option<ProjectDefinition>,
    #[serde(default)]
    pub crypto: CryptoAssetConfig,
    #[serde(default)]
    pub team: Vec<TeamMember>,
    #[serde(default)]
    pub inventory: Inventory,
    #[serde(default)]
    pub node_plan: Option<NodePlan>,
    #[serde(default)]
    pub provisioned_nodes: Vec<ProvisionedNode>,
    #[serde(default)]
    pub mutability_policy: MutabilityPolicy,

    /// Public stakeholder-dashboard base URL (the project's own node).
    #[serde(default)]
    pub stakeholder_url: String,
    /// Knot hash of the public project card anchored on the registry wallet.
    #[serde(default)]
    pub registry_anchor: String,
    /// Knot hash of the project genesis anchor on its own string.
    #[serde(default)]
    pub genesis_anchor: String,

    /// Scheduled report cadence (spec v1.0 §6.4): `""` (off), `hourly`,
    /// `daily`, `weekly`, or `monthly`.
    #[serde(default)]
    pub report_schedule: String,
    /// When the last scheduled report was generated (unix seconds).
    #[serde(default)]
    pub last_report_at: i64,
}

/// One scheduled (or on-demand) statutory / investor report - spec v1.0
/// §6.4 "Scheduled report generation". The narrative is produced by the
/// deterministic narrator over the period's analytics dossier, so every
/// figure in it traces to a named method; the dossier itself is stored
/// alongside as grounding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRecord {
    pub id: String,
    pub project_id: String,
    /// `hourly` | `daily` | `weekly` | `monthly` | `on_demand`.
    pub cadence: String,
    /// Period covered [from, to] (unix seconds).
    pub period_start: i64,
    pub period_end: i64,
    pub generated_at: i64,
    pub readings_in_scope: usize,
    /// Deterministic plain-language narrative.
    pub narrative: String,
    /// The full analytics dossier (grounding) as JSON.
    pub dossier: serde_json::Value,
    /// Knot hash of the `ScheduledReport` anchor on the project string.
    #[serde(default)]
    pub anchor: String,
}

impl Project {
    pub fn new(name_hint: &str, owner_wallet: &str) -> Self {
        let uid = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("prj_{}", &uid[..12]);
        let now = now_ts();
        Self {
            id: id.clone(),
            status: ProjectStatus::Draft,
            created_at: now,
            updated_at: now,
            wallet: project_wallet(&id),
            simulation: false,
            identity: None,
            definition: Some(ProjectDefinition {
                name: name_hint.to_string(),
                archetype: ProjectArchetype::Hybrid,
                tags: Vec::new(),
                country: String::new(),
                region: String::new(),
                gps: None,
                boundary: Vec::new(),
                description: String::new(),
                kpis: BTreeMap::new(),
                expected_assets_band: String::new(),
                ownership_structure: String::new(),
            }),
            crypto: CryptoAssetConfig::default(),
            team: vec![TeamMember {
                wallet: owner_wallet.to_string(),
                did: String::new(),
                role: Role::Owner,
                label: "Project owner".to_string(),
                scoped_assets: Vec::new(),
            }],
            inventory: Inventory::default(),
            node_plan: None,
            provisioned_nodes: Vec::new(),
            mutability_policy: MutabilityPolicy::default(),
            stakeholder_url: String::new(),
            registry_anchor: String::new(),
            genesis_anchor: String::new(),
            report_schedule: String::new(),
            last_report_at: 0,
        }
    }

    pub fn name(&self) -> String {
        self.definition
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_default()
    }

    /// Role of a wallet on this project, if any.
    pub fn role_of(&self, wallet: &str) -> Option<Role> {
        let w = wallet.to_lowercase();
        self.team
            .iter()
            .find(|m| m.wallet.to_lowercase() == w)
            .map(|m| m.role)
    }

    /// The public card anchored on the registry wallet and served to dcscan.
    pub fn public_card(&self) -> serde_json::Value {
        let def = self.definition.as_ref();
        serde_json::json!({
            "id": self.id,
            "name": def.map(|d| d.name.clone()).unwrap_or_default(),
            "archetype": def.map(|d| d.archetype).unwrap_or(ProjectArchetype::Hybrid),
            "tags": def.map(|d| d.tags.clone()).unwrap_or_default(),
            "country": def.map(|d| d.country.clone()).unwrap_or_default(),
            "region": def.map(|d| d.region.clone()).unwrap_or_default(),
            "status": self.status,
            "simulation": self.simulation,
            "asset_count": self.inventory.assets.len(),
            "sensor_count": self.inventory.sensors.len(),
            "crypto_asset": self.crypto.kind,
            "wallet": self.wallet,
            "stakeholder_url": self.stakeholder_url,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

// ---------------------------------------------------------------------------
// Live console facet events
// ---------------------------------------------------------------------------

/// One telemetry reading, as ingested from the gateway or the HTTP bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryReading {
    pub project_id: String,
    pub asset_id: String,
    pub sensor_id: String,
    pub parameter: String,
    pub value: f64,
    pub unit: String,
    pub ts: i64,
    /// `ok`, `warning`, or `critical` - computed against the sensor bands.
    pub band: String,
    /// Anchor knot hash when the reading was anchored on the asset string.
    #[serde(default)]
    pub anchor: String,
}

/// One AI diagnosis / recommendation event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosisEvent {
    pub project_id: String,
    pub asset_id: String,
    pub agent_id: String,
    pub diagnosis: String,
    pub recommendation: String,
    pub confidence: f64,
    pub ts: i64,
    #[serde(default)]
    pub anchor: String,
}

/// One stakeholder approval / governance event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalEvent {
    pub project_id: String,
    pub subject: String,
    pub approved_by: String,
    pub role: Role,
    pub note: String,
    pub ts: i64,
    #[serde(default)]
    pub anchor: String,
}

/// Classify a reading against the sensor's declared bands.
pub fn classify_band(sensor: &SensorRecord, value: f64) -> &'static str {
    if let Some([lo, hi]) = sensor.optimum {
        if value >= lo && value <= hi {
            return "ok";
        }
        if let Some([wlo, whi]) = sensor.warning {
            if value >= wlo && value <= whi {
                return "warning";
            }
        }
        return "critical";
    }
    if let Some([lo, hi]) = sensor.range {
        if value < lo || value > hi {
            return "critical";
        }
    }
    "ok"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizing_pilot() {
        let p = NodePlan::recommend(10, 100.0, 1, false);
        assert_eq!(p.tier, ScaleTier::Pilot);
        assert_eq!(p.node_count, 1);
        // Single node hosts all roles.
        assert_eq!(p.role_layout.len(), 1);
        assert!(p.role_layout[0].contains(&"ingestion_gateway".to_string()));
    }

    #[test]
    fn sizing_event_rate_dominates() {
        // Few assets but a firehose of readings → Growth by event rate.
        let p = NodePlan::recommend(40, 50_000.0, 1, false);
        assert_eq!(p.tier, ScaleTier::Growth);
    }

    #[test]
    fn sizing_multi_jurisdiction_forces_sovereign() {
        let p = NodePlan::recommend(10, 10.0, 3, true);
        assert_eq!(p.tier, ScaleTier::Sovereign);
        assert_eq!(p.node_count, 8);
    }

    #[test]
    fn band_classification() {
        let s = SensorRecord {
            id: "s1".into(),
            parent_asset_id: "a1".into(),
            parameter: "soil_moisture".into(),
            unit: "%".into(),
            cadence: "6min".into(),
            readings_per_hour: 10.0,
            range: Some([0.0, 100.0]),
            optimum: Some([35.0, 55.0]),
            warning: Some([20.0, 70.0]),
            protocol: "mqtt".into(),
            endpoint: String::new(),
            sharing_policy: "private".into(),
            write_path: "gateway".into(),
        };
        assert_eq!(classify_band(&s, 45.0), "ok");
        assert_eq!(classify_band(&s, 25.0), "warning");
        assert_eq!(classify_band(&s, 10.0), "critical");
    }

    #[test]
    fn project_wallet_deterministic() {
        assert_eq!(project_wallet("prj_abc"), project_wallet("prj_abc"));
        assert_ne!(project_wallet("prj_abc"), project_wallet("prj_def"));
        assert!(project_wallet("prj_abc").starts_with("0x"));
        assert_eq!(project_wallet("prj_abc").len(), 42);
    }

    #[test]
    fn role_matrix() {
        assert!(Role::Owner.can_mutate());
        assert!(Role::Administrator.can_mutate());
        assert!(!Role::Operator.can_mutate());
        assert!(!Role::Auditor.can_mutate());
        assert!(Role::Owner.is_owner());
        assert!(!Role::Administrator.is_owner());
    }
}
