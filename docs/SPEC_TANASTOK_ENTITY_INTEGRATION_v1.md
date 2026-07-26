# Specification - Tanastok Entity Integration with Datachain Rope (Quipu Canon v1.2)

**To:** Tanastok project agent (workspace: /Users/kazealphonseonguene/Downloads/tanastok-app/) and Datachain Rope node maintainers (RPC + entity_labels team)
**From:** Datachain Rope agent (workspace: /Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/)
**Re:** What every Tanastok-side entity (organizations, assets, DCNFTs, ERC-3643 securities, identity infra, monetization programs, claims, custody, partner issuers) must do to be a first-class citizen of the Rope Graph at /event.datachain.one and the per-entity Quipu string registry on the rope-node.
**Authority:** .cursor/rules/quipu-canon-v1.2-string-registry.mdc, .cursor/rules/handover-tanastok-tokenized-assets-for-dcscan-2026-03-30.mdc, datachain-rope/docs/handovers/handover-ecosystem-string-emission-2026-05-03.md
**Companion to:** Specification - Datachain Rope RPC for the Rope Graph (the v1.4.0 RPC spec)
**Status:** Draft v1, last updated 2026-05-21

---

## 0. TL;DR

Tanastok already does 70 percent of what is needed: it has computed canonical string IDs for every asset and every contract since the v1.2 emission shipped 2026-05-03 (file: src/lib/quipu-canon-emission.ts). What is missing is the other 30 percent and it is exactly the gap the founder flagged for DCSwap bots: the on-chain entity_labels registry on rope-node has placeholder Tanastok entries instead of the full 198+ asset URN string IDs and 793 deployed contract addresses, and the Tanastok issuer wallet still owns every Quipu knot Tanastok emits because rope_appendToString is not yet shipped. This document closes both halves of the gap with one spec.

What this delivers when fully implemented:

1. Every Tanastok asset, organization, DCNFT, ERC-3643, T-REX infra contract, monetization program, claim, custody record, and partner-issuer becomes a labelled, queryable entity in the Rope Graph.
2. The 198+ assets become first-class kind=asset strings on the chain registry, each with its own genesis knot and per-event lineage.
3. The 793 contracts (397 DCNFTs + 396 ERC-3643s) become first-class kind=contract strings, each binding back to its parent asset string.
4. The Tanastok organization itself, plus every issuing organization and every B2B partner, becomes a kind=did string.
5. dcscan.io and event.datachain.one render every Tanastok entity as a named, role-coloured node with parent ecosystem grouping, derived relations, and clickable cross-links (Tanastok page <-> DCScan page <-> DCSwap pool when listed).

---

## 1. Ground truth - what already exists in tanastok-app

Verified against the workspace at /Users/kazealphonseonguene/Downloads/tanastok-app/ on 2026-05-21.

### 1.1 Deployed contracts (deployments/asset-contracts-manifest.json)

A single manifest, frozen 2026-03-30T11:19Z, contains the authoritative pairing between Tanastok asset_id values and on-chain contract addresses:

    deployer:    0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195
    deployedAt:  2026-03-30T11:19:23.918Z
    dcr721Count: 397   (ERC-721 DCNFT title deeds)
    dcr3643Count: 396  (ERC-3643 T-REX security tokens)
    skipped:     14
    failed:      2

Each entry binds:

    {
      "assetId":  "featured-kibali-gold-mine"  (or "asset-203", ...)
      "name":     "Kibali Gold Mine, Congo DRC"
      "symbol":   "KGMCD"
      "dcr721":   "0x91f884D436858ad221436573BC2cB5117E27e564"
      "dcr3643":  "0x2D16be771cB30AEedD9913b70b6237a832828bbB"
      "txHash":   "0x1d4e159a..."
    }

This manifest is the canonical seed for the Datachain Rope entity_labels registry. Every (assetId, dcr721, dcr3643) triple resolves to exactly three v1.2 strings.

### 1.2 Quipu Canon v1.2 emission library (src/lib/quipu-canon-emission.ts)

Tanastok already computes canonical string IDs and ships three knot payload schemas. Verified shape:

    URN scheme:               "tanastok:asset:" + asset_id
    asset string_id:          keccak256(utf8(URN))
    contract string_id:       zeroPadValue(checksum(addr), 32)

Three knot schemas already wired into the codebase:

    tanastok_minting_complete_v1       (event_type: MintingComplete)
    tanastok_subscription_filled_v1    (event_type: SubscriptionFilled)
    tanastok_asset_registry_snapshot_v1 (event_type: RegistrySnapshot)

Wire path (lib/quipu-canon-emission.ts:invokeAppendToLedger):

    rope_appendToLedger(
      issuerWallet.address,
      {
        interaction_type: event_type,
        description: JSON.stringify(payload),
        metadata: { kind: "asset", emitter: "tanastok", schema, ... }
      }
    )

Issuer keys honoured: TANASTOK_ISSUER_PRIVATE_KEY then TANASTOK_DEPLOYER_PRIVATE_KEY. With neither set the call is logged and skipped.

Reverse-read helpers:

    fetchAssetStringKnotCount(assetId)       -> rope_getString({kind:"asset", string_id})
    fetchContractStringKnotCount(addr)       -> rope_getString({kind:"contract", string_id})

These already work today, but always return null because the rope-node has no kind=asset or kind=contract strings registered yet (see section 1.6 below).

### 1.3 Public API (src/app/api/v1/tokenized-assets/route.ts)

The /api/v1/tokenized-assets endpoint already exposes the v1.2 quipu_registry block on every asset:

    {
      "id": "featured-kibali-gold-mine",
      "string_id": "0x...",                                    (asset URN hash)
      "quipu_registry": {
        "kind": "asset",
        "string_id": "0x...",
        "knot_count": null                                     (until backfill runs)
      },
      "dcnft":  { "contractAddress": "0x...", "quipu_registry": {kind:"contract", string_id:"0x...0091f884...", knot_count: null} },
      "erc3643":{ "contractAddress": "0x...", "quipu_registry": {kind:"contract", string_id:"0x...002D16be...", knot_count: null} },
      ...
    }

CORS is open (Access-Control-Allow-Origin: *) and the cache headers (s-maxage=300, stale-while-revalidate=600) are appropriate for both DCScan and event.datachain.one polling.

### 1.4 Backfill script (scripts/quipu-backfill-asset-registry-knots.ts)

Reads every prisma.assets row where deleted_at IS NULL AND (is_tokenized OR token_contract_address IS NOT NULL), and emits one tanastok_asset_registry_snapshot_v1 knot per asset via the issuer key. Supports DRY_RUN=1, QUIPU_BACKFILL_LIMIT, QUIPU_BACKFILL_DELAY_MS=300.

Status today: not yet executed against erpc.datachain.network. When it runs, all the knots land on the issuer wallet string (not on per-asset strings) until rope_appendToString ships.

### 1.5 The Prisma data model (prisma/schema.prisma, 4335 lines)

Tanastok-side entities that have a clear on-chain Quipu equivalent or should:

| Prisma model              | Lines | Rough count | Quipu kind   |
|---------------------------|-------|-------------|--------------|
| organizations             | 2476  | 5..50       | did          |
| users (with KYC + wallet) | 2950  | many        | did          |
| user_wallets              | 2935  | many        | wallet       |
| assets                    | 889   | 198+        | asset        |
| dcnft_tokens              | 453   | 397         | contract     |
| erc3643_contracts         | 492   | 396         | contract     |
| asset_claim_requests      | 4171  | growing     | (knot only)  |
| asset_ownership_registry  | 4243  | growing     | (knot only)  |
| ownership_evidence_documents | 4203 | growing  | (knot only)  |
| monetization_programs     | 2258  | 1..N        | contract or asset |
| liquidity_pool_*          | 2136+ | N           | contract     |
| partner_api_keys          | 4289  | growing     | did          |
| partner_audit_trail       | 4321  | growing     | (knot only)  |
| ipfs_pin_manifest         | 4305  | growing     | (knot only)  |
| asset_valuations          | 868   | growing     | (knot only)  |
| asset_compliance_data     | 227   | growing     | (knot only)  |

The right side maps cleanly to Quipu Canon v1.2: every "real" entity becomes a string of one of 5 kinds (cord, wallet, contract, asset, did); everything that happens to that entity becomes a knot on its string. There are no ad-hoc kinds needed - the canon already covers Tanastok's full ontology.

### 1.6 Confirmed gaps (probed against erpc.datachain.network 2026-05-21)

    rope_globalStats              -> { total_strings: 64, by_kind: { wallet: 64 } }
    rope_listStrings(kind="asset")    -> 0 results
    rope_listStrings(kind="contract") -> 0 results
    rope_listStrings(kind="did")      -> 0 results
    entity_labels::built_in() Tanastok entries -> 1 ecosystem + 4 placeholder applications + 0 of 793 contracts + 0 of 198 assets

The chain-side has 0 percent of what Tanastok's data model expresses.

---

## 2. Target architecture - the Tanastok dimension of the Rope Graph

The frontend at event.datachain.one and dcscan.io should render the Tanastok ecosystem as a fully attributed, navigable subgraph:

    ECOSYSTEM "Tanastok"
      string_id  = synth_eco/tanastok        kind = ecosystem
      |
      +-- DID    "Tanastok Foundation"        kind = did   (the operator)
      +-- DID    "Tanastok Issuer Wallet"     kind = did   (0x297Ba8..., the v1.0 wallet that signs every emission)
      +-- DID    "Tanastok Deployer Wallet"   kind = did   (0x60FB32..., owner of TREXFactory)
      +-- DID    organizations[i].id          kind = did   (every issuing organization, one DID per org)
      |
      +-- APPLICATION "Tanastok ERC-3643 Issuance"  kind = application
      |     |
      |     +-- CONTRACT "TREXFactory"                 kind = contract  (shared infra)
      |     +-- CONTRACT "ONCHAINID Identity Registry" kind = contract  (shared infra)
      |     +-- CONTRACT "ONCHAINID ClaimTopicsRegistry" kind = contract
      |     +-- CONTRACT "ONCHAINID TrustedIssuersRegistry" kind = contract
      |     +-- CONTRACT "ONCHAINID IdentityRegistryStorage" kind = contract
      |     +-- CONTRACT "Tanastok DatawalletClaimIssuer"   kind = contract
      |     +-- CONTRACT "Tanastok ROPE ComplianceModule"   kind = contract
      |     +-- CONTRACT "Tanastok ONCHAINID"               kind = contract
      |     |
      |     +-- ASSET "Kibali Gold Mine, DRC"         kind = asset
      |     |     +-- CONTRACT DCNFT_KIBALI            kind = contract  (per-asset DCNFT 0x91f884...)
      |     |     +-- CONTRACT ERC3643_KIBALI          kind = contract  (per-asset ERC-3643 0x2D16be...)
      |     |     +-- CONTRACT ERC3643_KIBALI_IR       kind = contract  (per-asset IdentityRegistry, deployed by TREXFactory)
      |     |     +-- CONTRACT ERC3643_KIBALI_CM       kind = contract  (per-asset ComplianceModule)
      |     +-- ASSET "Pacific Blue Carbon C2"        kind = asset
      |     |     +-- CONTRACT DCNFT_PBC_C2
      |     |     +-- CONTRACT ERC3643_PBC_C2
      |     +-- ... 196 more assets ...
      |
      +-- APPLICATION "Tanastok Compliance"           kind = application
      |     +-- CONTRACT compliance_reports.*         kind = contract  (one per region/ruleset, optional)
      |
      +-- APPLICATION "Tanastok Monetization Programs" kind = application
      |     +-- CONTRACT MonetizationProgram[i]       kind = contract  (one per program; or kind=asset if treated as a yield-bearing pool)
      |
      +-- APPLICATION "Tanastok Partner Issuance"     kind = application
            +-- DID partner_api_keys[i]               kind = did       (one per partner, label.role="partner_issuer")
            +-- ASSET assets[partner=X]               kind = asset

For every ASSET node above, the on-chain knot lineage carries:

    asset string genesis knot:        AssetMinted | AssetGenesisRegistry
    asset string subsequent knots:    Valuation, ListingStatusChange, ComplianceUpdate,
                                      OwnershipClaim, OwnershipTransfer, AuditReport,
                                      InspectionUpload, IPFSPinManifestUpdate
    DCNFT contract string knots:      ContractDeployed, TokenURIUpdated, RoleGranted, Upgraded
    ERC-3643 contract string knots:   ContractDeployed, IdentityRegistered, ComplianceCheckFailed,
                                      DailyAggregate, Paused, Unpaused, Upgraded

For every DID node:

    did string knots:                 IdentityRegistered, ClaimAdded, ClaimRevoked,
                                      KYCStatusChanged, OrgVerificationGranted, OrgRevoked

This target is achievable today with the v1.2 surface and the Tanastok emission library that already exists. Section 4 lists the missing pieces.

---

## 3. The seven Tanastok entity classes - canonical mapping

Every Tanastok entity class maps to exactly one Quipu kind, with a deterministic id_bytes formula and a defined parent/ecosystem. This is the source of truth that feeds entity_labels::built_in() on the rope-node.

### 3.1 Tanastok ecosystem (the brand)

    kind:        ecosystem
    id_bytes:    keccak256("dcrope:ecosystem:tanastok")
    parent:      None (root)
    ecosystem:   self
    label:
      display_name = "Tanastok"
      short_name   = "Tanastok"
      platform     = "tanastok"
      role         = "ecosystem"
      description  = "Real-world asset tokenisation platform on Datachain Rope"
      verified     = true
      verifier     = "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195"   (deployer)

This is the synthetic root. It does not need to anchor knots itself; cardinality of its subtree is computed by the registry.

### 3.2 Tanastok applications (logical product surfaces)

Four applications per the ecosystem layout in section 2. Each is a synthetic id under the ecosystem:

    kind:      application
    id_bytes:  keccak256("dcrope:application:tanastok:" + slug)
    parent:    Tanastok ecosystem
    slug values:
      tanastok-issuance        ("Tanastok ERC-3643 Issuance")
      tanastok-compliance      ("Tanastok Compliance")
      tanastok-monetization    ("Tanastok Monetization Programs")
      tanastok-partner         ("Tanastok Partner Issuance")

These are pure synthetic strings - they exist only in the entity_labels registry; they never need to anchor knots. They are the grouping nodes the frontend uses to render the application column.

### 3.3 Organizations (the sovereign entities behind assets)

    Source:     prisma.organizations
    kind:       did
    id_bytes:   keccak256("tanastok:organization:" + organization.id)
    parent:     The application that hosts the org (typically tanastok-issuance)
    ecosystem:  tanastok
    label:
      display_name = organization.name
      short_name   = derived (first word + LegalForm)
      role         = "issuing_org" (derived from organization.type)
      verified     = organization.is_verified
      icon         = organization.logo_url

Genesis knot kind: tanastok_org_genesis_v1  (event_type: OrganizationOnboarded)
Subsequent knot kinds:
    tanastok_org_kyb_v1           (event_type: OrganizationVerificationStatusChanged)
    tanastok_org_role_v1          (event_type: OrganizationRoleAssigned)
    tanastok_org_revoked_v1       (event_type: OrganizationRevoked)

### 3.4 Tanastok-side users with KYC (the natural persons)

    Source:     prisma.users  (only those with kyc_aml_checks)
    kind:       did
    id_bytes:   ONCHAINID address bytes if the user has one,
                else keccak256("tanastok:user:" + user.id)
    parent:     Tanastok ecosystem (no application parent - users move across apps)
    label:
      display_name = redacted ("KYC-verified holder #" + short_id) by default;
                     unredacted ONLY if user explicitly opts in to public display
      role         = "holder"
      verified     = users.is_verified

Genesis knot kind: tanastok_user_kyc_v1  (event_type: KYCVerified)
Subsequent knot kinds:
    tanastok_user_claim_added_v1
    tanastok_user_claim_revoked_v1
    tanastok_user_subscription_v1   (when buying shares)
    tanastok_user_redemption_v1     (when redeeming yield)

GDPR Article 17 erasure path: see section 6 below.

### 3.5 Assets (the real-world things being tokenized) - the largest class

    Source:     prisma.assets where deleted_at IS NULL
    kind:       asset
    URN:        "tanastok:asset:" + asset.id
    id_bytes:   keccak256(URN)
    parent:     The asset's parent organization DID  (or tanastok-issuance application if no org)
    ecosystem:  tanastok
    label:
      display_name = assets.name
      short_name   = derived (truncate to 24 chars)
      description  = assets.short_description
      role         = mapped from assets.asset_type   (see section 3.5.1)
      icon         = assets.brand_logo_url or assets.hero_image_url
      verified     = assets.is_verified
      verifier     = the auditor address that signed the most recent valuation knot

Genesis knot kind: tanastok_asset_genesis_v1
    {
      "kind":            "tanastok_asset_genesis_v1",
      "event_type":      "AssetGenesis",
      "asset_id":        "featured-kibali-gold-mine",
      "name":            "Kibali Gold Mine, Congo DRC",
      "asset_type":      "GOLD_MINE",
      "value_usd":       10000000053,
      "total_shares":    19736,
      "owner_did":       "0x...",                      (if known at genesis)
      "organization_did":"0x...",                      (if known at genesis)
      "tanastok_url":    "https://tanastok.io/assets/featured-kibali-gold-mine",
      "dcnft_address":   "0x91f884...",                (if known at genesis; else linked later)
      "erc3643_address": "0x2D16be...",
      "ipfs_metadata":   "ipfs://Qm..." (if available)
    }

Subsequent knot kinds (every meaningful state change is a knot on the asset string):
    tanastok_asset_valuation_v1            (Quarterly DCF audit; carries previous_usd, new_usd, auditor, report_ipfs)
    tanastok_asset_listing_status_v1       (PENDING, ACTIVE, PAUSED, DELISTED, REJECTED)
    tanastok_asset_compliance_v1           (KYC tier change, geo restriction, regulatory approval)
    tanastok_asset_audit_report_v1         (Off-chain inspection, photos, sensor data, IPFS-pinned)
    tanastok_asset_ownership_transfer_v1   (owner_did_old -> owner_did_new, on-chain tx hash)
    tanastok_asset_claim_filed_v1          (asset_claim_requests row genesis)
    tanastok_asset_claim_resolved_v1       (asset_claim_requests reaches APPROVED or REJECTED)
    tanastok_asset_metadata_revision_v1    (tokenURI rotation; new IPFS CID)
    tanastok_asset_ipfs_pin_update_v1      (ipfs_pin_manifest changes for this asset)

#### 3.5.1 asset_type to role mapping (frontend palette)

The Prisma AssetType enum has 80+ values. Compress to the role taxonomy the frontend palette already uses:

    GOLD_MINE, COPPER_MINE, LITHIUM_MINE, RARE_EARTH_MINE, MINES, MINERALS, GEMN  -> "mine"
    DIAMOND, DIAMOND_MINE, JEWELRY                                                -> "gem"
    GOLD, PRECIOUS_METALS                                                         -> "precious_metal"
    REAL_ESTATE, COMMERCIAL_PROPERTY, RESIDENTIAL_PROPERTY, INDUSTRIAL_PROPERTY,
    MIXED_USE_DEVELOPMENT, AGRICULTURAL_LAND, LAND_MASS                           -> "real_estate"
    FOREST, FORESTERY, WETLAND, GRASSLAND, MANGROVE, CORAL_REEF,
    BIODIVERSITY_CREDIT, WETLAND_CREDIT, CARBON_CREDIT, CONSERVATION_AREA         -> "natureproof"
    SOLAR_FARM, WIND_FARM, HYDRO_PLANT, RENEWABLE_ENERGY, ENERGY                  -> "energy"
    OIL_FIELD, GAS_FIELD                                                          -> "fossil"
    CROP_LAND, ORCHARD, VINEYARD, LIVESTOCK_FARM, FISHING_QUOTA, AQUIFER,
    AGRICULTURAL                                                                  -> "agriculture"
    SUPERCAR, AUTOMOTIVE, PRIVATE_JETS, SUPER_YATCH, YATCH, AVIATION, SHIPPING,
    VEHICLES                                                                      -> "vehicle"
    LUXURY_WATCH, WATCHES, ART, ART_PIECE, FASHION, WINE, WINERY, COLLECTIBLE,
    NFT_COLLECTIBLE                                                               -> "luxury"
    INFRASTRUCTURE, INFRASTRUCTURE_PROJECT, BUNKER, MACHINERY                     -> "infrastructure"
    CULTURAL_HERITAGE                                                             -> "heritage"
    INTELLECTUAL_PROPERTY, MUSIC_ROYALTIES, ENTERTAINMENT, SPORTS, TECHNOLOGY,
    HEALTHCARE                                                                    -> "ip"
    BONDS, STARTUP_EQUITY, COMMODITIES, STOCKS, DERIVATIVE, PRIVATE_EQUITY,
    HEDGE_FUND, STRUCTURED_PRODUCT, SECURITY, FINANCIAL, CRYPTOCURRENCY, OTHER    -> "financial"

These role values are what entity_labels.rs surfaces in label.role and what the frontend reads in the PLATFORMS palette.

### 3.6 Per-asset contracts (DCNFT + ERC-3643 + per-asset T-REX infra)

    Source:     prisma.dcnft_tokens, prisma.erc3643_contracts,
                erc3643_contracts.identity_registry, erc3643_contracts.compliance_module
    kind:       contract
    id_bytes:   zeroPadValue(checksum(contract_address), 32)   (per existing emission lib)
    parent:     The asset string this contract belongs to
    ecosystem:  tanastok
    label:
      display_name = asset.name + " - DCNFT"  /  asset.name + " - " + token_symbol  /  asset.name + " - IR"  /  asset.name + " - CM"
      short_name   = symbol or "DCNFT" / "IR" / "CM"
      role         = "dcnft" | "erc3643" | "trex_identity_registry" | "trex_compliance_module"
      verified     = true (always - they were deployed by the trusted deployer)
      verifier     = 0x60FB32...

Genesis knot kind:
    tanastok_contract_deployed_v1
      { kind: "tanastok_contract_deployed_v1",
        event_type: "ContractDeployed",
        role: "dcnft" | "erc3643" | "trex_identity_registry" | "trex_compliance_module",
        contract_address, asset_string_id, asset_id, deployer,
        tx_hash, deployment_date, factory_salt }

Subsequent knot kinds (DCNFT):
    tanastok_dcnft_minted_v1               (after the title token is actually minted to the issuer)
    tanastok_dcnft_uri_revision_v1         (tokenURI rotation - new IPFS metadata CID)
    tanastok_dcnft_role_change_v1          (MINTER_ROLE / UPDATER_ROLE granted/revoked)
    tanastok_dcnft_upgraded_v1             (UUPS upgrade event)

Subsequent knot kinds (ERC-3643):
    tanastok_erc3643_unpaused_v1           (initial Listing event)
    tanastok_erc3643_paused_v1
    tanastok_erc3643_compliance_failure_v1 (when transfer is rejected by compliance)
    tanastok_erc3643_daily_aggregate_v1    (cron, once per 24h: holders, supply_minted, supply_burned, transfer_count)
    tanastok_erc3643_listed_dcswap_v1      (when erc3643_contracts.is_listed_dcswap flips true; binds to DCSwap pool)

These contract strings are the right place to land regulator-relevant compliance events without bloating the asset string.

### 3.7 Shared T-REX infrastructure (one set per network, not per asset)

These addresses already exist in handover-dcswap-redeployed-2026-02-26.mdc but the production set must be re-listed by the Tanastok agent in tanastok-app/deployments/ once redeployed post-Reth-migration:

    kind:       contract
    id_bytes:   zeroPadValue(checksum(addr), 32)
    parent:     application "tanastok-issuance"
    role:       "trex_factory" | "trex_implementation_authority" | "onchainid_factory" |
                "onchainid_identity_registry" | "onchainid_claim_topics_registry" |
                "onchainid_trusted_issuers_registry" | "onchainid_identity_registry_storage" |
                "tanastok_datawallet_claim_issuer" | "tanastok_compliance_module" |
                "tanastok_onchainid"
    verified:   true
    verifier:   deployer

Each gets a single genesis knot tanastok_infra_deployed_v1 carrying its role and bytecode hash. Day-to-day operational events (claim issued, identity registered) land on these contract strings.

### 3.8 Monetization programs and liquidity pools (program-level entities)

    Source:    prisma.monetization_programs
    kind:      contract  (when smart_contract_address is non-null)
                or asset (when treated as a pool that itself has shares)
    id_bytes:  zeroPadValue(smart_contract_address, 32) or keccak256("tanastok:program:" + program.id)
    parent:    application "tanastok-monetization"
    role:      "monetization_program" or specific PPP mechanism type

Genesis knot: tanastok_program_deployed_v1
Subsequent knots:
    tanastok_program_status_v1   (DRAFT, OPEN, FUNDED, ACTIVE, COMPLETED, CANCELLED)
    tanastok_program_yield_v1    (per-distribution payouts)
    tanastok_program_membership_v1 (participant joined/left)

### 3.9 Partner issuers (B2B integrations via /api/v1/partner)

    Source:    prisma.partner_api_keys
    kind:      did
    id_bytes:  keccak256("tanastok:partner:" + api_key_prefix)
    parent:    application "tanastok-partner"
    role:      "partner_issuer"
    verified:  partner_api_keys.is_active && partner_api_keys.expires_at > now

Genesis knot: tanastok_partner_onboarded_v1
Subsequent knots:
    tanastok_partner_asset_submitted_v1  (POST /api/v1/partner/assets - one knot per asset they push)
    tanastok_partner_audit_v1            (one knot per row in partner_audit_trail)
    tanastok_partner_revoked_v1          (key disabled)

This makes the partner channel auditable on chain - regulators see which assets came from which partner and when.

---

## 4. What the Datachain Rope side must do

This is the chain-side counterpart. None of it requires the v1.2.1 RPC extension.

### 4.1 Extend entity_labels::built_in() with the Tanastok manifest

The rope-node Rust source at crates/rope-node/src/entity_labels.rs currently has 1 ecosystem entry and a handful of placeholder applications. It must learn to enumerate every Tanastok contract address from a generated catalogue. Two implementation options:

Option A - bake at compile time (simple, immutable):
    1. Tanastok agent commits deployments/asset-contracts-manifest.json to the rope-node tree as
       crates/rope-node/data/tanastok-manifest.json (or via a build.rs include_str!).
    2. entity_labels.rs reads the manifest at static init and emits 1 + 4 + 397 + 396 entries
       (ecosystem + applications + DCNFTs + ERC-3643s). Plus 10 T-REX infra entries from a
       separate small catalogue.
    3. Naming: per-asset contract entries are auto-generated from the manifest's name + role:
            display_name = manifest.name + " (DCNFT)"  or  manifest.name + " (" + symbol + ")"
            parent       = the asset string id  (computed from URN at static init)
            ecosystem    = tanastok ecosystem id

Option B - lazy load from the live API (dynamic, cache-backed):
    1. rope-node spawns a 5-minute refresh task that hits
       GET https://tanastok.io/api/v1/tokenized-assets?limit=500
    2. Builds an overlay HashMap merged on top of the compile-time built_in() table.
    3. Survives Tanastok onboarding new assets without a node redeploy.

Recommendation: ship Option A as the immediate fix (it closes the gap today) AND ship Option B as the steady-state behaviour. Option A is the safety net when Tanastok's API is down; Option B is the source of truth when the asset list grows.

The two together produce a registry where every Tanastok contract on the chain has a label, role, parent asset, and ecosystem before a single knot has been emitted.

### 4.2 Synthesize per-asset strings in rope_listStrings without on-chain writes

Until rope_appendToString ships, the rope-node MUST surface synthetic "shadow" strings derived purely from entity_labels. Concretely: if a label has kind=asset and id_bytes are present, rope_listStrings({kind:"asset"}) must return a descriptor for it even when no on-chain knot exists yet, with knot_count=0 and a label.synthetic=true marker.

This unblocks the frontend:
    - Today (no Tanastok writes yet): the graph already shows 198 named asset nodes derived from labels.
    - Day Tanastok backfill runs: knot_count flips from 0 to 1 for each asset, no schema change needed.
    - Day rope_appendToString ships: the asset strings migrate to "real" registry entries; the label still drives display.

The same applies to kind=contract for the 793 Tanastok contracts and kind=did for the orgs.

This was already built into the v1.4.0 RPC implementation (synthetic_string_to_json in crates/rope-node/src/rpc_server.rs) - it just needs the labels populated.

### 4.3 Derive Tanastok-specific relations in rope_listRelations

Six relation kinds the rope-node should derive from the (now-populated) Tanastok labels:

    kind="contains"      ecosystem -> application
    kind="hosts"         application -> asset
    kind="issues"        organization_did -> asset
    kind="anchors"       asset -> per-asset DCNFT contract
    kind="securitizes"   asset -> per-asset ERC-3643 contract
    kind="settles_into"  per-asset ERC-3643 -> DCSwap pool (only when erc3643_contracts.is_listed_dcswap)

The first four are implicit in the parent/ecosystem fields and ship for free with the existing derive_relations() helper. "Issues" needs a backreference from the asset label to organization_id (already in prisma.assets.organization_id). "Settles_into" requires the rope-node to also know the DCSwap pool address per ERC-3643 (a single extra column in the manifest, see 4.4).

### 4.4 Extend asset-contracts-manifest.json with the missing seven fields

The current manifest carries assetId, name, symbol, dcr721, dcr3643, txHash. To make the rope-node label rich enough, six fields must be added going forward (and backfilled once for the existing 397 assets):

    assetType         (mapped to Quipu role per section 3.5.1)
    organizationId    (FK to prisma.organizations)
    identityRegistry  (per-asset T-REX IR address)
    complianceModule  (per-asset T-REX CM address)
    dcswapPoolAddress (when listed; null otherwise)
    isVerified        (asset.is_verified at manifest write time)
    initialValuationUsd (asset.value at manifest write time)

This is purely a Tanastok-side manifest write; no chain change. Once shipped, the rope-node's compile-time include picks up everything it needs to render the full subgraph.

### 4.5 Add a `kind=organization` view

While `did` is the canonical kind for organizations, add a synthetic alias on rope_listStrings({kind:"organization"}) that returns `did` strings whose label.role starts with "issuing_org". Pure server-side filter; no canon change. Used by the frontend "Issuers" tab.

### 4.6 Optional: generate a Tanastok-asset semantic search index seed

The semantic-agent at https://semantic-agent.datachain.network already indexes every knot. To make Tanastok asset names findable BEFORE any knots exist, ship a one-time seed: ingest the manifest at build time and pre-populate the tantivy index with (string_id, display_name, description, asset_type) tuples. Optional but high-leverage for /v1/search?q=kibali.

---

## 5. What the Tanastok side must do

Five concrete deliverables on the tanastok-app side. Each is small in isolation; together they close the round trip.

### 5.1 Run scripts/quipu-backfill-asset-registry-knots.ts in production

This emits one tanastok_asset_registry_snapshot_v1 knot per tokenized asset. Today every emission collapses onto the issuer wallet string (because rope_appendToLedger is wallet-keyed), but each knot's metadata.string_id and metadata.asset_id provide enough information for the v1.2.1 migration tool to replay them onto per-asset strings later.

Pre-flight:
    DRY_RUN=1 npx tsx scripts/quipu-backfill-asset-registry-knots.ts | tee backfill.log
    grep "would emit" backfill.log | wc -l   # should equal 198+

Production:
    QUIPU_BACKFILL_DELAY_MS=300 npx tsx scripts/quipu-backfill-asset-registry-knots.ts

Verification (rope-node side):
    curl https://erpc.datachain.network -X POST -H 'content-type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"rope_getString","params":[["0x297Ba821da55ED5E37C5C25B3832CE45fC54C475"]]}'
    # should report knot_count >= 198

### 5.2 Hook genesis emission into the asset onboarding pipeline

prisma.assets writes happen in three places in the codebase:
    src/app/api/v1/partner/assets/route.ts        (partner POST)
    src/app/api/admin/assets/...                  (admin asset-creation flows)
    src/app/api/datawallet/...                    (datawallet+ asset bridge)

Each call site must emit a tanastok_asset_genesis_v1 knot AT the moment the asset row's is_tokenized flips to true (or when the DCNFT minting tx confirms, whichever comes first). Add a thin wrapper in src/lib/quipu-canon-emission.ts:

    export function emitAssetGenesis(args: {
      assetId: string;
      name: string;
      assetType: string;
      ownerDid?: string;
      organizationDid?: string;
      dcnftAddress?: string;
      erc3643Address?: string;
      valueUsd?: number;
      totalShares?: number;
    }): void

Behaviour mirrors emitMintingCompleteErc3643. Schedule, do not await.

### 5.3 Wire valuation, listing-status, claim-resolution, and ownership-transfer events

Four high-value knots that are pure value-add for regulators and end users:

    Hook 1 - asset_valuations.create()  -> emitAssetValuation(...)
    Hook 2 - assets.update({ listing_status }) -> emitAssetListingStatus(...)
    Hook 3 - asset_claim_requests.update({ status }) when status reaches APPROVED|REJECTED
             -> emitAssetClaimResolved(...)
    Hook 4 - asset_ownership_registry.create() -> emitAssetOwnershipTransfer(...)

Schemas defined in section 3.5. Each is a 30-line addition to quipu-canon-emission.ts plus one line at the call site.

### 5.4 Expose an /api/v1/tanastok-entity-manifest endpoint

The rope-node 5-minute pull (section 4.1 Option B) needs a single endpoint that lists every entity Tanastok wants registered, not just assets. Suggested shape:

    GET /api/v1/tanastok-entity-manifest
    {
      "version": "1.0.0",
      "generated_at": 1779296443,
      "entities": [
        { "kind": "did", "string_id": "0x...", "id_bytes": "0x...",
          "label": { "display_name": "Tanastok Foundation", "role": "operator", "verified": true },
          "parent_string_id": null, "ecosystem_id": "0x..." },
        { "kind": "asset", ... },
        { "kind": "contract", ... },
        ...
      ]
    }

This collapses three existing endpoints (tokenized-assets, organizations, partner) into one entity-graph view. The rope-node consumes it; the frontend can use it too.

### 5.5 Stop overloading "is_verified" - introduce a verifier address

Today prisma.assets.is_verified is a boolean with no audit trail. The Quipu canon expects a verifier_address. Add one column:

    is_verified         BOOLEAN  (existing)
    verifier_address    VARCHAR(66)  -> NEW
    verified_at         TIMESTAMP    (rename verification_date)
    verification_proof  TEXT         -> NEW (signed message OR ipfs cid of proof bundle)

The verifier_address shows up as label.verifier on the rope-node side. This is the foundation for "verified by Foundation" / "verified by external auditor X" badges in the graph.

---

## 6. GDPR Article 17 - the per-entity erasure path

Quipu Canon v1.2 supports rope_untieKnot (per-knot tombstones). Tanastok must define which knots are erasable for which actor:

| Knot kind                                  | Erasable by                | Tombstone preserves   |
|--------------------------------------------|----------------------------|-----------------------|
| tanastok_asset_genesis_v1                  | NEVER (regulatory record)  | -                     |
| tanastok_asset_valuation_v1                | NEVER                      | -                     |
| tanastok_asset_listing_status_v1           | NEVER                      | -                     |
| tanastok_asset_compliance_v1               | NEVER                      | -                     |
| tanastok_asset_audit_report_v1             | NEVER                      | -                     |
| tanastok_asset_ownership_transfer_v1       | NEVER                      | -                     |
| tanastok_user_kyc_v1                       | The user (Art 17)          | timestamp + DID       |
| tanastok_user_claim_added_v1               | The user (Art 17)          | timestamp + DID       |
| tanastok_user_subscription_v1              | NEVER (financial record)   | -                     |
| tanastok_user_redemption_v1                | NEVER (financial record)   | -                     |
| tanastok_partner_audit_v1                  | NEVER                      | -                     |

Erasure flow:
    1. User submits POST https://compliance-agent.datachain.network/v1/gdpr/article17 with { user_did, knot_kinds[] }.
    2. compliance-agent verifies the request signature.
    3. compliance-agent calls rope_untieKnot(string_id=user_did, knot_id) for each erasable knot.
    4. compliance-agent anchors a GdprArticle17Testimony to the Foundation's compliance string.
    5. Tanastok-side mirror: a webhook sets prisma.users.gdpr_erased_at = now() and clears the underlying KYC payload.

The asset and contract layers are immune by design: regulator-relevant facts about a tokenised real-world asset are never personal data of the individual user, so Article 17 never reaches them.

---

## 7. Acceptance tests (run these to confirm the spec is shipped)

Save as scripts/tanastok-rope-acceptance.sh on either side. Every test should pass on a correctly implemented stack.

T1 - rope-node knows about Tanastok ecosystem:
    rope_listEcosystems()   ->  result.ecosystems[*].labels.platform contains "tanastok"

T2 - rope-node enumerates all Tanastok assets via labels:
    rope_listStrings({kind:"asset", platform:"tanastok", limit:500})
        -> total >= 198 even before any backfill knots are emitted

T3 - rope-node enumerates all Tanastok contracts:
    rope_listStrings({kind:"contract", platform:"tanastok", limit:1000})
        -> total >= 793 (397 DCNFTs + 396 ERC-3643s + ~10 shared infra + per-asset IR + per-asset CM)

T4 - asset string resolves with full metadata:
    rope_getString({kind:"asset", string_id: keccak256("tanastok:asset:featured-kibali-gold-mine")})
        -> labels.display_name == "Kibali Gold Mine, Congo DRC"
        -> labels.role         == "mine"
        -> ecosystem_id        == tanastok ecosystem synthetic id
        -> verified            == true (post-3.5 row migration)

T5 - contract string parents back to asset string:
    rope_getString({kind:"contract", string_id: zeroPad32("0x91f884D436858ad221436573BC2cB5117E27e564")})
        -> labels.role         == "dcnft"
        -> parent_string_id    == keccak256("tanastok:asset:featured-kibali-gold-mine")
        -> labels.display_name contains "Kibali"

T6 - relations link assets to their contracts:
    rope_listRelations({from: <kibali asset string id>, kind: "anchors"})
        -> at least one relation pointing to the DCNFT contract
    rope_listRelations({from: <kibali asset string id>, kind: "securitizes"})
        -> at least one relation pointing to the ERC-3643 contract

T7 - bulk read returns full Tanastok subgraph in one call:
    rope_listStringsWithKnots({platform:"tanastok", limit:200, knot_limit:5})
        -> 198+ entries, each with knots[] (possibly empty pre-backfill)
        -> wall-clock < 500 ms

T8 - tanastok side: backfill ran:
    rope_globalStats() -> by_kind.wallet.knots increased by >= 198 since baseline
    OR (post v1.2.1)
    rope_globalStats() -> by_kind.asset.strings >= 198 AND by_kind.asset.knots >= 198

T9 - tanastok side: API exposes string ids:
    GET https://tanastok.io/api/v1/tokenized-assets?limit=5
        -> every record has string_id, quipu_registry.kind=asset, quipu_registry.string_id
        -> dcnft.quipu_registry.kind=contract  AND  erc3643.quipu_registry.kind=contract

T10 - tanastok side: entity manifest available:
    GET https://tanastok.io/api/v1/tanastok-entity-manifest
        -> result.entities length >= 1000   (1 eco + 4 apps + 198 assets + 793 contracts + N orgs + N partners)

T11 - per-knot CI invariant holds (existing test in scripts/check-rope-invariant.ts):
    rope_globalStats().invariant_holds === true   (tested today; must stay true after every batch)

---

## 8. Rollout plan (so nothing breaks at any step)

The dependency graph is small. The phases below are ordered by risk-and-effort.

Phase 1 - Label-only fix (no chain writes) - rope-node side, ~2 days
    1.1 Tanastok commits a checked-in copy of asset-contracts-manifest.json to either
        the rope-node tree (preferred) or pushes /api/v1/tanastok-entity-manifest live.
    1.2 entity_labels.rs gains a tanastok_manifest module that materialises:
          - 1 ecosystem
          - 4 applications
          - 397 DCNFT contract labels
          - 396 ERC-3643 contract labels
          - 10 T-REX infra labels
          - 198+ synthetic asset labels (one per assetId in the manifest)
          - N organisation DID labels
    1.3 rope_listStrings/contracts/assets returns shadow descriptors with knot_count=0.
    1.4 Frontend immediately renders the full Tanastok subgraph as named nodes -
        "Kibali Gold Mine", "Pacific Blue Carbon C2", "FIN1/WFAT Pool" - even though
        no knots exist yet.
    1.5 ACCEPTANCE: T1, T2, T3, T4, T5 pass.

Phase 2 - Tanastok backfill - tanastok side, ~1 day
    2.1 Tanastok runs scripts/quipu-backfill-asset-registry-knots.ts in production.
    2.2 198+ knots land on the issuer wallet string with metadata.kind=asset.
    2.3 rope-node reflects the higher knot_count via the existing reverse-read helpers.
    2.4 ACCEPTANCE: T8, T11 pass.

Phase 3 - Live emission on every state change - tanastok side, ~3-5 days
    3.1 Wire emitAssetGenesis at the three asset-creation call sites.
    3.2 Wire emitAssetValuation, emitAssetListingStatus, emitAssetClaimResolved,
        emitAssetOwnershipTransfer per section 5.3.
    3.3 Wire ERC-3643 daily-aggregate cron (one knot per ERC-3643 per day).
    3.4 Wire DCNFT URI revision and role-change events.
    3.5 ACCEPTANCE: T6, T7, T9 pass; rope-node knot count grows organically.

Phase 4 - Manifest enrichment + verifier addresses - tanastok side, ~2 days
    4.1 Add the seven manifest fields per section 4.4.
    4.2 Add verifier_address column per section 5.5; backfill from auditor records.
    4.3 Re-run Phase 1 build (rope-node consumes richer manifest).
    4.4 Frontend gains the "verified by X" badges and the settles_into relation.
    4.5 ACCEPTANCE: T6 includes settles_into for any DCSwap-listed ERC-3643.

Phase 5 - rope_appendToString migration - rope-node side, blocked on v1.2.1
    5.1 Rope-node ships rope_appendToString({kind, id_bytes, payload}) per the canon.
    5.2 quipu-canon-emission.ts swaps invokeAppendToLedger for invokeAppendToString.
    5.3 The v1.2.1 replay tool walks every Tanastok knot on the issuer wallet string,
        re-emits to the per-entity string, and tombstones the wallet copy.
    5.4 ACCEPTANCE: rope_globalStats.by_kind.asset.strings >= 198 AND knots >= 1000.

Phase 6 - GDPR Article 17 surface - tanastok + compliance-agent, ~2 days
    6.1 Tanastok admin UI exposes a "request erasure" link for KYC-verified users.
    6.2 The link POSTs to compliance-agent /v1/gdpr/article17 with the user DID and
        the knot kinds they want erased (default: section 6 erasable list).
    6.3 Tanastok webhook clears the underlying KYC payload in prisma.users.

Phase 7 - Frontend cut-over - event.datachain.one, ~1-2 days
    7.1 Replace the Tanastok placeholder map in the rope-graph IIFE with a single
        rope_listStringsWithKnots({platform:"tanastok"}) call.
    7.2 Frontend draws the full ecosystem -> application -> asset -> contracts
        nesting from real chain data.
    7.3 ACCEPTANCE: page renders 198+ named entities with role-coloured knots
        and the cross-link panel (Tanastok / DCScan / DCSwap) wired to every node.

---

## 9. Why this matters for 25 June

The Rope Graph centrepiece on event.datachain.one currently shows 52 anonymous wallets and 3,937 wallet-string knots. Tanastok alone, when this spec is implemented, contributes:

    198+   asset strings (one per tokenized RWA)
    793+   contract strings (DCNFTs + ERC-3643s + per-asset T-REX infra)
    10+    shared T-REX infra strings
    1      ecosystem string
    4      application strings
    N      DID strings (organizations + KYC users + partners)
    -----
    1006+  named, role-coloured, parent-grouped nodes (vs 52 hex strings today)

Plus, for each asset, ~5 to ~50 knots over the lifecycle (genesis + valuations + status changes + compliance + ownership + audit reports). At a conservative average of 15 knots per asset, this is another ~3000 first-class on-chain events that today exist only in the Tanastok Postgres database.

The press, investors, and patron-cosigners on launch night will see "1000+ tokenized real-world assets, with full per-asset audit lineage, queryable on a public smartchain". That claim is provable end-to-end the moment Phase 1 ships.

---

## 10. Reference

Primary files:
    /Users/kazealphonseonguene/Downloads/tanastok-app/deployments/asset-contracts-manifest.json
    /Users/kazealphonseonguene/Downloads/tanastok-app/src/lib/quipu-canon-emission.ts
    /Users/kazealphonseonguene/Downloads/tanastok-app/src/app/api/v1/tokenized-assets/route.ts
    /Users/kazealphonseonguene/Downloads/tanastok-app/scripts/quipu-backfill-asset-registry-knots.ts
    /Users/kazealphonseonguene/Downloads/tanastok-app/scripts/check-rope-invariant.ts
    /Users/kazealphonseonguene/Downloads/tanastok-app/prisma/schema.prisma
    /Users/kazealphonseonguene/Downloads/tanastok-app/contracts/DataAugmentedDCNFT.sol
    /Users/kazealphonseonguene/Downloads/tanastok-app/contracts/TanastokSecurityToken.sol
    /Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/datachain-rope/crates/rope-node/src/entity_labels.rs
    /Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/datachain-rope/crates/rope-node/src/rpc_server.rs
    /Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/datachain-rope/docs/handovers/handover-ecosystem-string-emission-2026-05-03.md
    /Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/.cursor/rules/quipu-canon-v1.2-string-registry.mdc
    /Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/.cursor/rules/handover-tanastok-tokenized-assets-for-dcscan-2026-03-30.mdc

Live infrastructure:
    https://erpc.datachain.network                 (RPC)
    https://dcscan.io                              (block explorer)
    https://event.datachain.one                    (launch landing page)
    https://tanastok.io                            (Tanastok app)
    https://tanastok.io/api/v1/tokenized-assets    (public asset feed, already exposes Quipu string ids)
    https://semantic-agent.datachain.network/v1/search   (semantic search across all knots)
    https://compliance-agent.datachain.network/v1/gdpr/article17  (GDPR Art 17 endpoint)

---

## 11. Open points (please reply on this thread before implementation)

1. Are the 198+ asset URN strings allowed to live as synthetic shadow strings in entity_labels (Phase 1) before Tanastok actually emits any knots, or must every kind=asset descriptor be backed by at least one on-chain knot? The proposed answer: synthetic shadow strings are allowed during Phase 1, with a label.synthetic=true marker on the descriptor; the moment Tanastok backfills, the marker drops. This avoids the chicken-and-egg problem of "the frontend cannot find the asset because the asset has no knots yet, but the asset has no knots yet because nobody emitted them, and nobody emitted them because they are not visible".

2. How are organisations represented? The proposed answer is kind=did with id_bytes = keccak256("tanastok:organization:" + id), but if Datachain Foundation prefers per-org ONCHAINID-style identity addresses we should align. Datawallet+ may have a strong opinion since they own the DID layer.

3. Should the Tanastok issuer wallet (0x297Ba8...) be retroactively re-labelled in entity_labels with role="tanastok_issuer", parent=tanastok-issuance application? That would group the existing 64 knots produced by Tanastok into the right place in the Rope Graph immediately, even before the per-asset migration.

4. Is the manifest pinned to /api/v1/tanastok-entity-manifest authoritative, or is the on-chain registry (post v1.2.1) authoritative? Recommendation: the on-chain registry is the source of truth; the manifest is the bootstrap/discovery channel. They MUST agree. A diff-checker cron should alert if they diverge.

5. What is the canonical role taxonomy across the ecosystem? Section 3.5.1 proposes 14 role values for the asset palette. NaturaProof and Datawallet+ may want their own role values; we should agree on a shared registry across .cursor/rules/ in all four workspaces.

---

*If anything in this spec is ambiguous, please reply on this thread before implementation. Small surface-shape disagreements early are cheaper than re-flowing the manifest later. For coordination questions, ping contact@datachain.one.*
