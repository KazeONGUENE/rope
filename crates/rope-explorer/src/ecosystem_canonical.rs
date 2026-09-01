//! Canonical Datachain Rope ecosystem registry.
//!
//! `dcscan.io/ecosystem` originally sourced its project cards exclusively
//! from Ecosystem Deployment Console (EDC) instances via
//! `/api/v1/ecosystem/public/projects`. That surface only covers projects
//! that self-registered through a live EDC (currently zero on production,
//! per `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11` §11).
//!
//! The real Datachain ecosystem is much larger: dcswap.net, tanastok.io,
//! naturaproof.com, datawallet.plus, id.datachain.network, the canonical
//! AI agents (semantic / oracle / insurance / validation / compliance),
//! plus the Datachain Foundation and DCScan itself.
//!
//! This module holds a hand-curated registry of those projects, each
//! anchored to a verifiable on-chain wallet or contract (where one
//! exists) and a public URL. `refresh_ecosystem_directory_cache` merges
//! these entries alongside anything the EDC returns. Deduplication is by
//! lowercase `id`; when both sources publish the same id, the EDC entry
//! wins because it represents a live registered deployment.
//!
//! # Design constraints
//!
//! - Only real projects. No stubs, no roadmap items. Every entry must
//!   have either a live public URL or a documented on-chain footprint.
//! - Every field is a plain string so the shape matches the EDC's
//!   `/api/v1/ecosystem/public/projects` response exactly.
//! - Sourced from the workspace handovers in `.cursor/rules/`, the
//!   deployed nginx vhosts on rope-vps, and DCSwap's `271828.json`.
//!
//! When adding a new entry: pick the archetype from the enum-like set in
//! `canonical_archetypes()` so the frontend already knows how to render
//! its badge. New archetypes must be added there **and** in the badge map
//! in `static/ecosystem.html`.

use serde_json::{json, Value};

/// The genesis timestamp used for canonical projects that predate the
/// Ecosystem Deployment Console launch. Chosen deliberately so the "Since"
/// field on the `/ecosystem` cards reads honestly: the projects have
/// existed on the network since the 2026-05 canonical-agents drop.
const CANONICAL_SINCE_TS: i64 = 1_777_536_000; // 2026-05-05T12:00:00Z

/// Ecosystem project visibility level. Controls what unauthenticated
/// viewers see on `/ecosystem` and how project detail queries at
/// `/api/v1/ecosystem/directory/:id` respond.
///
/// | Variant           | List view | Detail view | Open project button |
/// |-------------------|-----------|-------------|---------------------|
/// | `Public`          | shown     | full        | enabled             |
/// | `PrivateVisible`  | shown     | redacted    | disabled            |
/// | `PrivateHidden`   | hidden    | 404         | n/a                 |
///
/// The list-view "hidden" and detail-view "redacted" behaviour are
/// enforced server-side (see `ecosystem_directory` and
/// `ecosystem_directory_project` in `main.rs`). Admin-authenticated
/// viewers (via `X-Admin-Token` carrying a dynamic admin token with the
/// `ProjectAdmin` role - or `MultiRole`; see
/// [`crate::admin_tokens`]) see everything, with the visibility flag
/// preserved on the card so the frontend can render the correct eye
/// icon (open / closed / hidden).
///
/// See `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc`
/// §30 for the full design.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    PrivateVisible,
    PrivateHidden,
}

impl Visibility {
    /// Wire-format string used on JSON responses. Kept lowercase +
    /// snake_case so the frontend can switch on it directly.
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::PrivateVisible => "private_visible",
            Visibility::PrivateHidden => "private_hidden",
        }
    }

    /// Parse from the wire-format string. Unknown values fall back to
    /// `Public` because that is the least-surprising default when a
    /// downstream (or a manually-crafted request) sends garbage.
    pub fn from_str(s: &str) -> Visibility {
        match s.to_ascii_lowercase().as_str() {
            "private_visible" => Visibility::PrivateVisible,
            "private_hidden" => Visibility::PrivateHidden,
            _ => Visibility::Public,
        }
    }
}

impl Default for Visibility {
    fn default() -> Self {
        Visibility::Public
    }
}

/// Canonical project ids that are `PrivateHidden`. Kept as a small static
/// list because the vast majority of the registry is `Public`, and
/// bloating every `CanonicalEntry` struct literal with a `visibility:`
/// field would add ~24 lines of boilerplate for a 4-entry override.
///
/// To hide a new project: add its lowercase id here. To un-hide:
/// remove it. Both operations are gated behind the operator green-light
/// (this file is source, not runtime config).
const PRIVATE_HIDDEN_IDS: &[&str] = &[
    // Per user request 2026-08-13: owner-hidden research / early-stage
    // side projects; visible only to Kazé A. ONGUENE via admin token.
    "moneymaker",
    "picentriq",
    "reinvoiceotc",
    "braincities-2026",
];

/// Canonical project ids that are `PrivateVisible`. Empty for now;
/// documented so future operators know where to add entries. When
/// populated, matching entries render name + description publicly but
/// hide the detail modal, on-chain string, and open-project button
/// behind the admin token.
const PRIVATE_VISIBLE_IDS: &[&str] = &[];

/// Compute the visibility for a canonical entry id. Non-canonical
/// (EDC-registered) entries always default to `Public`; if a self-hosted
/// EDC ever wants to publish a private project, it must return
/// `"visibility": "private_visible"` (or `"private_hidden"`) on its own
/// `/api/v1/ecosystem/public/projects/:id` response.
pub fn visibility_for(id: &str) -> Visibility {
    let id_lower = id.to_ascii_lowercase();
    if PRIVATE_HIDDEN_IDS.contains(&id_lower.as_str()) {
        Visibility::PrivateHidden
    } else if PRIVATE_VISIBLE_IDS.contains(&id_lower.as_str()) {
        Visibility::PrivateVisible
    } else {
        Visibility::Public
    }
}

/// Emit the full curated set as JSON project cards ready to be merged
/// with EDC output. Each card has `source: "canonical"` so downstream
/// consumers can distinguish curated entries from live EDC registrations.
pub fn canonical_project_cards() -> Vec<Value> {
    let entries = canonical_entries();
    entries.into_iter().map(entry_to_card).collect()
}

/// The archetype tokens the frontend badge map knows about. Keep in sync
/// with `static/ecosystem.html::archetypeBadge`.
pub fn canonical_archetypes() -> &'static [&'static str] {
    &[
        "predictive_maintenance",
        "environmental_monitoring",
        "hybrid",
        "dex",
        "asset_tokenization",
        "identity_wallet",
        "sso",
        "block_explorer",
        "ai_agent",
        "governance",
        "foundation",
        "bridge",
        "biodiversity",
        "health",
        "investment",
        "infrastructure",
    ]
}

struct CanonicalEntry {
    id: &'static str,
    name: &'static str,
    archetype: &'static str,
    status: &'static str,
    tags: &'static [&'static str],
    region: &'static str,
    country: &'static str,
    wallet: &'static str,
    stakeholder_url: &'static str,
    description: &'static str,
    /// Free-form facet count fields so cards remain visually consistent
    /// with the archetype-specific EDC cards. Zero when not applicable
    /// (e.g. DEX has neither "assets monitored" nor "sensors").
    asset_count: u64,
    sensor_count: u64,
    /// Optional per-entry override for the "Since" field. `None` uses
    /// `CANONICAL_SINCE_TS`.
    created_at_override: Option<i64>,
    /// Optional URL to a project logo (favicon.ico, /logo.svg, etc.).
    /// Frontend renders it as a 32x32 icon on each card when present and
    /// falls back to the first-letter tile / archetype glyph when `None`.
    /// MUST be an absolute `https://` URL (validated in the tests below)
    /// so the ecosystem page never issues a mixed-content request.
    ///
    /// When adding a new entry, HEAD-check the URL before populating
    /// (`curl -sSI --max-time 5 <url>`); the daily test
    /// `logo_url_is_absolute_https_when_present` only validates the
    /// shape, not live reachability. Leaving `None` is always safe.
    logo_url: Option<&'static str>,
}

fn entry_to_card(e: CanonicalEntry) -> Value {
    let visibility = visibility_for(e.id);
    json!({
        "id": e.id,
        "name": e.name,
        "archetype": e.archetype,
        "status": e.status,
        "tags": e.tags,
        "region": e.region,
        "country": e.country,
        "wallet": e.wallet,
        "stakeholder_url": e.stakeholder_url,
        "description": e.description,
        "asset_count": e.asset_count,
        "sensor_count": e.sensor_count,
        "created_at": e.created_at_override.unwrap_or(CANONICAL_SINCE_TS),
        "source": "canonical",
        "edc_base": null,
        // Present as JSON `null` when unset so downstream consumers can
        // rely on the key always existing (no `.contains(logo_url)` checks
        // in JS). Test `logo_url_key_is_always_emitted` enforces this.
        "logo_url": e.logo_url,
        // Visibility flag. Public for the vast majority; PrivateHidden
        // for the 4 owner-only side projects (Moneymaker, Picentriq,
        // ReinvoiceOTC, BrainCities 2026). Server-side handlers filter
        // PrivateHidden out of non-admin list responses and redact
        // PrivateVisible detail responses; the flag is still emitted on
        // the card so the admin-view frontend can render the correct
        // eye icon.
        "visibility": visibility.as_str(),
    })
}

/// The curated Datachain Rope ecosystem. When adding an entry, verify
/// the `stakeholder_url` is live (`curl -I -L $URL`) and the `wallet`
/// address resolves on dcscan (`https://dcscan.io/address/<wallet>`).
fn canonical_entries() -> Vec<CanonicalEntry> {
    vec![
        // ------------------------------------------------------------------
        // Live production apps (verified 2026-08-11)
        // ------------------------------------------------------------------
        CanonicalEntry {
            id: "dcswap",
            name: "DCSwap",
            archetype: "dex",
            status: "live",
            tags: &["dex", "amm", "dcr-20", "liquidity", "fat"],
            region: "Public network",
            country: "GLOBAL",
            // DCSwapRouter (contracts/deployments/271828.json)
            wallet: "0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4",
            stakeholder_url: "https://dcswap.net",
            description: "Datachain Rope's native DEX. UniswapV2-style AMM \
                          with zero-fee stablecoin pairs (FAT/USDC, FAT/USDT, \
                          FAT/EUROD) and a canonical price oracle at \
                          /v1/prices consumed across the ecosystem.",
            asset_count: 4,   // live pools
            sensor_count: 62, // multi-strategy bot wallets
            created_at_override: Some(1_772_064_000), // 2026-02-26
            logo_url: Some("https://dcswap.net/favicon.png"),
        },
        CanonicalEntry {
            id: "tanastok",
            name: "Tanastok",
            archetype: "asset_tokenization",
            status: "live",
            tags: &["rwa", "erc-3643", "dcnft", "mifid-ii", "gold", "forest"],
            region: "Global",
            country: "GLOBAL",
            // TanastokONCHAINID (T-REX identity)
            wallet: "0xE9D4fd64DF93fe848fE13303EAa28008feb72789",
            stakeholder_url: "https://tanastok.io",
            description: "Real-world asset tokenization on Datachain Rope. \
                          1,626+ assets minted as DCNFT title deeds paired \
                          with ERC-3643 security tokens (Kibali Gold Mine, \
                          Congo DRC; carbon forests; luxury goods).",
            asset_count: 1626,
            sensor_count: 0,
            created_at_override: Some(1_775_030_400), // 2026-03-30
            logo_url: Some("https://tanastok.io/favicon.ico"),
        },
        CanonicalEntry {
            id: "naturaproof",
            name: "NaturaProof",
            archetype: "biodiversity",
            status: "live",
            tags: &["biodiversity", "verification", "certification", "esg"],
            region: "Global",
            country: "GLOBAL",
            // Static registrar wallet; adjust when live claim-issuer contract lands
            wallet: "",
            stakeholder_url: "https://naturaproof.com",
            description: "Biodiversity and environmental claim verification \
                          platform. Anchors third-party field measurements \
                          and certificate issuances as knots on per-asset \
                          entity strings for ESG audit.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
        CanonicalEntry {
            id: "dcscan",
            name: "DCScan (DC Explorer)",
            archetype: "block_explorer",
            status: "live",
            tags: &["explorer", "indexer", "supply", "labels", "dcscan"],
            region: "Public network",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "https://dcscan.io",
            description: "The canonical block explorer for Datachain Rope. \
                          Serves the /api/v1/supply/* endpoints used by \
                          CoinMarketCap and CoinGecko, the address / tx / \
                          token detail pages, and the ecosystem directory \
                          you are looking at right now.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: Some(1_772_582_400), // 2026-03-03
            logo_url: Some("https://dcscan.io/assets/logo.svg"),
        },
        CanonicalEntry {
            id: "datachain-foundation",
            name: "Datachain Foundation",
            archetype: "foundation",
            status: "live",
            tags: &["foundation", "governance", "treasury", "sovereign"],
            region: "Global",
            country: "FR",
            // Foundation Operator (see UNCIRCULATED_BUILTIN)
            wallet: "0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb",
            stakeholder_url: "https://datachain.network",
            description: "The Datachain Foundation SAS - author and steward \
                          of the Quipu Primitive Canon, the DC FAT emission \
                          schedule, and the Datachain Rope network itself.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: Some("https://dcscan.io/assets/logo.svg"),
        },
        CanonicalEntry {
            id: "datawallet-plus",
            name: "Datawallet+",
            archetype: "identity_wallet",
            status: "live",
            tags: &[
                "wallet",
                "onchainid",
                "did",
                "post-quantum",
                "gdpr",
                "art17",
            ],
            region: "Global",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "https://datawallet.plus",
            description: "Post-quantum identity and asset wallet with native \
                          GDPR Article 17 support (rope_untieKnot + tombstone \
                          knots). ONCHAINID-compatible; DID-issuer for \
                          Tanastok's compliant investor onboarding.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: Some("https://datawallet.plus/favicon.ico"),
        },
        CanonicalEntry {
            id: "datachain-id",
            name: "Datachain ID (SSO)",
            archetype: "sso",
            status: "live",
            tags: &["sso", "oauth", "ed25519", "jwt", "eip-191"],
            region: "Public network",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "https://id.datachain.network",
            description: "Ecosystem-wide single sign-on gateway. Issues \
                          Ed25519-signed JWTs for any platform via \
                          Datawallet+ credentials or wallet EIP-191 \
                          signatures. Consumed by dcscan.io, dcswap.net, \
                          tanastok.io.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: Some(1_783_411_200), // 2026-07-07
            logo_url: Some("https://dcscan.io/assets/logo.svg"),
        },
        CanonicalEntry {
            id: "syndicated-investment",
            name: "Syndicated Investment",
            archetype: "investment",
            status: "live",
            tags: &["fund", "carbon", "green", "syndication"],
            region: "Europe",
            country: "FR",
            wallet: "",
            stakeholder_url: "https://syndicated.ltd",
            description: "Investment syndication vehicle bridging the \
                          Datachain Foundation's Green Fund with tokenized \
                          real-world assets on Tanastok.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
        // ------------------------------------------------------------------
        // Canonical AI agents (2026-05-05 handover, all live)
        // ------------------------------------------------------------------
        CanonicalEntry {
            id: "semantic-agent",
            name: "SemanticAgent",
            archetype: "ai_agent",
            status: "live",
            tags: &["ai", "search", "index", "tantivy", "testimony"],
            region: "Public network",
            country: "GLOBAL",
            wallet: "0x0000000000000000000000000000000000000C001",
            stakeholder_url: "https://semantic-agent.datachain.network",
            description: "Indexes every knot into a tantivy full-text index \
                          and exposes /v1/search. Anchors merkle-rooted \
                          IndexCheckpointTestimonies every 10 min.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: Some("https://agents.datachain.network/favicon.ico"),
        },
        CanonicalEntry {
            id: "oracle-agent",
            name: "OracleAgent",
            archetype: "ai_agent",
            status: "live",
            tags: &["ai", "oracle", "price", "attestation", "dcswap"],
            region: "Public network",
            country: "GLOBAL",
            wallet: "0x0000000000000000000000000000000000000C002",
            stakeholder_url: "https://agents.datachain.network",
            description: "Pulls the canonical DC FAT price from DCSwap \
                          reserves (VWAP with outlier rejection) and \
                          anchors PriceAttestation knots. Source of truth \
                          for /api/v1/stats.fatPrice on dcscan.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: Some("https://agents.datachain.network/favicon.ico"),
        },
        CanonicalEntry {
            id: "insurance-agent",
            name: "InsuranceAgent",
            archetype: "ai_agent",
            status: "live",
            tags: &["ai", "parametric-insurance", "tanastok", "attestation"],
            region: "Public network",
            country: "GLOBAL",
            wallet: "0x0000000000000000000000000000000000000C003",
            stakeholder_url: "https://agents.datachain.network",
            description: "Polls Tanastok tokenized-asset feeds hourly and \
                          issues ParametricInsuranceAttestation knots \
                          against per-asset entity strings.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: Some("https://agents.datachain.network/favicon.ico"),
        },
        CanonicalEntry {
            id: "validation-agent",
            name: "ValidationAgent",
            archetype: "ai_agent",
            status: "live",
            tags: &["ai", "consensus", "ed25519", "dilithium3", "testimony"],
            region: "Public network",
            country: "GLOBAL",
            wallet: "0x0000000000000000000000000000000000000C004",
            stakeholder_url: "https://agents.datachain.network",
            description: "Verifies hybrid (Ed25519 + Dilithium3) signatures \
                          on every cord anchor. First line of defence \
                          against Testimony-consensus signature forgery.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: Some("https://agents.datachain.network/favicon.ico"),
        },
        CanonicalEntry {
            id: "compliance-agent",
            name: "ComplianceAgent",
            archetype: "ai_agent",
            status: "live",
            tags: &["ai", "gdpr", "art17", "mifid-ii", "dora"],
            region: "Public network",
            country: "GLOBAL",
            wallet: "0x0000000000000000000000000000000000000C005",
            stakeholder_url: "https://compliance-agent.datachain.network",
            description: "GDPR Article 17 erasure orchestration (rope_untieKnot \
                          + tombstone anchoring). Exposes /v1/gdpr/article17 \
                          for Datawallet+ users to request cryptographic \
                          erasure of their on-chain history.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: Some("https://agents.datachain.network/favicon.ico"),
        },
        // ------------------------------------------------------------------
        // Governance / bridge / infra (live but back-of-house)
        // ------------------------------------------------------------------
        CanonicalEntry {
            id: "governance-voting",
            name: "Governance & NGO Cause Voting",
            archetype: "governance",
            status: "live",
            tags: &["governance", "voting", "vote-escrow", "ngo", "cause"],
            region: "Public network",
            country: "GLOBAL",
            // VoteEscrow (governance-phase5 handover)
            wallet: "0x4e8D198a2D1072e5aA507fD7a73c2047226f5E40",
            stakeholder_url: "https://dcscan.io/vote",
            description: "60% random-jury + pay-to-vote governance stack. \
                          NGOs can submit causes, a randomly-drawn jury \
                          combined with FAT-weighted votes decides funding, \
                          and winners receive native FAT + a bespoke cause \
                          token via CauseTokenFactory.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: Some(1_785_484_800), // 2026-07-30
            logo_url: Some("https://dcscan.io/assets/logo.svg"),
        },
        CanonicalEntry {
            id: "fat-migration-minter",
            name: "Legacy DC to FAT Migration",
            archetype: "bridge",
            status: "live",
            tags: &["migration", "erc-777", "xrc-20", "fat", "escrow"],
            region: "Public network",
            country: "GLOBAL",
            // FATMigrationMinter (Rope 271828)
            wallet: "0x70406ae110D6ccff9a73a2AC2b82d3B666B5a51a",
            stakeholder_url: "https://dcswap.net",
            description: "Escrow-release minter that mints native FAT on \
                          Datachain Rope for every verified legacy DC burn \
                          on Ethereum (ERC-777) or XDC (XRC-20). Paused \
                          before deploy; Timelock-owned; 500M FAT escrowed \
                          per top-up.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: Some(1_783_929_600), // 2026-07-13
            logo_url: Some("https://dcscan.io/assets/logo.svg"),
        },
        CanonicalEntry {
            id: "ecosystem-deployment-console",
            name: "Ecosystem Deployment Console",
            archetype: "infrastructure",
            status: "live",
            tags: &["console", "edc", "deployment", "sovereign-node"],
            region: "Public network",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "https://console.datachain.network/console/",
            description: "One-click sovereign-node deployment for new \
                          ecosystem projects. Signs stakeholders in via \
                          EIP-191; ships with a sandbox mode (no KYB, no \
                          cost) for prototyping predictive-maintenance and \
                          environmental-monitoring deployments.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: Some(1_783_411_200),
            logo_url: Some("https://dcscan.io/assets/logo.svg"),
        },
        // ------------------------------------------------------------------
        // Development / preview (status: development). Included so the
        // ecosystem page reflects the actual project surface area even
        // before a public URL goes live.
        // ------------------------------------------------------------------
        CanonicalEntry {
            id: "careaway",
            name: "Careaway",
            archetype: "health",
            status: "development",
            tags: &["health", "care", "gdpr", "onchainid"],
            region: "Europe",
            country: "FR",
            wallet: "",
            stakeholder_url: "",
            description: "Health-and-wellness care coordination platform. \
                          Anchors care-plan enrolments, deliveries, and \
                          payouts as knots on per-beneficiary strings, \
                          gated by Datawallet+ consent.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
        CanonicalEntry {
            id: "databox-network",
            name: "Global Databox Network",
            archetype: "infrastructure",
            status: "development",
            tags: &["databox", "iot", "storage", "sovereign"],
            region: "Global",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "https://dcscan.io/databoxes",
            description: "Physical, home-hosted micro-nodes that anchor \
                          per-household data streams as sovereign strings. \
                          Complements the datawallet+ mobile app with a \
                          long-lived storage tier.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            // Datachain-Foundation-hosted feature living on dcscan.io; use
            // the Datachain mark rather than leaving the card iconless.
            logo_url: Some("https://dcscan.io/assets/logo.svg"),
        },
        CanonicalEntry {
            id: "mapstore",
            name: "Mapstore",
            archetype: "asset_tokenization",
            status: "development",
            tags: &["retail", "location", "poi", "harvester"],
            region: "Asia",
            country: "KR",
            wallet: "",
            stakeholder_url: "",
            description: "Retail location / point-of-interest data \
                          harvester and marketplace, seeded from the Korean \
                          Small Business Development Agency dataset.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
        CanonicalEntry {
            id: "shametrails",
            name: "Shametrails",
            archetype: "hybrid",
            status: "development",
            tags: &["reputation", "attestation"],
            region: "Global",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "",
            description: "Reputation-and-attestation platform for retail \
                          and hospitality operators. Anchors verified \
                          customer-experience reports on-chain.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
        CanonicalEntry {
            id: "moneymaker",
            name: "Moneymaker",
            archetype: "investment",
            status: "development",
            tags: &["monetization", "yield"],
            region: "Global",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "",
            description: "Retail-oriented yield-and-monetization dashboard \
                          on top of DCSwap liquidity pools and Tanastok \
                          fractional shares.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
        CanonicalEntry {
            id: "picentriq",
            name: "Picentriq",
            archetype: "hybrid",
            status: "development",
            tags: &["ai", "media", "generation"],
            region: "Global",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "",
            description: "AI-assisted media generation and licensing \
                          workspace, with per-asset royalty anchoring on \
                          Datachain Rope.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
        CanonicalEntry {
            id: "reinvoiceotc",
            name: "ReinvoiceOTC",
            archetype: "hybrid",
            status: "development",
            tags: &["invoice", "otc", "finance"],
            region: "Global",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "",
            description: "OTC invoice-and-receivable settlement rails using \
                          DCR-20 stablecoins on DCSwap for on-chain \
                          settlement.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
        CanonicalEntry {
            id: "braincities-2026",
            name: "BrainCities 2026",
            archetype: "hybrid",
            status: "development",
            tags: &["urban", "ai", "iot"],
            region: "Global",
            country: "GLOBAL",
            wallet: "",
            stakeholder_url: "",
            description: "Urban-AI research initiative exploring city-scale \
                          sensor + AI deployments anchored on the Datachain \
                          Rope canon.",
            asset_count: 0,
            sensor_count: 0,
            created_at_override: None,
            logo_url: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ids_unique_and_lowercase() {
        let cards = canonical_project_cards();
        let mut ids = std::collections::HashSet::new();
        for card in &cards {
            let id = card.get("id").and_then(|v| v.as_str()).unwrap();
            assert!(!id.is_empty(), "id must not be empty");
            assert_eq!(id, id.to_lowercase(), "id must be lowercase: {id}");
            assert!(ids.insert(id.to_string()), "duplicate id: {id}");
        }
        assert!(cards.len() >= 15, "expected at least 15 canonical entries");
    }

    #[test]
    fn every_archetype_is_declared() {
        let known: std::collections::HashSet<&str> =
            canonical_archetypes().iter().copied().collect();
        for card in canonical_project_cards() {
            let arch = card.get("archetype").and_then(|v| v.as_str()).unwrap();
            assert!(
                known.contains(arch),
                "archetype {arch} not in canonical_archetypes(); update the frontend badge map"
            );
        }
    }

    #[test]
    fn cards_have_stable_source_marker() {
        for card in canonical_project_cards() {
            assert_eq!(card.get("source").and_then(|v| v.as_str()), Some("canonical"));
            assert!(card.get("edc_base").map_or(true, |v| v.is_null()));
        }
    }

    #[test]
    fn live_entries_carry_public_url_or_on_chain_footprint() {
        for card in canonical_project_cards() {
            let status = card.get("status").and_then(|v| v.as_str()).unwrap();
            if status != "live" {
                continue;
            }
            let url = card.get("stakeholder_url").and_then(|v| v.as_str()).unwrap_or("");
            let wallet = card.get("wallet").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                !url.is_empty() || !wallet.is_empty(),
                "live entry {} must have either a URL or a wallet",
                card.get("id").unwrap()
            );
        }
    }

    #[test]
    fn logo_url_field_is_always_emitted() {
        // Every card must carry the `logo_url` field so the frontend can rely
        // on it (either a string URL or JSON `null`). No entry may silently
        // omit the key.
        for card in canonical_project_cards() {
            let id = card.get("id").and_then(|v| v.as_str()).unwrap();
            assert!(
                card.get("logo_url").is_some(),
                "entry {id} is missing logo_url; must be Some(url) or None"
            );
        }
    }

    #[test]
    fn logo_url_when_present_is_absolute_https() {
        // Prevents accidental relative paths (which would 404 from any
        // origin) and mixed-content requests (which browsers refuse).
        for card in canonical_project_cards() {
            let id = card.get("id").and_then(|v| v.as_str()).unwrap();
            let logo = card.get("logo_url").unwrap();
            if logo.is_null() {
                continue;
            }
            let url = logo.as_str().unwrap_or_else(|| {
                panic!("logo_url for {id} must be a string or null, got {logo}")
            });
            assert!(
                url.starts_with("https://"),
                "logo_url for {id} must be absolute https:// (got: {url})"
            );
            assert!(
                !url.contains(' ') && !url.contains('\n'),
                "logo_url for {id} contains whitespace: {url}"
            );
        }
    }

    #[test]
    fn logo_url_domains_are_from_known_ecosystem_hosts() {
        // Guardrail: any logo we serve must come from a Datachain-controlled
        // domain OR a canonical partner domain listed in the ecosystem
        // handovers. This blocks a future contributor from wiring an
        // unaudited third-party CDN into every ecosystem page render.
        let allowed_hosts: &[&str] = &[
            "dcscan.io",
            "datachain.network",
            "agents.datachain.network",
            "id.datachain.network",
            "console.datachain.network",
            "compliance-agent.datachain.network",
            "semantic-agent.datachain.network",
            "tanastok.io",
            "datawallet.plus",
            "dcswap.net",
            "naturaproof.com",
            "syndicated.ltd",
        ];
        for card in canonical_project_cards() {
            let id = card.get("id").and_then(|v| v.as_str()).unwrap();
            let logo = card.get("logo_url").unwrap();
            if logo.is_null() {
                continue;
            }
            let url = logo.as_str().unwrap();
            // Extract the host between "https://" and the next "/".
            let after = &url["https://".len()..];
            let host = after.split('/').next().unwrap_or("");
            assert!(
                allowed_hosts.contains(&host),
                "logo_url for {id} points at unapproved host {host}; add to \
                 allowed_hosts or use a canonical Datachain URL"
            );
        }
    }

    #[test]
    fn logo_url_coverage_meets_minimum_bar() {
        // Sanity floor: at least a third of the canonical entries must
        // carry a logo so the ecosystem page doesn't look empty. This is
        // a ratchet - the number should only ever go up.
        let cards = canonical_project_cards();
        let with_logo = cards
            .iter()
            .filter(|c| c.get("logo_url").map_or(false, |v| !v.is_null()))
            .count();
        let ratio = (with_logo * 100) / cards.len().max(1);
        // Ratchet: 15 entries carry a logo as of 2026-08-12. Do not lower
        // this floor without a compelling reason - the point of the
        // canonical registry is to look like a real ecosystem directory,
        // not a list of unbranded cards.
        assert!(
            with_logo >= 12,
            "expected at least 12 entries with logo_url, got {with_logo}/{} ({ratio}%)",
            cards.len()
        );
    }

    // ---- Visibility feature tests (added 2026-08-13) --------------------

    #[test]
    fn visibility_as_str_round_trips_through_from_str() {
        // Wire-format contract: every enum variant must serialise + parse
        // back to itself. Guards against drift between the string constants
        // used on the wire and the enum shape.
        for v in [
            Visibility::Public,
            Visibility::PrivateVisible,
            Visibility::PrivateHidden,
        ] {
            let s = v.as_str();
            let parsed = Visibility::from_str(s);
            assert_eq!(parsed, v, "round-trip failed for {v:?} via {s}");
        }
    }

    #[test]
    fn visibility_from_str_defaults_to_public_for_unknown_input() {
        // Non-surprising default: anything we don't recognise falls back to
        // public. Prevents a downstream typo from silently hiding a project.
        assert_eq!(Visibility::from_str(""), Visibility::Public);
        assert_eq!(Visibility::from_str("garbage"), Visibility::Public);
        assert_eq!(Visibility::from_str("PRIVATE"), Visibility::Public);
        // But recognised values must still map correctly, including odd
        // casing (matches what a self-hosted EDC might publish).
        assert_eq!(
            Visibility::from_str("PRIVATE_VISIBLE"),
            Visibility::PrivateVisible
        );
        assert_eq!(
            Visibility::from_str("Private_Hidden"),
            Visibility::PrivateHidden
        );
    }

    #[test]
    fn visibility_default_is_public() {
        // Belt-and-braces: the Default impl also gives Public. Any struct
        // literal that omits visibility gets the same posture as a bare id
        // lookup with no override.
        assert_eq!(Visibility::default(), Visibility::Public);
    }

    #[test]
    fn private_hidden_ids_contain_the_four_owner_hidden_projects() {
        // Locks in the user request from 2026-08-13: Moneymaker,
        // Picentriq, ReinvoiceOTC, BrainCities 2026 must all be
        // owner-only. If any of these accidentally goes public, this test
        // fails at CI before it can reach production.
        let expected = ["moneymaker", "picentriq", "reinvoiceotc", "braincities-2026"];
        for id in expected {
            assert!(
                PRIVATE_HIDDEN_IDS.contains(&id),
                "PRIVATE_HIDDEN_IDS is missing owner-hidden project {id}"
            );
            assert_eq!(
                visibility_for(id),
                Visibility::PrivateHidden,
                "visibility_for({id}) did not resolve to PrivateHidden"
            );
        }
    }

    #[test]
    fn private_hidden_ids_are_lowercase_and_nonempty() {
        // Lookup is case-insensitive (visibility_for lowercases the input),
        // but the list itself must stay canonical lowercase so future
        // reviewers can grep for a project by lowercase id and find it.
        for id in PRIVATE_HIDDEN_IDS {
            assert!(!id.is_empty(), "PRIVATE_HIDDEN_IDS contains an empty id");
            assert_eq!(
                *id,
                id.to_lowercase(),
                "PRIVATE_HIDDEN_IDS entry {id} must be lowercase"
            );
        }
    }

    #[test]
    fn private_hidden_ids_have_no_duplicates() {
        // A duplicate would still work at lookup time but signals a stale
        // entry that outlived a project rename. Fail loudly instead of
        // silently succeeding.
        let mut seen = std::collections::HashSet::new();
        for id in PRIVATE_HIDDEN_IDS {
            assert!(
                seen.insert(*id),
                "PRIVATE_HIDDEN_IDS contains duplicate id: {id}"
            );
        }
    }

    #[test]
    fn private_hidden_ids_exist_in_canonical_registry() {
        // If someone renames a canonical entry but forgets to update the
        // hidden list, the project becomes silently public again. This
        // test catches that class of mistake by asserting every hidden id
        // resolves to an actual entry.
        let cards = canonical_project_cards();
        let known: std::collections::HashSet<String> = cards
            .iter()
            .map(|c| c.get("id").and_then(|v| v.as_str()).unwrap().to_string())
            .collect();
        for id in PRIVATE_HIDDEN_IDS {
            assert!(
                known.contains(*id),
                "PRIVATE_HIDDEN_IDS entry {id} is not in canonical_project_cards(); \
                 either add the entry or remove it from the hidden list"
            );
        }
    }

    #[test]
    fn visibility_for_is_case_insensitive() {
        // Callers can be sloppy about casing (frontend, admin token flow,
        // ecosystem exploration script). The resolver normalises input.
        assert_eq!(visibility_for("Moneymaker"), Visibility::PrivateHidden);
        assert_eq!(visibility_for("MONEYMAKER"), Visibility::PrivateHidden);
        assert_eq!(visibility_for("moneymaker"), Visibility::PrivateHidden);
    }

    #[test]
    fn visibility_for_returns_public_for_unlisted_ids() {
        // The vast majority of canonical entries stay public. Verify the
        // resolver's default path for a couple of known-public ids.
        assert_eq!(visibility_for("tanastok"), Visibility::Public);
        assert_eq!(visibility_for("dcswap"), Visibility::Public);
        assert_eq!(visibility_for("nonexistent-project-id"), Visibility::Public);
    }

    #[test]
    fn every_card_carries_visibility_field() {
        // Frontend contract: every card in the directory MUST have a
        // visibility field so the JS can switch on it without a
        // `.contains()` check. Guards against a future refactor that drops
        // the field on some code path.
        for card in canonical_project_cards() {
            let id = card.get("id").and_then(|v| v.as_str()).unwrap();
            let vis = card.get("visibility");
            assert!(
                vis.is_some(),
                "entry {id} is missing visibility field"
            );
            let vis_str = vis.unwrap().as_str();
            assert!(
                vis_str.is_some(),
                "entry {id} visibility is not a string: {vis:?}"
            );
            let vis_str = vis_str.unwrap();
            assert!(
                matches!(vis_str, "public" | "private_visible" | "private_hidden"),
                "entry {id} has invalid visibility {vis_str}"
            );
        }
    }

    #[test]
    fn cards_visibility_matches_visibility_for_lookup() {
        // The card's visibility field must match what visibility_for
        // returns for the same id. Catches a refactor that changes one
        // path without updating the other.
        for card in canonical_project_cards() {
            let id = card.get("id").and_then(|v| v.as_str()).unwrap();
            let card_vis = card.get("visibility").and_then(|v| v.as_str()).unwrap();
            let expected_vis = visibility_for(id).as_str();
            assert_eq!(
                card_vis, expected_vis,
                "entry {id}: card visibility={card_vis}, but visibility_for()={expected_vis}"
            );
        }
    }

    #[test]
    fn four_owner_hidden_cards_render_as_private_hidden() {
        // End-to-end assertion: the exact 4 projects the user asked to
        // hide must show private_hidden on their card. Combines
        // visibility_for + entry_to_card in one shot.
        let cards = canonical_project_cards();
        let expected = ["moneymaker", "picentriq", "reinvoiceotc", "braincities-2026"];
        for id in expected {
            let card = cards
                .iter()
                .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(id))
                .unwrap_or_else(|| panic!("canonical registry missing entry {id}"));
            let vis = card.get("visibility").and_then(|v| v.as_str()).unwrap();
            assert_eq!(
                vis, "private_hidden",
                "entry {id} rendered as {vis}, expected private_hidden"
            );
        }
    }

    #[test]
    fn no_public_entry_accidentally_became_hidden() {
        // Ratchet: expect exactly N private_hidden entries (currently 4).
        // Bumping this number is a deliberate act; a silent bump is
        // probably a bug (e.g. someone added a project id that
        // accidentally overlaps with the hidden list). This test forces
        // the reviewer to acknowledge the count change.
        let cards = canonical_project_cards();
        let hidden_count = cards
            .iter()
            .filter(|c| {
                c.get("visibility").and_then(|v| v.as_str()) == Some("private_hidden")
            })
            .count();
        assert_eq!(
            hidden_count,
            PRIVATE_HIDDEN_IDS.len(),
            "expected exactly {} private_hidden cards (matching PRIVATE_HIDDEN_IDS.len()), \
             got {hidden_count}. If you added a new hidden project, update \
             PRIVATE_HIDDEN_IDS; if you added a project id that happens to overlap, \
             rename it.",
            PRIVATE_HIDDEN_IDS.len()
        );
    }
}
