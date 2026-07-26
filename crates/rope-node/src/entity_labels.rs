//! Entity labels and ecosystem taxonomy (Quipu Canon v1.2 / Rope Graph v1).
//!
//! This module is the server-side ground-truth registry that turns the
//! anonymous hex addresses on Datachain Rope into a navigable graph of
//! ECOSYSTEMS -> APPLICATIONS -> CONTRACTS / BOTS / ASSETS / AGENTS.
//!
//! It is consumed by the `rope_listStrings`, `rope_listEcosystems`,
//! `rope_listApplications`, `rope_listRelations` and `rope_resolveLabel`
//! RPC methods in `rpc_server.rs`. Until on-chain attestation of labels
//! lands (see open question 1 in the Rope Graph spec), this static
//! registry is the canonical source. Ecosystem operators contribute by
//! sending PRs against this file; everything is reproducible and
//! auditable in git history.
//!
//! Categories and the field taxonomy match exactly what the
//! `event.datachain.one` Rope Graph hero panel needs: a `String` shape
//! with `kind`, `parent_string_id`, `ecosystem_id`, `child_count`,
//! `descendant_count`, and a human-readable `labels` block.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};

/// What a string represents in the Rope Graph nested taxonomy.
///
/// This is a SUPERSET of `rope_core::personal_ledger::StringKind`. The
/// canon-level kinds are the on-chain primitives; the labels here add
/// derived/synthetic kinds (`bot`, `application`, `ecosystem`, `agent`,
/// `validator`, `oracle`) so frontends can render the full topology
/// without inventing client-side heuristics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LabelKind {
    Wallet,
    Contract,
    Bot,
    Application,
    Ecosystem,
    Asset,
    Did,
    Agent,
    Validator,
    Oracle,
    Organization,
    Partner,
    Cord,
}

impl LabelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LabelKind::Wallet => "wallet",
            LabelKind::Contract => "contract",
            LabelKind::Bot => "bot",
            LabelKind::Application => "application",
            LabelKind::Ecosystem => "ecosystem",
            LabelKind::Asset => "asset",
            LabelKind::Did => "did",
            LabelKind::Agent => "agent",
            LabelKind::Validator => "validator",
            LabelKind::Oracle => "oracle",
            LabelKind::Organization => "organization",
            LabelKind::Partner => "partner",
            LabelKind::Cord => "cord",
        }
    }

    /// Parse from the wire form. Accepts the canonical kebab/lowercase
    /// names and the obvious plural shortcuts. Unknown strings -> None.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "wallet" | "wallets" | "eoa" => Some(LabelKind::Wallet),
            "contract" | "contracts" => Some(LabelKind::Contract),
            "bot" | "bots" => Some(LabelKind::Bot),
            "application" | "applications" | "app" | "apps" => Some(LabelKind::Application),
            "ecosystem" | "ecosystems" | "platform" => Some(LabelKind::Ecosystem),
            "asset" | "assets" | "rwa" => Some(LabelKind::Asset),
            "did" | "dids" | "identity" => Some(LabelKind::Did),
            "agent" | "agents" | "ai" => Some(LabelKind::Agent),
            "validator" | "validators" | "knot-witness" => Some(LabelKind::Validator),
            "oracle" | "oracles" => Some(LabelKind::Oracle),
            "organization" | "organizations" | "org" | "orgs" => Some(LabelKind::Organization),
            "partner" | "partners" => Some(LabelKind::Partner),
            "cord" => Some(LabelKind::Cord),
            _ => None,
        }
    }
}

/// Static, server-side label for a single string id (lowercased hex).
///
/// All fields are `Copy`, so callers can safely cache an owned snapshot
/// from the live registry without borrowing the underlying `Arc`.
#[derive(Clone, Copy, Debug)]
pub struct EntityLabel {
    /// Lowercased hex of the string id (no `0x` prefix). For wallets and
    /// contracts this is the 20-byte EVM address; for assets it is the
    /// keccak256 of the canonical asset URI; for the cord it is 32 zero
    /// bytes; for ecosystems and applications it is a synthetic id (see
    /// [`synthetic_id`]).
    pub id_hex: &'static str,
    pub kind: LabelKind,
    pub display_name: &'static str,
    pub short_name: &'static str,
    pub description: &'static str,
    /// Slug matching the frontend `PLATFORMS` palette
    /// (`dcswap`, `tanastok`, `naturaproof`, `datawalletplus`,
    /// `careaway`, `syndicated`, `foundation`).
    pub platform: &'static str,
    /// Free-form classification: `router`, `pool`, `bot`, `oracle`,
    /// `factory`, `treasury`, `issuer`, `claim_issuer`, `compliance`,
    /// `identity_registry`, `validator`, `agent`, `cord`, ...
    pub role: &'static str,
    /// `id_hex` of the parent string (an application's parent is its
    /// ecosystem; a contract or bot's parent is its application).
    pub parent: Option<&'static str>,
    /// `id_hex` of the root ecosystem ancestor for this entity. Usually
    /// `Some(synthetic_id("ecosystem", platform))` for everything except
    /// the ecosystem itself.
    pub ecosystem: Option<&'static str>,
    /// True when this label is signed by an ecosystem operator (today
    /// always true for static entries — they ship in the binary). When
    /// on-chain attestation lands this becomes a real verification step.
    pub verified: bool,
    /// Address of the verifier (ecosystem operator). Cosmetic for now.
    pub verifier: Option<&'static str>,
    /// FontAwesome icon shorthand (e.g. `fa-water`, `fa-robot`).
    pub icon: &'static str,
    /// When true, the raw id is permanently redacted from any label
    /// response — only the human-readable name is exposed. Mirrors the
    /// `hidden` flag in `rope-explorer`.
    pub hidden: bool,
}

/// Globally-accessible label registry — **built-in only**.
///
/// This stays static so callers who hold a `&'static LabelRegistry` reference
/// keep working unchanged. Most RPC consumers should now prefer
/// [`current`] which also includes the live Tanastok manifest overlay
/// installed by `crate::entity_manifest`.
pub fn registry() -> &'static LabelRegistry {
    static R: OnceLock<LabelRegistry> = OnceLock::new();
    R.get_or_init(LabelRegistry::built_in)
}

/// Convenience: lookup by lowercased hex without `0x` prefix.
///
/// Looks in the **built-in** registry only. Use [`lookup`] for the
/// live merged view (built-ins + Tanastok manifest + future ecosystem
/// manifests).
pub fn get_label(id_hex: &str) -> Option<&'static EntityLabel> {
    registry().get(id_hex)
}

/// Live merged-view lookup. Returns an owned copy of the label so the
/// caller doesn't need to keep the `Arc<LabelRegistry>` alive itself.
/// Falls back to the built-in registry if the manifest hasn't loaded
/// (or is disabled via `ROPE_DISABLE_ENTITY_MANIFEST`).
pub fn lookup(id_hex: &str) -> Option<EntityLabel> {
    current().get(id_hex).copied().or_else(|| registry().get(id_hex).copied())
}

// ============================================================================
// Live overlay (Quipu Canon v1.2 — Phase 5 / SPEC §4.1 Option B)
//
// `entity_manifest` (sibling module) periodically pulls
// https://tanastok.io/api/v1/tanastok-entity-manifest, converts the
// response into [`EntityLabel`] structs (using the [`intern`] helper to
// turn dynamic `String`s into safe `&'static str`s) and calls
// [`install_current`] to atomically swap a fresh merged registry into a
// global `RwLock<Arc<LabelRegistry>>`. Reads use [`current`] which
// returns an `Arc` clone — readers never block writers and vice versa.
// ============================================================================

/// Lock-protected slot for the merged (built-in + manifest) registry.
fn current_slot() -> &'static RwLock<Arc<LabelRegistry>> {
    static SLOT: OnceLock<RwLock<Arc<LabelRegistry>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Arc::new(LabelRegistry::built_in())))
}

/// Atomic snapshot of the live label registry. Cheap clone (`Arc`).
///
/// The returned registry contains:
///
/// 1. Every built-in entry (compile-time `BUILT_IN_*`).
/// 2. Every entry installed via the most recent successful
///    `entity_manifest` refresh (Tanastok today; DCSwap, NaturaProof,
///    Datawallet+ tomorrow).
///
/// If the manifest fetcher has never run successfully, this is identical
/// to the built-in `registry()`.
pub fn current() -> Arc<LabelRegistry> {
    Arc::clone(&current_slot().read().expect("entity_labels current slot poisoned"))
}

/// Atomically replace the live merged registry. Invoked by
/// `crate::entity_manifest::apply_response` after a successful fetch.
pub fn install_current(registry: LabelRegistry) {
    let mut slot = current_slot().write().expect("entity_labels current slot poisoned");
    *slot = Arc::new(registry);
}

/// Builder used by `entity_manifest`: starts with the built-in topology
/// and lets the caller insert dynamic entries before installing.
pub fn fresh_with_builtins() -> LabelRegistry {
    LabelRegistry::built_in()
}

// ============================================================================
// String interning — turns `String` into `&'static str` for the registry.
//
// Manifest entries arrive as owned `String`s but the existing
// [`EntityLabel`] struct is `&'static str`-typed (kept that way so the
// ~1300 lines of `BUILT_IN_*` literals don't need refactoring). We
// intern each unique string once via `Box::leak`, dedup-keyed in the
// `INTERNED` set so refreshes that re-emit the same strings reuse the
// existing leak.
//
// Bound on memory: Tanastok currently ships 1,626 entities × ~6 string
// fields ≈ 10K unique strings × ~50 bytes ≈ ~500 KB total. Growth over
// the launch year is bounded by the number of *new* entity IDs and
// names, not by refresh frequency.
// ============================================================================

/// Intern a string: returns a `&'static str` that is byte-equal to the
/// input. The first call for a given string `Box::leak`s it; subsequent
/// calls reuse the leaked allocation. Concurrent-safe under a `RwLock`.
pub fn intern(s: &str) -> &'static str {
    static INTERNED: OnceLock<RwLock<HashSet<&'static str>>> = OnceLock::new();
    let lock = INTERNED.get_or_init(|| RwLock::new(HashSet::new()));

    // Fast path — string already interned.
    if let Some(existing) = lock
        .read()
        .expect("intern set poisoned")
        .get(s)
        .copied()
    {
        return existing;
    }

    // Slow path — promote to a leaked &'static and insert. Re-check
    // under the write lock to avoid a race where two threads leak the
    // same string twice.
    let mut w = lock.write().expect("intern set poisoned");
    if let Some(existing) = w.get(s).copied() {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    w.insert(leaked);
    leaked
}

/// Optional intern for a string-or-none.
pub fn intern_opt(s: Option<&str>) -> Option<&'static str> {
    s.map(intern)
}

/// Indexed view over [`EntityLabel`]s. Built once at startup; lock-free
/// for reads. Frontend RPC paths must never block on this.
pub struct LabelRegistry {
    by_id: HashMap<String, EntityLabel>,
    by_platform: HashMap<&'static str, Vec<&'static str>>,
    by_kind: HashMap<LabelKind, Vec<&'static str>>,
    by_parent: HashMap<&'static str, Vec<&'static str>>,
    by_ecosystem: HashMap<&'static str, Vec<&'static str>>,
}

impl LabelRegistry {
    fn empty() -> Self {
        Self {
            by_id: HashMap::new(),
            by_platform: HashMap::new(),
            by_kind: HashMap::new(),
            by_parent: HashMap::new(),
            by_ecosystem: HashMap::new(),
        }
    }

    /// Insert (or overwrite) one entity label. `pub(crate)` so the
    /// `entity_manifest` loader can populate the live overlay.
    pub(crate) fn insert(&mut self, label: EntityLabel) {
        let id = label.id_hex.to_string();
        self.by_platform
            .entry(label.platform)
            .or_default()
            .push(label.id_hex);
        self.by_kind.entry(label.kind).or_default().push(label.id_hex);
        if let Some(p) = label.parent {
            self.by_parent.entry(p).or_default().push(label.id_hex);
        }
        if let Some(e) = label.ecosystem {
            self.by_ecosystem.entry(e).or_default().push(label.id_hex);
        }
        self.by_id.insert(id, label);
    }

    pub fn get(&self, id_hex: &str) -> Option<&EntityLabel> {
        let key = id_hex
            .trim_start_matches("0x")
            .to_ascii_lowercase();
        self.by_id.get(&key)
    }

    pub fn list_by_kind(&self, kind: LabelKind) -> Vec<&EntityLabel> {
        self.by_kind
            .get(&kind)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.by_id.get(*id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    pub fn list_by_platform(&self, platform: &str) -> Vec<&EntityLabel> {
        self.by_platform
            .get(platform)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.by_id.get(*id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    pub fn children_of(&self, parent_id_hex: &str) -> Vec<&EntityLabel> {
        let key = parent_id_hex
            .trim_start_matches("0x")
            .to_ascii_lowercase();
        self.by_parent
            .iter()
            .filter(|(p, _)| p.eq_ignore_ascii_case(&key))
            .flat_map(|(_, ids)| ids.iter().filter_map(|id| self.by_id.get(*id)))
            .collect()
    }

    /// Direct children of a given parent id (synthetic or hex).
    pub fn child_count_of(&self, parent_id_hex: &str) -> usize {
        let key = parent_id_hex
            .trim_start_matches("0x")
            .to_ascii_lowercase();
        self.by_parent
            .iter()
            .filter(|(p, _)| p.eq_ignore_ascii_case(&key))
            .map(|(_, ids)| ids.len())
            .sum()
    }

    /// Recursive subtree size from a given id (excluding the id itself).
    pub fn descendant_count_of(&self, parent_id_hex: &str) -> usize {
        let mut total = 0usize;
        let direct = self.children_of(parent_id_hex);
        total += direct.len();
        for c in &direct {
            total += self.descendant_count_of(c.id_hex);
        }
        total
    }

    /// All ecosystems registered.
    pub fn ecosystems(&self) -> Vec<&EntityLabel> {
        self.list_by_kind(LabelKind::Ecosystem)
    }

    /// Iterate over every label.
    pub fn all(&self) -> Vec<&EntityLabel> {
        self.by_id.values().collect()
    }

    /// Substring/prefix search across display_name, short_name, and
    /// id_hex. Case-insensitive. Used by `rope_resolveLabel`.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&EntityLabel> {
        let q = query.trim().trim_start_matches("0x").to_ascii_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<&EntityLabel> = self
            .by_id
            .values()
            .filter(|l| {
                l.id_hex.contains(&q)
                    || l.display_name.to_ascii_lowercase().contains(&q)
                    || l.short_name.to_ascii_lowercase().contains(&q)
                    || l.platform.to_ascii_lowercase().contains(&q)
                    || l.role.to_ascii_lowercase().contains(&q)
            })
            .collect();
        // Prefer exact id_hex matches, then prefix matches on display_name.
        hits.sort_by_key(|l| {
            let id_match = if l.id_hex == q { 0 } else { 1 };
            let name_prefix = if l.display_name.to_ascii_lowercase().starts_with(&q) {
                0
            } else {
                1
            };
            (id_match, name_prefix, l.display_name.len())
        });
        hits.truncate(limit);
        hits
    }

    pub fn ecosystem_of(&self, id_hex: &str) -> Option<&'static str> {
        self.get(id_hex).and_then(|l| l.ecosystem.or(if l.kind == LabelKind::Ecosystem { Some(l.id_hex) } else { None }))
    }

    pub fn parent_of(&self, id_hex: &str) -> Option<&'static str> {
        self.get(id_hex).and_then(|l| l.parent)
    }

    /// Aggregate counts by `(platform, kind)`. Useful for ecosystem
    /// summary cards.
    pub fn platform_breakdown(&self) -> BTreeMap<String, BTreeMap<&'static str, usize>> {
        let mut out: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
        for l in self.by_id.values() {
            out.entry(l.platform.to_string())
                .or_default()
                .entry(l.kind.as_str())
                .and_modify(|n| *n += 1)
                .or_insert(1);
        }
        out
    }

    /// Built-in ecosystem topology shipped in the binary.
    ///
    /// Ecosystem operators contribute by sending PRs against this
    /// function. The id field for ecosystems and applications is
    /// synthetic (deterministic 32-byte BLAKE3 hash of a canonical URI)
    /// so the frontend can render them as "real" strings even before
    /// any on-chain string of those kinds is created.
    fn built_in() -> Self {
        let mut r = LabelRegistry::empty();

        // ============================================================
        // ECOSYSTEMS — top-level brand strings (synthetic ids)
        // ============================================================
        for eco in BUILT_IN_ECOSYSTEMS {
            r.insert(*eco);
        }

        // ============================================================
        // APPLICATIONS — logical apps inside an ecosystem
        // ============================================================
        for app in BUILT_IN_APPLICATIONS {
            r.insert(*app);
        }

        // ============================================================
        // CORE CONTRACTS — DCSwap, T-REX, ONCHAINID, bridges, tokens
        // ============================================================
        for c in BUILT_IN_CONTRACTS {
            r.insert(*c);
        }

        // ============================================================
        // POOLS — DCSwap AMM pools (assets within DCSwap AMM)
        // ============================================================
        for p in BUILT_IN_POOLS {
            r.insert(*p);
        }

        // ============================================================
        // BOTS — DCSwap multi-strategy bot fleet (62 wallets)
        //
        // Real per-wallet labels live in /etc/rope/bots.toml; this is
        // the deterministic fallback used when that file is absent.
        // ============================================================
        for b in BUILT_IN_BOTS {
            r.insert(*b);
        }

        // ============================================================
        // CANONICAL AI AGENTS (5 production agents per
        // handover-canonical-agents-live-from-rope-2026-05-05.mdc)
        // ============================================================
        for a in BUILT_IN_AGENTS {
            r.insert(*a);
        }

        // ============================================================
        // CORD — the global federation cord
        // ============================================================
        r.insert(CORD_LABEL);

        r
    }
}

// ============================================================================
// Synthetic ID helpers
// ============================================================================
//
// Synthetic ids let us render ecosystems and applications as first-class
// strings in `rope_listStrings` even before any on-chain canon string of
// those kinds exists. They are 32-byte hex prefixes derived
// deterministically from a canonical URI:
//
//   ecosystem://dcswap                  -> 0xdcec...0001 (synthetic_id)
//   application://dcswap/amm-v1         -> 0xdcec...0002
//
// The leading byte 0xdc identifies the synthetic scheme so they cannot
// collide with real 20-byte EVM addresses (which never have this
// prefix combination by chance) and 0xec encodes "ecosystem-class".

/// Synthetic id prefix for ecosystems (`dcec...`).
#[allow(dead_code)]
pub const SYN_ECO_PREFIX: &str = "dcec";
/// Synthetic id prefix for applications (`dcab...`).
#[allow(dead_code)]
pub const SYN_APP_PREFIX: &str = "dcab";

// ============================================================================
// Built-in ecosystem topology (Rope Graph hero panel — June 2026 launch)
// ============================================================================
//
// Ids without `0x` prefix; lowercase hex; 20 bytes for real contracts and
// wallets, 32 bytes for synthetic ecosystem/application ids (prefix
// `dcec` or `dcab`).

const BUILT_IN_ECOSYSTEMS: &[EntityLabel] = &[
    EntityLabel {
        id_hex: "dcec00000000000000000000000000000000000000000000000000000000dc01",
        kind: LabelKind::Ecosystem,
        display_name: "DCSwap",
        short_name: "DCSwap",
        description: "Decentralised AMM and bridge — primary DC FAT liquidity venue",
        platform: "dcswap",
        role: "ecosystem",
        parent: None,
        ecosystem: None,
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-arrow-right-arrow-left",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcec00000000000000000000000000000000000000000000000000000000dc02",
        kind: LabelKind::Ecosystem,
        display_name: "Tanastok",
        short_name: "Tanastok",
        description: "Real-world-asset tokenisation — DCNFT title deeds and ERC-3643 securities",
        platform: "tanastok",
        role: "ecosystem",
        parent: None,
        ecosystem: None,
        verified: true,
        verifier: Some("0x297ba821da55ed5e37c5c25b3832ce45fc54c475"),
        icon: "fa-landmark",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcec00000000000000000000000000000000000000000000000000000000dc03",
        kind: LabelKind::Ecosystem,
        display_name: "NaturaProof",
        short_name: "NaturaProof",
        description: "Biodiversity verification — field measurement and certificate issuance",
        platform: "naturaproof",
        role: "ecosystem",
        parent: None,
        ecosystem: None,
        verified: true,
        verifier: None,
        icon: "fa-leaf",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcec00000000000000000000000000000000000000000000000000000000dc04",
        kind: LabelKind::Ecosystem,
        display_name: "Datawallet+",
        short_name: "Datawallet+",
        description: "Sovereign identity and personal-ledger wallet (Datawallet ReactNative)",
        platform: "datawalletplus",
        role: "ecosystem",
        parent: None,
        ecosystem: None,
        verified: true,
        verifier: None,
        icon: "fa-id-badge",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcec00000000000000000000000000000000000000000000000000000000dc05",
        kind: LabelKind::Ecosystem,
        display_name: "Datachain Foundation",
        short_name: "Foundation",
        description: "Core protocol, governance, treasury, and canonical AI agents",
        platform: "foundation",
        role: "ecosystem",
        parent: None,
        ecosystem: None,
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-building-columns",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcec00000000000000000000000000000000000000000000000000000000dc06",
        kind: LabelKind::Ecosystem,
        display_name: "Careaway",
        short_name: "Careaway",
        description: "Care-plan tokenisation and parametric attestations",
        platform: "careaway",
        role: "ecosystem",
        parent: None,
        ecosystem: None,
        verified: true,
        verifier: None,
        icon: "fa-heart-pulse",
        hidden: false,
    },
];

// ----- Synthetic ecosystem ids (re-exported for parent linkage) -----
/// Synthetic id of the DCSwap ecosystem.
pub const ECO_DCSWAP: &str =
    "dcec00000000000000000000000000000000000000000000000000000000dc01";
/// Synthetic id of the Tanastok ecosystem.
pub const ECO_TANASTOK: &str =
    "dcec00000000000000000000000000000000000000000000000000000000dc02";
/// Synthetic id of the NaturaProof ecosystem.
#[allow(dead_code)]
pub const ECO_NATURAPROOF: &str =
    "dcec00000000000000000000000000000000000000000000000000000000dc03";
/// Synthetic id of the Datawallet+ ecosystem.
pub const ECO_DATAWALLET: &str =
    "dcec00000000000000000000000000000000000000000000000000000000dc04";
/// Synthetic id of the Datachain Foundation ecosystem.
pub const ECO_FOUNDATION: &str =
    "dcec00000000000000000000000000000000000000000000000000000000dc05";
/// Synthetic id of the Careaway ecosystem.
#[allow(dead_code)]
pub const ECO_CAREAWAY: &str =
    "dcec00000000000000000000000000000000000000000000000000000000dc06";

// ============================================================================
// APPLICATIONS — logical apps inside an ecosystem
// ============================================================================

const APP_DCSWAP_AMM: &str = "dcab00000000000000000000000000000000000000000000000000000000ab01";
const APP_DCSWAP_BOTS: &str = "dcab00000000000000000000000000000000000000000000000000000000ab02";
const APP_DCSWAP_BRIDGE: &str = "dcab00000000000000000000000000000000000000000000000000000000ab03";
const APP_TANASTOK_ISSUANCE: &str = "dcab00000000000000000000000000000000000000000000000000000000ab04";
const APP_TANASTOK_COMPLIANCE: &str = "dcab00000000000000000000000000000000000000000000000000000000ab05";
const APP_DATAWALLET_IDENTITY: &str = "dcab00000000000000000000000000000000000000000000000000000000ab06";
const APP_FOUNDATION_AGENTS: &str = "dcab00000000000000000000000000000000000000000000000000000000ab07";
const APP_FOUNDATION_TREASURY: &str = "dcab00000000000000000000000000000000000000000000000000000000ab08";
const APP_FOUNDATION_GOVERNANCE: &str =
    "dcab00000000000000000000000000000000000000000000000000000000ab09";

const BUILT_IN_APPLICATIONS: &[EntityLabel] = &[
    // ---- DCSwap ----
    EntityLabel {
        id_hex: APP_DCSWAP_AMM,
        kind: LabelKind::Application,
        display_name: "DCSwap AMM v1",
        short_name: "AMM",
        description: "Constant-product zero-fee AMM (FAT pairs) and stable USDC/USDT pool",
        platform: "dcswap",
        role: "amm",
        parent: Some(ECO_DCSWAP),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-water",
        hidden: false,
    },
    EntityLabel {
        id_hex: APP_DCSWAP_BOTS,
        kind: LabelKind::Application,
        display_name: "DCSwap Multi-Strategy Bot Fleet",
        short_name: "Bots",
        description: "62-wallet HD-derived market-making, arbitrage, and retail-sim bots",
        platform: "dcswap",
        role: "bot-fleet",
        parent: Some(ECO_DCSWAP),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-robot",
        hidden: false,
    },
    EntityLabel {
        id_hex: APP_DCSWAP_BRIDGE,
        kind: LabelKind::Application,
        display_name: "DCSwap Bridge",
        short_name: "Bridge",
        description: "Cross-chain bridged tokens (USDC, USDT, EUROD, WFAT)",
        platform: "dcswap",
        role: "bridge",
        parent: Some(ECO_DCSWAP),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: None,
        icon: "fa-arrow-right-arrow-left",
        hidden: false,
    },
    // ---- Tanastok ----
    EntityLabel {
        id_hex: APP_TANASTOK_ISSUANCE,
        kind: LabelKind::Application,
        display_name: "Tanastok ERC-3643 Issuance",
        short_name: "Issuance",
        description: "T-REX security-token issuance pipeline backing real-world assets",
        platform: "tanastok",
        role: "issuance",
        parent: Some(ECO_TANASTOK),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: Some("0x297ba821da55ed5e37c5c25b3832ce45fc54c475"),
        icon: "fa-stamp",
        hidden: false,
    },
    EntityLabel {
        id_hex: APP_TANASTOK_COMPLIANCE,
        kind: LabelKind::Application,
        display_name: "Tanastok Compliance",
        short_name: "Compliance",
        description: "ONCHAINID claims, identity registry, and ROPE compliance modules",
        platform: "tanastok",
        role: "compliance",
        parent: Some(ECO_TANASTOK),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-gavel",
        hidden: false,
    },
    // ---- Datawallet+ ----
    EntityLabel {
        id_hex: APP_DATAWALLET_IDENTITY,
        kind: LabelKind::Application,
        display_name: "Datawallet+ Identity",
        short_name: "Identity",
        description: "ONCHAINID-backed sovereign-identity wallet",
        platform: "datawalletplus",
        role: "identity",
        parent: Some(ECO_DATAWALLET),
        ecosystem: Some(ECO_DATAWALLET),
        verified: true,
        verifier: None,
        icon: "fa-fingerprint",
        hidden: false,
    },
    // ---- Foundation ----
    EntityLabel {
        id_hex: APP_FOUNDATION_AGENTS,
        kind: LabelKind::Application,
        display_name: "Canonical AI Agents",
        short_name: "Agents",
        description: "5 production agents (Semantic, Oracle, Insurance, Validation, Compliance)",
        platform: "foundation",
        role: "agent-framework",
        parent: Some(ECO_FOUNDATION),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-microchip",
        hidden: false,
    },
    EntityLabel {
        id_hex: APP_FOUNDATION_TREASURY,
        kind: LabelKind::Application,
        display_name: "DC Treasury",
        short_name: "Treasury",
        description: "Foundation deployer + treasury wallet",
        platform: "foundation",
        role: "treasury",
        parent: Some(ECO_FOUNDATION),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-vault",
        hidden: false,
    },
    EntityLabel {
        id_hex: APP_FOUNDATION_GOVERNANCE,
        kind: LabelKind::Application,
        display_name: "Master-Node Governance",
        short_name: "Governance",
        description: "Master/member node registry, founder DID, governance log",
        platform: "foundation",
        role: "governance",
        parent: Some(ECO_FOUNDATION),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-shield-halved",
        hidden: false,
    },
];

// ============================================================================
// CORE CONTRACTS
// ============================================================================
const BUILT_IN_CONTRACTS: &[EntityLabel] = &[
    // ---- DCSwap (post-Reth migration addresses) ----
    EntityLabel {
        id_hex: "285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4",
        kind: LabelKind::Contract,
        display_name: "WFAT (Wrapped DC FAT)",
        short_name: "WFAT",
        description: "Wrapped native FAT — DCR-20 wrapper used by DCSwap pools",
        platform: "dcswap",
        role: "token",
        parent: Some(APP_DCSWAP_AMM),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-coins",
        hidden: false,
    },
    EntityLabel {
        id_hex: "b93bd8db94f1baff474aa9cba0739daaad01641f",
        kind: LabelKind::Contract,
        display_name: "USDC (bridged)",
        short_name: "USDC",
        description: "Bridged USD Coin — 6 decimals",
        platform: "dcswap",
        role: "token",
        parent: Some(APP_DCSWAP_BRIDGE),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: None,
        icon: "fa-dollar-sign",
        hidden: false,
    },
    EntityLabel {
        id_hex: "79a26132f48394421382c13b54ae77fa3af73289",
        kind: LabelKind::Contract,
        display_name: "USDT (bridged)",
        short_name: "USDT",
        description: "Bridged Tether — 6 decimals",
        platform: "dcswap",
        role: "token",
        parent: Some(APP_DCSWAP_BRIDGE),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: None,
        icon: "fa-dollar-sign",
        hidden: false,
    },
    EntityLabel {
        id_hex: "24d6137807fa8a592888726d87ac748d018c6d4a",
        kind: LabelKind::Contract,
        display_name: "EUROD",
        short_name: "EUROD",
        description: "Euro stablecoin — 6 decimals",
        platform: "dcswap",
        role: "token",
        parent: Some(APP_DCSWAP_BRIDGE),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: None,
        icon: "fa-euro-sign",
        hidden: false,
    },
    EntityLabel {
        id_hex: "772e5fd559069aecce5e6983c0c415c8579d780d",
        kind: LabelKind::Contract,
        display_name: "DCSwap Factory",
        short_name: "Factory",
        description: "Pair factory — deploys CREATE2 pools",
        platform: "dcswap",
        role: "factory",
        parent: Some(APP_DCSWAP_AMM),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-industry",
        hidden: false,
    },
    EntityLabel {
        id_hex: "8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4",
        kind: LabelKind::Contract,
        display_name: "DCSwap Router",
        short_name: "Router",
        description: "Primary swap and add/removeLiquidity entry point",
        platform: "dcswap",
        role: "router",
        parent: Some(APP_DCSWAP_AMM),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-arrow-right-arrow-left",
        hidden: false,
    },
    EntityLabel {
        id_hex: "c2eeb0100aa7e81a3193bdce6733ff767f3bb93a",
        kind: LabelKind::Contract,
        display_name: "Multicall3",
        short_name: "Multicall3",
        description: "Bulk view-call aggregator (interop infra)",
        platform: "dcswap",
        role: "infrastructure",
        parent: Some(APP_DCSWAP_AMM),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: None,
        icon: "fa-layer-group",
        hidden: false,
    },
    // ---- T-REX / ONCHAINID (Tanastok) ----
    EntityLabel {
        id_hex: "76b40d5439f1cb661b2479fd15410662a7fe0991",
        kind: LabelKind::Contract,
        display_name: "T-REX Factory (Tanastok)",
        short_name: "T-REX Factory",
        description: "Per-asset T-REX suite deployer",
        platform: "tanastok",
        role: "factory",
        parent: Some(APP_TANASTOK_ISSUANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: Some("0x297ba821da55ed5e37c5c25b3832ce45fc54c475"),
        icon: "fa-industry",
        hidden: false,
    },
    EntityLabel {
        id_hex: "3065138f0ce815eb09f14d2e87e8bcbe98dd172b",
        kind: LabelKind::Contract,
        display_name: "ONCHAINID Identity Registry",
        short_name: "Identity Registry",
        description: "ERC-734/735 identity registry (claims, keys)",
        platform: "tanastok",
        role: "identity_registry",
        parent: Some(APP_TANASTOK_COMPLIANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-id-card",
        hidden: false,
    },
    EntityLabel {
        id_hex: "98a7ec2f86cfe4721dff36c648396f1f5ba11ab0",
        kind: LabelKind::Contract,
        display_name: "ONCHAINID Claim Topics",
        short_name: "Claim Topics",
        description: "ERC-3643 claim-topic registry",
        platform: "tanastok",
        role: "claim_topics",
        parent: Some(APP_TANASTOK_COMPLIANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-list-check",
        hidden: false,
    },
    EntityLabel {
        id_hex: "42d605a05a063d91e83481867839bfd713d21666",
        kind: LabelKind::Contract,
        display_name: "ONCHAINID Trusted Issuers",
        short_name: "Trusted Issuers",
        description: "Whitelisted ERC-3643 claim issuers",
        platform: "tanastok",
        role: "trusted_issuers",
        parent: Some(APP_TANASTOK_COMPLIANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-shield-halved",
        hidden: false,
    },
    EntityLabel {
        id_hex: "4f4741f3cbeafd9b4ab92b549ce6f49c426bcb03",
        kind: LabelKind::Contract,
        display_name: "ONCHAINID Identity Storage",
        short_name: "Identity Storage",
        description: "Per-DID storage backing the identity registry",
        platform: "tanastok",
        role: "identity_storage",
        parent: Some(APP_TANASTOK_COMPLIANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-database",
        hidden: false,
    },
    EntityLabel {
        id_hex: "e5156df30ed0645a585cb8207caa93d8d3847417",
        kind: LabelKind::Contract,
        display_name: "Datawallet+ Claim Issuer",
        short_name: "Claim Issuer",
        description: "Issues ERC-3643 verification claims for Datawallet+ users",
        platform: "datawalletplus",
        role: "claim_issuer",
        parent: Some(APP_DATAWALLET_IDENTITY),
        ecosystem: Some(ECO_DATAWALLET),
        verified: true,
        verifier: None,
        icon: "fa-certificate",
        hidden: false,
    },
    EntityLabel {
        id_hex: "0919baf7e91785ae65351698a04b07bb13d14bbc",
        kind: LabelKind::Contract,
        display_name: "ROPE Compliance Module",
        short_name: "Compliance Module",
        description: "Modular compliance wired into every Tanastok ERC-3643 token",
        platform: "tanastok",
        role: "compliance",
        parent: Some(APP_TANASTOK_COMPLIANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-gavel",
        hidden: false,
    },
    EntityLabel {
        id_hex: "d28cf001910d814c578e773efcbf0459d98db15f",
        kind: LabelKind::Contract,
        display_name: "Tanastok ONCHAINID",
        short_name: "Tanastok DID",
        description: "Tanastok issuer's ONCHAINID",
        platform: "tanastok",
        role: "did",
        parent: Some(APP_TANASTOK_ISSUANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-fingerprint",
        hidden: false,
    },
    EntityLabel {
        id_hex: "30fec506029781ba7d1d2ea27bdf9be422af81a7",
        kind: LabelKind::Contract,
        display_name: "Deployer ONCHAINID",
        short_name: "Deployer DID",
        description: "Foundation deployer's ONCHAINID",
        platform: "foundation",
        role: "did",
        parent: Some(APP_FOUNDATION_TREASURY),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-fingerprint",
        hidden: false,
    },
    EntityLabel {
        id_hex: "183c0666bfcfdab9453c0d48c0d39d511b4010b3",
        kind: LabelKind::Contract,
        display_name: "DCNFT Bytecode Template",
        short_name: "DCNFT Template",
        description: "ERC-721 title-deed implementation cloned per asset",
        platform: "tanastok",
        role: "template",
        parent: Some(APP_TANASTOK_ISSUANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-file-code",
        hidden: false,
    },
    EntityLabel {
        id_hex: "0264e76755493caf8f6eae214df188f2b9f6bbe2",
        kind: LabelKind::Contract,
        display_name: "T-REX Implementation Authority",
        short_name: "T-REX IA",
        description: "Upgrade authority for T-REX proxy implementations",
        platform: "tanastok",
        role: "implementation_authority",
        parent: Some(APP_TANASTOK_ISSUANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-key",
        hidden: false,
    },
    EntityLabel {
        id_hex: "bd3d7372caf8e448c6a3457561cc1c5de08bf1ef",
        kind: LabelKind::Contract,
        display_name: "T-REX IA Factory",
        short_name: "IA Factory",
        description: "Implementation Authority deployer",
        platform: "tanastok",
        role: "factory",
        parent: Some(APP_TANASTOK_ISSUANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: None,
        icon: "fa-industry",
        hidden: false,
    },
    EntityLabel {
        id_hex: "297ba821da55ed5e37c5c25b3832ce45fc54c475",
        kind: LabelKind::Wallet,
        display_name: "Tanastok Issuer",
        short_name: "Issuer",
        description: "Tanastok deployer wallet — owns per-asset T-REX suites",
        platform: "tanastok",
        role: "issuer",
        parent: Some(APP_TANASTOK_ISSUANCE),
        ecosystem: Some(ECO_TANASTOK),
        verified: true,
        verifier: Some("0x297ba821da55ed5e37c5c25b3832ce45fc54c475"),
        icon: "fa-stamp",
        hidden: false,
    },
    // ---- Foundation deployer wallet ----
    EntityLabel {
        id_hex: "60fb32ef3a2381c2ed71613f34fd56d56fcf4195",
        kind: LabelKind::Wallet,
        display_name: "DC Treasury / Foundation Deployer",
        short_name: "Treasury",
        description: "Datachain Foundation deployer + treasury wallet",
        platform: "foundation",
        role: "treasury",
        parent: Some(APP_FOUNDATION_TREASURY),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-vault",
        hidden: true,
    },
    EntityLabel {
        id_hex: "302fa11a6e784dfa89f96942a919c09b45559676",
        kind: LabelKind::Wallet,
        display_name: "Genesis",
        short_name: "Genesis",
        description: "Reth dev-mode genesis account (testnet seed)",
        platform: "foundation",
        role: "genesis",
        parent: Some(APP_FOUNDATION_TREASURY),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: None,
        icon: "fa-cube",
        hidden: false,
    },
];

// ============================================================================
// POOLS (DCSwap AMM)
// ============================================================================
const BUILT_IN_POOLS: &[EntityLabel] = &[
    EntityLabel {
        id_hex: "d9ebc3da001618a3ae90481d33ae7ef85e130317",
        kind: LabelKind::Asset,
        display_name: "FAT/USDC Pool",
        short_name: "FAT/USDC",
        description: "Primary DC FAT - USDC zero-fee AMM pool",
        platform: "dcswap",
        role: "pool",
        parent: Some(APP_DCSWAP_AMM),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-water",
        hidden: false,
    },
    EntityLabel {
        id_hex: "644da44bcd5f453c593781dbe22dfd733e8d1441",
        kind: LabelKind::Asset,
        display_name: "FAT/USDT Pool",
        short_name: "FAT/USDT",
        description: "DC FAT - USDT zero-fee AMM pool",
        platform: "dcswap",
        role: "pool",
        parent: Some(APP_DCSWAP_AMM),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-water",
        hidden: false,
    },
    EntityLabel {
        id_hex: "1e9c2ccf67320459bc4999a9f8be4a063d4021e4",
        kind: LabelKind::Asset,
        display_name: "FAT/EUROD Pool",
        short_name: "FAT/EUROD",
        description: "DC FAT - EUROD zero-fee AMM pool",
        platform: "dcswap",
        role: "pool",
        parent: Some(APP_DCSWAP_AMM),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-water",
        hidden: false,
    },
    EntityLabel {
        id_hex: "b86bdcecad93573d6ca21313aa7eac52800513c8",
        kind: LabelKind::Asset,
        display_name: "USDC/USDT Pool",
        short_name: "USDC/USDT",
        description: "Stablecoin peg-keeping pool",
        platform: "dcswap",
        role: "pool",
        parent: Some(APP_DCSWAP_AMM),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-water",
        hidden: false,
    },
];

// ============================================================================
// BOTS — DCSwap multi-strategy bot fleet
// ============================================================================
//
// The bot fleet is HD-derived from a single mnemonic (62 wallets per
// `handover-dcswap-redeployed-2026-02-26.mdc`). Real wallet addresses
// rotate; we expose the canonical placeholder ids that the on-chain
// `Bot` strings will use once the DCSwap agent emits them.
//
// Each entry below carries the role classification (market_maker,
// cross_pair_arb, stable_trader, retail_sim, whale, scalper, momentum,
// lp_manager, dca) + an index range. The frontend renders these as
// strands grouped under the "DCSwap Multi-Strategy Bot Fleet"
// application string.

const BUILT_IN_BOTS: &[EntityLabel] = &[
    // The 9 strategy cohorts as application-style sub-strings. Real
    // per-wallet rows should be appended at startup from
    // /etc/rope/bots.toml when present (handled in registry::insert).
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab21",
        kind: LabelKind::Bot,
        display_name: "DCSwap MarketMaker Cohort (#0-4)",
        short_name: "MarketMaker",
        description: "Mean-reversion market makers, 0.5s-15s cycle",
        platform: "dcswap",
        role: "market_maker",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-balance-scale",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab22",
        kind: LabelKind::Bot,
        display_name: "DCSwap CrossPairArb Cohort (#5-9)",
        short_name: "CrossPairArb",
        description: "Cross-FAT-pair arbitrage, 2s-12s cycle",
        platform: "dcswap",
        role: "cross_pair_arb",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-shuffle",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab23",
        kind: LabelKind::Bot,
        display_name: "DCSwap StableTrader Cohort (#10-14)",
        short_name: "StableTrader",
        description: "USDC/USDT peg keeping, 5s-60s cycle",
        platform: "dcswap",
        role: "stable_trader",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-equals",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab24",
        kind: LabelKind::Bot,
        display_name: "DCSwap RetailSim Cohort (#15-34)",
        short_name: "RetailSim",
        description: "20-wallet retail behaviour simulation, 10s-300s cycle",
        platform: "dcswap",
        role: "retail_sim",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-users",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab25",
        kind: LabelKind::Bot,
        display_name: "DCSwap Whale Cohort (#35-37)",
        short_name: "Whale",
        description: "Large infrequent trades 500-5000 FAT, 30s-300s cycle",
        platform: "dcswap",
        role: "whale",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-fish",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab26",
        kind: LabelKind::Bot,
        display_name: "DCSwap Scalper Cohort (#38-45)",
        short_name: "Scalper",
        description: "Rapid-fire micro trades 0.1-5 FAT, 10ms-5s cycle",
        platform: "dcswap",
        role: "scalper",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-bolt",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab27",
        kind: LabelKind::Bot,
        display_name: "DCSwap Momentum Cohort (#46-51)",
        short_name: "Momentum",
        description: "Trend-following with price history, 5s-30s cycle",
        platform: "dcswap",
        role: "momentum",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-chart-line",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab28",
        kind: LabelKind::Bot,
        display_name: "DCSwap LPManager Cohort (#52-56)",
        short_name: "LPManager",
        description: "Adds and removes liquidity, 60s-600s cycle",
        platform: "dcswap",
        role: "lp_manager",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-faucet",
        hidden: false,
    },
    EntityLabel {
        id_hex: "dcab00000000000000000000000000000000000000000000000000bot00ab29",
        kind: LabelKind::Bot,
        display_name: "DCSwap DCA Cohort (#57-61)",
        short_name: "DCA",
        description: "Dollar-cost averaging periodic buys, 60s-600s cycle",
        platform: "dcswap",
        role: "dca",
        parent: Some(APP_DCSWAP_BOTS),
        ecosystem: Some(ECO_DCSWAP),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-clock",
        hidden: false,
    },
];

// ============================================================================
// CANONICAL AI AGENTS
// ============================================================================
const BUILT_IN_AGENTS: &[EntityLabel] = &[
    EntityLabel {
        id_hex: "00000000000000000000000000000000000000c1",
        kind: LabelKind::Agent,
        display_name: "SemanticAgent",
        short_name: "Semantic",
        description: "Tantivy full-text indexer for every knot, 10-min checkpoints",
        platform: "foundation",
        role: "semantic_search",
        parent: Some(APP_FOUNDATION_AGENTS),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-magnifying-glass",
        hidden: false,
    },
    EntityLabel {
        id_hex: "00000000000000000000000000000000000000c2",
        kind: LabelKind::Agent,
        display_name: "OracleAgent",
        short_name: "Oracle",
        description: "Pulls external feeds and attests price/data on-chain",
        platform: "foundation",
        role: "oracle",
        parent: Some(APP_FOUNDATION_AGENTS),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-satellite-dish",
        hidden: false,
    },
    EntityLabel {
        id_hex: "00000000000000000000000000000000000000c3",
        kind: LabelKind::Agent,
        display_name: "InsuranceAgent",
        short_name: "Insurance",
        description: "Polls Tanastok and NaturaProof feeds, issues parametric attestations",
        platform: "foundation",
        role: "insurance",
        parent: Some(APP_FOUNDATION_AGENTS),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-shield",
        hidden: false,
    },
    EntityLabel {
        id_hex: "00000000000000000000000000000000000000c4",
        kind: LabelKind::Agent,
        display_name: "ValidationAgent",
        short_name: "Validation",
        description: "Verifies hybrid Ed25519 plus Dilithium3 signatures on cord anchors",
        platform: "foundation",
        role: "validation",
        parent: Some(APP_FOUNDATION_AGENTS),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-check-double",
        hidden: false,
    },
    EntityLabel {
        id_hex: "00000000000000000000000000000000000000c5",
        kind: LabelKind::Agent,
        display_name: "ComplianceAgent",
        short_name: "Compliance",
        description: "GDPR Art.17, MiFID II event batching, DORA incident anchoring",
        platform: "foundation",
        role: "compliance",
        parent: Some(APP_FOUNDATION_AGENTS),
        ecosystem: Some(ECO_FOUNDATION),
        verified: true,
        verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
        icon: "fa-gavel",
        hidden: false,
    },
];

// ============================================================================
// CORD — global federation cord
// ============================================================================
const CORD_LABEL: EntityLabel = EntityLabel {
    id_hex: "0000000000000000000000000000000000000000000000000000000000000000",
    kind: LabelKind::Cord,
    display_name: "Federation Cord",
    short_name: "Cord",
    description: "Single global anchor cord — all knot anchors land here",
    platform: "foundation",
    role: "cord",
    parent: Some(ECO_FOUNDATION),
    ecosystem: Some(ECO_FOUNDATION),
    verified: true,
    verifier: Some("0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"),
    icon: "fa-circle-nodes",
    hidden: false,
};

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_loads_all_kinds() {
        let r = registry();
        assert!(r.list_by_kind(LabelKind::Ecosystem).len() >= 5);
        assert!(r.list_by_kind(LabelKind::Application).len() >= 6);
        assert!(r.list_by_kind(LabelKind::Contract).len() >= 10);
        assert!(r.list_by_kind(LabelKind::Asset).len() >= 4);
        assert!(r.list_by_kind(LabelKind::Bot).len() >= 9);
        assert!(r.list_by_kind(LabelKind::Agent).len() == 5);
        assert!(r.list_by_kind(LabelKind::Cord).len() == 1);
    }

    #[test]
    fn lookup_is_case_insensitive_and_handles_0x_prefix() {
        let r = registry();
        let l1 = r.get("0x60FB32EF3A2381C2ED71613F34FD56D56FCF4195");
        let l2 = r.get("60fb32ef3a2381c2ed71613f34fd56d56fcf4195");
        assert!(l1.is_some());
        assert!(l2.is_some());
        assert_eq!(l1.unwrap().display_name, l2.unwrap().display_name);
    }

    #[test]
    fn dcswap_router_resolves_with_full_lineage() {
        let r = registry();
        let l = r.get("8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4").unwrap();
        assert_eq!(l.platform, "dcswap");
        assert_eq!(l.role, "router");
        assert_eq!(l.parent.unwrap(), APP_DCSWAP_AMM);
        assert_eq!(l.ecosystem.unwrap(), ECO_DCSWAP);
    }

    #[test]
    fn child_count_descends_correctly() {
        let r = registry();
        // The DCSwap ecosystem has at least 3 applications.
        assert!(r.child_count_of(ECO_DCSWAP) >= 3);
        // And many descendants (apps + their contracts/bots/pools).
        assert!(r.descendant_count_of(ECO_DCSWAP) >= 10);
    }

    #[test]
    fn search_finds_router_by_display_name() {
        let r = registry();
        let hits = r.search("router", 5);
        assert!(!hits.is_empty());
        assert!(hits
            .iter()
            .any(|l| l.id_hex == "8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4"));
    }

    #[test]
    fn ecosystems_have_stable_synthetic_ids() {
        let r = registry();
        assert!(r.get(ECO_DCSWAP).is_some());
        assert!(r.get(ECO_TANASTOK).is_some());
        assert!(r.get(ECO_FOUNDATION).is_some());
    }
}


