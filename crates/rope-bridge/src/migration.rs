//! # Legacy DC → Native FAT Migration Verification
//!
//! Implements Part A of the DC FAT Legacy Migration specification
//! (`docs/DC_FAT_LEGACY_MIGRATION_AND_MARKET_VISIBILITY_SPEC_V2.md`):
//! the Rope-side verification layer for the one-directional burn-and-mint
//! migration of legacy ERC-20 DC (Ethereum) and XRC-20 DC (XDC Network)
//! into native DCR-20 FAT on Datachain Rope (chain ID 271828).
//!
//! ## Responsibilities
//!
//! * **Origin-chain registry** — the two legacy contracts, their chain IDs
//!   and finality depths (spec §A.1 / §A.4 step 4).
//! * **Tracked state roots** — proofs only verify against roots installed
//!   through the audited light-client update path (spec §8, v1.0).
//! * **Nullifier set** — atomic check-and-consume so a burn event can never
//!   mint twice (spec §8: "checked atomically with the mint operation").
//! * **Caps + auto-pause** — per-transaction and sliding-window caps that
//!   escalate to an automatic pause on breach (spec §A.7).
//! * **Canonical error codes** — 2001–2005 exactly as published in the RPC
//!   specification (spec §6.4), so `rope_submitMigrationProof` can surface
//!   them verbatim.
//!
//! ## Proof binding
//!
//! A proof is the binary Merkle format already used by
//! [`crate::ethereum::EthereumBridge::verify_proof`]:
//! `root(32) ‖ key_len(4,BE) ‖ key ‖ value_len(4,BE) ‖ value ‖ siblings(32×N)`.
//! The `value` MUST be the 32-byte burn commitment
//! `BLAKE3(chain_id_be8 ‖ burn_id ‖ amount_be16 ‖ destination)` — this binds
//! the Merkle inclusion to the exact claim (chain, burn ID, amount,
//! destination Datawallet), which is what turns an inclusion proof into an
//! authorization to mint. Any divergence between the claim and the proven
//! commitment is an amount/binding mismatch (error 2003).

use std::collections::{HashMap, HashSet, VecDeque};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Domain-separation tag for the burn commitment hash. Changing this is a
/// breaking protocol change and requires a coordinated redeploy of the
/// origin burn contracts' event derivation.
pub const BURN_COMMITMENT_DOMAIN: &[u8] = b"DCROPE/legacy-migration/burn-commitment/v1";

/// Canonical RPC error codes (spec §6.4).
pub const ERR_INVALID_PROOF: u16 = 2001;
pub const ERR_NULLIFIER_USED: u16 = 2002;
pub const ERR_AMOUNT_MISMATCH: u16 = 2003;
pub const ERR_UNKNOWN_ORIGIN: u16 = 2004;
pub const ERR_BRIDGE_PAUSED: u16 = 2005;

/// The two legacy origin chains this migration accepts. Deliberately a
/// closed enum: adding an origin chain is a governance decision, not a
/// configuration knob.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OriginChain {
    /// Ethereum mainnet — legacy ERC-777/ERC-20 DC.
    Ethereum,
    /// XDC Network — legacy XRC-20 DC.
    Xdc,
}

impl OriginChain {
    /// EVM chain ID of the origin chain.
    pub fn chain_id(&self) -> u64 {
        match self {
            OriginChain::Ethereum => 1,
            OriginChain::Xdc => 50,
        }
    }

    /// The verified legacy DC contract on this chain (spec §A.1).
    pub fn legacy_contract(&self) -> &'static str {
        match self {
            OriginChain::Ethereum => "0x0b44547be0a0df5dcd5327de8ea73680517c5a54",
            OriginChain::Xdc => "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a",
        }
    }

    /// Finality depth the relayer must wait for before a proof is
    /// considered stable (spec §A.4 step 4). Ethereum: two epochs
    /// expressed in blocks (64); XDC: 30 blocks.
    pub fn finality_depth(&self) -> u64 {
        match self {
            OriginChain::Ethereum => 64,
            OriginChain::Xdc => 30,
        }
    }

    /// Resolve from an EVM chain ID. Unknown IDs are rejected with
    /// error 2004 by the verifier.
    pub fn from_chain_id(id: u64) -> Option<Self> {
        match id {
            1 => Some(OriginChain::Ethereum),
            50 => Some(OriginChain::Xdc),
            _ => None,
        }
    }
}

/// Migration verification error. `code()` yields the canonical RPC code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationError {
    /// 2001 — proof malformed, root not tracked, or Merkle verification failed.
    InvalidProof(String),
    /// 2002 — the burn ID was already consumed by a successful mint.
    NullifierUsed([u8; 32]),
    /// 2003 — the claim's amount/destination does not match the proven commitment.
    BindingMismatch,
    /// 2004 — origin chain not recognized.
    UnknownOriginChain(u64),
    /// 2005 — bridge paused (manually or by cap auto-pause).
    Paused(String),
}

impl MigrationError {
    /// The canonical RPC error code (spec §6.4).
    pub fn code(&self) -> u16 {
        match self {
            MigrationError::InvalidProof(_) => ERR_INVALID_PROOF,
            MigrationError::NullifierUsed(_) => ERR_NULLIFIER_USED,
            MigrationError::BindingMismatch => ERR_AMOUNT_MISMATCH,
            MigrationError::UnknownOriginChain(_) => ERR_UNKNOWN_ORIGIN,
            MigrationError::Paused(_) => ERR_BRIDGE_PAUSED,
        }
    }
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::InvalidProof(s) => write!(f, "[{}] invalid proof: {}", self.code(), s),
            MigrationError::NullifierUsed(id) => {
                write!(f, "[{}] nullifier already used: 0x{}", self.code(), hex::encode(id))
            }
            MigrationError::BindingMismatch => write!(
                f,
                "[{}] amount/destination mismatch between claim and proven burn commitment",
                self.code()
            ),
            MigrationError::UnknownOriginChain(id) => {
                write!(f, "[{}] origin chain {} not recognized", self.code(), id)
            }
            MigrationError::Paused(reason) => {
                write!(f, "[{}] migration bridge paused: {}", self.code(), reason)
            }
        }
    }
}

impl std::error::Error for MigrationError {}

/// Lifecycle status of a migration, as returned by `rope_getMigrationStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationStatus {
    /// Burn observed, awaiting origin-chain finality.
    Pending,
    /// Finality reached; proof generated and ready for submission.
    ProofReady,
    /// Proof verified; mint in flight.
    Verified,
    /// Mint deferred by the minter's 24h sliding-window cap. The
    /// `FATMigrationMinter` auto-paused (`AutoPaused` event) WITHOUT
    /// consuming the nullifier, so the claim is resubmittable once
    /// governance unpauses. Relayer rule (per the DCSwap Phase 0c
    /// handover, 2026-07-08): after a `mintFromMigration` /
    /// `claimMigration` transaction succeeds, check
    /// `isNullifierUsed(burnId)` on the minter — `false` means the mint
    /// was deferred, NOT completed. Never report `Minted` on tx success
    /// alone.
    Deferred,
    /// Native FAT minted; nullifier consumed on the minter. Terminal.
    /// Only reachable after `isNullifierUsed(burnId)` returns `true`.
    Minted,
    /// Verification failed. The burn record persists; a fresh proof may be
    /// submitted (unless the failure was a consumed nullifier).
    Failed,
}

/// A migration claim as submitted to `rope_submitMigrationProof`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BurnClaim {
    /// Unique burn identifier emitted by the Origin Burn Contract:
    /// `keccak256(chainId ‖ txHash ‖ logIndex)`.
    pub burn_id: [u8; 32],
    /// EVM chain ID of the origin chain (1 or 50).
    pub origin_chain_id: u64,
    /// Amount burned, in wei units (18 decimals), 1:1 with the mint.
    pub amount: u128,
    /// Destination Datawallet (32 bytes: 20-byte EVM address left-padded,
    /// or a 32-byte Quipu string ID).
    pub destination: [u8; 32],
    /// Binary Merkle proof (format documented at module level).
    pub proof: Vec<u8>,
}

impl BurnClaim {
    /// The burn commitment this claim asserts. The proven `value` must
    /// equal this hash for verification to succeed.
    pub fn commitment(&self) -> [u8; 32] {
        burn_commitment(self.origin_chain_id, &self.burn_id, self.amount, &self.destination)
    }
}

/// Compute the burn commitment hash binding a burn event to its claim.
pub fn burn_commitment(
    origin_chain_id: u64,
    burn_id: &[u8; 32],
    amount: u128,
    destination: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(BURN_COMMITMENT_DOMAIN);
    hasher.update(&origin_chain_id.to_be_bytes());
    hasher.update(burn_id);
    hasher.update(&amount.to_be_bytes());
    hasher.update(destination);
    *hasher.finalize().as_bytes()
}

/// Classify the outcome of a minter transaction, encoding the
/// auto-pause protocol of `FATMigrationMinter` (DCSwap Phase 0c,
/// 2026-07-08): a window-cap breach flips the minter to paused and
/// returns success WITHOUT consuming the nullifier, so transaction
/// success alone never proves the mint happened. The relayer MUST call
/// this with the post-transaction `isNullifierUsed(burnId)` read before
/// reporting a terminal status.
pub fn classify_mint_outcome(tx_succeeded: bool, nullifier_used_after: bool) -> MigrationStatus {
    match (tx_succeeded, nullifier_used_after) {
        (true, true) => MigrationStatus::Minted,
        (true, false) => MigrationStatus::Deferred,
        // A failed tx never consumes the nullifier on-chain; if the
        // post-read still says consumed, an earlier tx already minted.
        (false, true) => MigrationStatus::Minted,
        (false, false) => MigrationStatus::Failed,
    }
}

/// A successfully verified migration, ready for the minter.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedMigration {
    pub burn_id: [u8; 32],
    pub origin_chain: OriginChain,
    pub amount: u128,
    pub destination: [u8; 32],
    /// BLAKE3 hash of the verified proof bytes — recorded in the
    /// provenance knot as `proofReference` (spec §7).
    pub proof_reference: [u8; 32],
    /// Unix timestamp (seconds) at verification.
    pub verified_at: u64,
}

/// Per-origin-chain caps (spec §A.7). Amounts in wei units.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct MigrationCaps {
    /// Maximum amount for a single migration.
    pub per_tx: u128,
    /// Maximum cumulative amount inside the sliding window.
    pub per_window: u128,
    /// Sliding-window length in seconds.
    pub window_secs: u64,
}

impl MigrationCaps {
    /// Phase-1 rollout caps (spec §A.7): 5,000,000 DC per transaction,
    /// 25,000,000 DC per 24 h window, per origin chain.
    pub fn phase1() -> Self {
        Self {
            per_tx: 5_000_000u128 * 10u128.pow(18),
            per_window: 25_000_000u128 * 10u128.pow(18),
            window_secs: 86_400,
        }
    }
}

/// Serializable snapshot of the verifier's consumable state, for
/// persistence across restarts. The nullifier set is safety-critical:
/// losing it would re-open every historical burn for double-minting, so
/// the host process MUST persist snapshots on every successful
/// verification and restore them at startup.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MigrationStateSnapshot {
    /// Consumed nullifiers (hex-encoded for stable JSON round-tripping).
    pub nullifiers: Vec<String>,
    /// Total migrated per origin chain id, in wei units (decimal strings —
    /// u128 exceeds JSON's safe integer range).
    pub total_migrated: HashMap<u64, String>,
    /// Whether the bridge is paused and why.
    pub paused: Option<String>,
}

/// Interior state guarded by one mutex so that proof verification,
/// nullifier consumption, cap accounting, and pause checks are a single
/// atomic decision (spec §8: nullifier checked atomically with the mint).
struct VerifierInner {
    nullifiers: HashSet<[u8; 32]>,
    /// Tracked state roots per origin chain, installed only via
    /// [`MigrationVerifier::update_state_root`].
    state_roots: HashMap<OriginChain, [u8; 32]>,
    /// Sliding window of (timestamp, amount) per chain for cap accounting.
    windows: HashMap<OriginChain, VecDeque<(u64, u128)>>,
    /// Running total of migrated supply per chain (public reconciliation
    /// input, spec §9).
    total_migrated: HashMap<OriginChain, u128>,
    paused: Option<String>,
}

/// The Rope-side migration verifier (spec §6.2 Bridge Verification Module).
pub struct MigrationVerifier {
    caps: MigrationCaps,
    inner: Mutex<VerifierInner>,
}

impl MigrationVerifier {
    /// Create a verifier with the given caps and no tracked roots.
    /// Proofs cannot verify until the light-client path installs a root
    /// for the relevant chain via [`Self::update_state_root`].
    pub fn new(caps: MigrationCaps) -> Self {
        Self {
            caps,
            inner: Mutex::new(VerifierInner {
                nullifiers: HashSet::new(),
                state_roots: HashMap::new(),
                windows: HashMap::new(),
                total_migrated: HashMap::new(),
                paused: None,
            }),
        }
    }

    /// Install/advance the tracked state root for an origin chain. This is
    /// the ONLY way roots enter the verifier; the caller must be the
    /// audited light-client update path (spec §8).
    pub fn update_state_root(&self, chain: OriginChain, root: [u8; 32]) {
        let mut inner = self.inner.lock();
        inner.state_roots.insert(chain, root);
        tracing::info!(
            "migration: state root updated for {:?} -> 0x{}",
            chain,
            hex::encode(root)
        );
    }

    /// Governance pause (Timelock-gated at the RPC layer).
    pub fn pause(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.inner.lock().paused = Some(reason.clone());
        tracing::warn!("migration: bridge PAUSED: {}", reason);
    }

    /// Governance unpause.
    pub fn unpause(&self) {
        self.inner.lock().paused = None;
        tracing::info!("migration: bridge unpaused");
    }

    /// Whether the bridge is currently paused.
    pub fn is_paused(&self) -> bool {
        self.inner.lock().paused.is_some()
    }

    /// Public view for clients to check a burn ID before resubmitting
    /// (spec §6.2 `isNullifierUsed`).
    pub fn is_nullifier_used(&self, burn_id: &[u8; 32]) -> bool {
        self.inner.lock().nullifiers.contains(burn_id)
    }

    /// Total migrated supply, summed across chains (spec §6.3
    /// `totalMigratedSupply`).
    pub fn total_migrated_supply(&self) -> u128 {
        self.inner.lock().total_migrated.values().sum()
    }

    /// Total migrated per origin chain (reconciliation feed input).
    pub fn total_migrated_by_chain(&self) -> HashMap<OriginChain, u128> {
        self.inner.lock().total_migrated.clone()
    }

    /// The most recent tracked state root for a chain (spec §6.2
    /// `getTrackedStateRoot`).
    pub fn tracked_state_root(&self, chain: OriginChain) -> Option<[u8; 32]> {
        self.inner.lock().state_roots.get(&chain).copied()
    }

    /// Export the consumable state for persistence.
    pub fn snapshot(&self) -> MigrationStateSnapshot {
        let inner = self.inner.lock();
        MigrationStateSnapshot {
            nullifiers: inner.nullifiers.iter().map(hex::encode).collect(),
            total_migrated: inner
                .total_migrated
                .iter()
                .map(|(chain, amt)| (chain.chain_id(), amt.to_string()))
                .collect(),
            paused: inner.paused.clone(),
        }
    }

    /// Restore consumable state from a persisted snapshot. Entries that
    /// fail to parse are rejected loudly rather than skipped: a partial
    /// nullifier restore silently re-opens double-mint windows.
    pub fn restore(&self, snapshot: &MigrationStateSnapshot) -> Result<(), MigrationError> {
        let mut nullifiers = HashSet::with_capacity(snapshot.nullifiers.len());
        for n in &snapshot.nullifiers {
            let bytes = hex::decode(n)
                .map_err(|e| MigrationError::InvalidProof(format!("snapshot nullifier: {e}")))?;
            let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                MigrationError::InvalidProof("snapshot nullifier: wrong length".into())
            })?;
            nullifiers.insert(arr);
        }
        let mut totals = HashMap::new();
        for (chain_id, amount) in &snapshot.total_migrated {
            let chain = OriginChain::from_chain_id(*chain_id)
                .ok_or(MigrationError::UnknownOriginChain(*chain_id))?;
            let amt: u128 = amount
                .parse()
                .map_err(|e| MigrationError::InvalidProof(format!("snapshot total: {e}")))?;
            totals.insert(chain, amt);
        }
        let mut inner = self.inner.lock();
        inner.nullifiers = nullifiers;
        inner.total_migrated = totals;
        inner.paused = snapshot.paused.clone();
        Ok(())
    }

    /// Verify a claim and, on success, atomically consume its nullifier
    /// and account it against the caps. This is THE critical section:
    /// between the nullifier check and its insertion no other claim can
    /// interleave, so a burn can never authorize two mints.
    ///
    /// `now_unix` is injected (rather than read inside) so the host can
    /// use its consensus clock and tests are deterministic.
    pub fn verify_and_consume(
        &self,
        claim: &BurnClaim,
        now_unix: u64,
    ) -> Result<VerifiedMigration, MigrationError> {
        // Resolve the origin chain before taking the lock — pure function.
        let chain = OriginChain::from_chain_id(claim.origin_chain_id)
            .ok_or(MigrationError::UnknownOriginChain(claim.origin_chain_id))?;

        if claim.amount == 0 {
            return Err(MigrationError::BindingMismatch);
        }

        let mut inner = self.inner.lock();

        // 1. Pause check (2005) — manual or cap-triggered.
        if let Some(reason) = &inner.paused {
            return Err(MigrationError::Paused(reason.clone()));
        }

        // 2. Nullifier check (2002) — before any expensive work.
        if inner.nullifiers.contains(&claim.burn_id) {
            return Err(MigrationError::NullifierUsed(claim.burn_id));
        }

        // 3. Tracked-root + Merkle verification (2001) and claim binding (2003).
        let tracked_root = inner
            .state_roots
            .get(&chain)
            .copied()
            .ok_or_else(|| MigrationError::InvalidProof(format!(
                "no tracked state root for {chain:?}; light client not synced"
            )))?;
        let parsed = parse_proof(&claim.proof)?;
        if parsed.root != tracked_root {
            return Err(MigrationError::InvalidProof(
                "proof root does not match tracked state root (stale or forged)".into(),
            ));
        }
        // The proven value must be the burn commitment for exactly this
        // claim. A valid inclusion proof for a *different* commitment
        // (wrong amount, wrong destination) is a binding mismatch (2003),
        // distinguishable from a malformed/stale proof (2001).
        if parsed.value.len() != 32 {
            return Err(MigrationError::InvalidProof(
                "proven value is not a 32-byte burn commitment".into(),
            ));
        }
        if !verify_merkle_path(&parsed) {
            return Err(MigrationError::InvalidProof("Merkle path verification failed".into()));
        }
        if parsed.value.as_slice() != claim.commitment() {
            return Err(MigrationError::BindingMismatch);
        }

        // 4. Caps (auto-pause on breach, spec §A.7).
        if claim.amount > self.caps.per_tx {
            let reason = format!(
                "per-tx cap exceeded on {:?}: {} > {}",
                chain, claim.amount, self.caps.per_tx
            );
            inner.paused = Some(reason.clone());
            tracing::error!("migration: AUTO-PAUSE: {}", reason);
            return Err(MigrationError::Paused(reason));
        }
        let window = inner.windows.entry(chain).or_default();
        let cutoff = now_unix.saturating_sub(self.caps.window_secs);
        while window.front().is_some_and(|(t, _)| *t < cutoff) {
            window.pop_front();
        }
        let window_total: u128 = window.iter().map(|(_, a)| *a).sum();
        if window_total.saturating_add(claim.amount) > self.caps.per_window {
            let reason = format!(
                "sliding-window cap exceeded on {:?}: {} + {} > {}",
                chain, window_total, claim.amount, self.caps.per_window
            );
            inner.paused = Some(reason.clone());
            tracing::error!("migration: AUTO-PAUSE: {}", reason);
            return Err(MigrationError::Paused(reason));
        }

        // 5. Consume — nullifier, window entry, running total; still inside
        //    the same lock scope as every check above.
        inner.nullifiers.insert(claim.burn_id);
        inner
            .windows
            .get_mut(&chain)
            .expect("window entry created above")
            .push_back((now_unix, claim.amount));
        *inner.total_migrated.entry(chain).or_insert(0) += claim.amount;

        let proof_reference = *blake3::hash(&claim.proof).as_bytes();
        tracing::info!(
            "migration: verified burn 0x{} on {:?} for {} wei -> dest 0x{}",
            hex::encode(claim.burn_id),
            chain,
            claim.amount,
            hex::encode(claim.destination),
        );

        Ok(VerifiedMigration {
            burn_id: claim.burn_id,
            origin_chain: chain,
            amount: claim.amount,
            destination: claim.destination,
            proof_reference,
            verified_at: now_unix,
        })
    }
}

/// Parsed binary proof (module-level format).
struct ParsedProof {
    root: [u8; 32],
    key: Vec<u8>,
    value: Vec<u8>,
    siblings: Vec<[u8; 32]>,
}

/// Parse the binary proof envelope. Structural failures are 2001.
fn parse_proof(proof: &[u8]) -> Result<ParsedProof, MigrationError> {
    if proof.len() < 40 {
        return Err(MigrationError::InvalidProof("proof too short".into()));
    }
    let root: [u8; 32] = proof[0..32].try_into().expect("length checked");
    let key_len = u32::from_be_bytes(proof[32..36].try_into().expect("length checked")) as usize;
    if proof.len() < 36 + key_len + 4 {
        return Err(MigrationError::InvalidProof("truncated key".into()));
    }
    let key = proof[36..36 + key_len].to_vec();
    let value_start = 36 + key_len;
    let value_len = u32::from_be_bytes(
        proof[value_start..value_start + 4]
            .try_into()
            .expect("length checked"),
    ) as usize;
    if proof.len() < value_start + 4 + value_len {
        return Err(MigrationError::InvalidProof("truncated value".into()));
    }
    let value = proof[value_start + 4..value_start + 4 + value_len].to_vec();
    let mut siblings = Vec::new();
    let mut i = value_start + 4 + value_len;
    while i + 32 <= proof.len() {
        siblings.push(proof[i..i + 32].try_into().expect("length checked"));
        i += 32;
    }
    if i != proof.len() {
        return Err(MigrationError::InvalidProof(
            "trailing bytes after last sibling".into(),
        ));
    }
    Ok(ParsedProof { root, key, value, siblings })
}

/// BLAKE3 binary-Merkle path verification — bit `i` of the key selects the
/// hashing order at level `i`. Identical construction to
/// [`crate::ethereum::EthereumBridge::verify_merkle_proof`], kept here as a
/// pure function so the verifier has no async/bridge dependency.
fn verify_merkle_path(p: &ParsedProof) -> bool {
    let mut current = blake3::hash(&p.value).as_bytes().to_vec();
    for (i, sibling) in p.siblings.iter().enumerate() {
        let mut hasher = blake3::Hasher::new();
        if (p.key.get(i / 8).unwrap_or(&0) >> (7 - (i % 8))) & 1 == 0 {
            hasher.update(&current);
            hasher.update(sibling);
        } else {
            hasher.update(sibling);
            hasher.update(&current);
        }
        current = hasher.finalize().as_bytes().to_vec();
    }
    current.as_slice() == p.root
}

/// Build the binary proof envelope from parts. Used by the relayer and by
/// tests; the on-wire format matches `parse_proof` exactly.
pub fn encode_proof(root: &[u8; 32], key: &[u8], value: &[u8], siblings: &[[u8; 32]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + key.len() + value.len() + siblings.len() * 32);
    out.extend_from_slice(root);
    out.extend_from_slice(&(key.len() as u32).to_be_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
    for s in siblings {
        out.extend_from_slice(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a claim + matching single-node proof whose root we control.
    /// With zero siblings the Merkle root is BLAKE3(value), so installing
    /// that root makes the proof verify — exercising the full path without
    /// needing a live light client.
    fn make_valid(
        chain: OriginChain,
        burn_seed: u8,
        amount: u128,
    ) -> (BurnClaim, [u8; 32]) {
        let burn_id = [burn_seed; 32];
        let destination = [0xDD; 32];
        let commitment = burn_commitment(chain.chain_id(), &burn_id, amount, &destination);
        let root = *blake3::hash(&commitment).as_bytes();
        let proof = encode_proof(&root, &[0u8; 4], &commitment, &[]);
        (
            BurnClaim {
                burn_id,
                origin_chain_id: chain.chain_id(),
                amount,
                destination,
                proof,
            },
            root,
        )
    }

    fn one_dc(n: u128) -> u128 {
        n * 10u128.pow(18)
    }

    #[test]
    fn happy_path_verifies_and_accounts() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (claim, root) = make_valid(OriginChain::Ethereum, 1, one_dc(1000));
        v.update_state_root(OriginChain::Ethereum, root);

        let out = v.verify_and_consume(&claim, 1_000_000).expect("must verify");
        assert_eq!(out.amount, one_dc(1000));
        assert_eq!(out.origin_chain, OriginChain::Ethereum);
        assert!(v.is_nullifier_used(&claim.burn_id));
        assert_eq!(v.total_migrated_supply(), one_dc(1000));
    }

    #[test]
    fn replay_rejected_2002() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (claim, root) = make_valid(OriginChain::Xdc, 2, one_dc(5));
        v.update_state_root(OriginChain::Xdc, root);

        v.verify_and_consume(&claim, 10).expect("first passes");
        let err = v.verify_and_consume(&claim, 11).unwrap_err();
        assert_eq!(err.code(), ERR_NULLIFIER_USED);
        // Replay must not double-account.
        assert_eq!(v.total_migrated_supply(), one_dc(5));
    }

    #[test]
    fn amount_tamper_rejected_2003() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (mut claim, root) = make_valid(OriginChain::Ethereum, 3, one_dc(10));
        v.update_state_root(OriginChain::Ethereum, root);

        claim.amount = one_dc(1_000_000); // inflate vs proven commitment
        let err = v.verify_and_consume(&claim, 10).unwrap_err();
        assert_eq!(err.code(), ERR_AMOUNT_MISMATCH);
        assert!(!v.is_nullifier_used(&claim.burn_id));
    }

    #[test]
    fn destination_tamper_rejected_2003() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (mut claim, root) = make_valid(OriginChain::Ethereum, 4, one_dc(10));
        v.update_state_root(OriginChain::Ethereum, root);

        claim.destination = [0xEE; 32]; // redirect vs proven commitment
        let err = v.verify_and_consume(&claim, 10).unwrap_err();
        assert_eq!(err.code(), ERR_AMOUNT_MISMATCH);
    }

    #[test]
    fn unknown_chain_rejected_2004() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (mut claim, _) = make_valid(OriginChain::Ethereum, 5, one_dc(1));
        claim.origin_chain_id = 56; // BSC — not a registered origin
        let err = v.verify_and_consume(&claim, 10).unwrap_err();
        assert_eq!(err.code(), ERR_UNKNOWN_ORIGIN);
    }

    #[test]
    fn paused_rejected_2005() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (claim, root) = make_valid(OriginChain::Ethereum, 6, one_dc(1));
        v.update_state_root(OriginChain::Ethereum, root);
        v.pause("governance drill");
        let err = v.verify_and_consume(&claim, 10).unwrap_err();
        assert_eq!(err.code(), ERR_BRIDGE_PAUSED);
        v.unpause();
        v.verify_and_consume(&claim, 11).expect("passes after unpause");
    }

    #[test]
    fn stale_root_rejected_2001() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (claim, _real_root) = make_valid(OriginChain::Ethereum, 7, one_dc(1));
        // Install a DIFFERENT root than the proof's.
        v.update_state_root(OriginChain::Ethereum, [0xAB; 32]);
        let err = v.verify_and_consume(&claim, 10).unwrap_err();
        assert_eq!(err.code(), ERR_INVALID_PROOF);
    }

    #[test]
    fn missing_root_rejected_2001() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (claim, _) = make_valid(OriginChain::Xdc, 8, one_dc(1));
        // No root installed for XDC at all.
        let err = v.verify_and_consume(&claim, 10).unwrap_err();
        assert_eq!(err.code(), ERR_INVALID_PROOF);
    }

    #[test]
    fn per_tx_cap_auto_pauses_2005() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (claim, root) = make_valid(OriginChain::Ethereum, 9, one_dc(5_000_001));
        v.update_state_root(OriginChain::Ethereum, root);
        let err = v.verify_and_consume(&claim, 10).unwrap_err();
        assert_eq!(err.code(), ERR_BRIDGE_PAUSED);
        assert!(v.is_paused(), "cap breach must auto-pause, not just reject");
    }

    #[test]
    fn window_cap_auto_pauses_and_window_slides() {
        let caps = MigrationCaps {
            per_tx: one_dc(10),
            per_window: one_dc(15),
            window_secs: 100,
        };
        let v = MigrationVerifier::new(caps);
        let (c1, r1) = make_valid(OriginChain::Ethereum, 10, one_dc(10));
        v.update_state_root(OriginChain::Ethereum, r1);
        v.verify_and_consume(&c1, 1000).expect("first ok");

        // Second claim inside the window pushes past 15 DC -> auto-pause.
        let (c2, r2) = make_valid(OriginChain::Ethereum, 11, one_dc(10));
        v.update_state_root(OriginChain::Ethereum, r2);
        let err = v.verify_and_consume(&c2, 1050).unwrap_err();
        assert_eq!(err.code(), ERR_BRIDGE_PAUSED);

        // After governance review + unpause and window expiry, it passes.
        v.unpause();
        v.verify_and_consume(&c2, 1101).expect("window slid, passes");
        assert_eq!(v.total_migrated_supply(), one_dc(20));
    }

    #[test]
    fn zero_amount_rejected() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (claim, root) = make_valid(OriginChain::Ethereum, 12, 0);
        v.update_state_root(OriginChain::Ethereum, root);
        let err = v.verify_and_consume(&claim, 10).unwrap_err();
        assert_eq!(err.code(), ERR_AMOUNT_MISMATCH);
    }

    #[test]
    fn snapshot_restore_round_trip() {
        let v = MigrationVerifier::new(MigrationCaps::phase1());
        let (claim, root) = make_valid(OriginChain::Ethereum, 13, one_dc(42));
        v.update_state_root(OriginChain::Ethereum, root);
        v.verify_and_consume(&claim, 10).expect("verifies");

        let snap = v.snapshot();
        let json = serde_json::to_string(&snap).expect("serializes");
        let snap2: MigrationStateSnapshot = serde_json::from_str(&json).expect("parses");

        let v2 = MigrationVerifier::new(MigrationCaps::phase1());
        v2.restore(&snap2).expect("restores");
        assert!(v2.is_nullifier_used(&claim.burn_id));
        assert_eq!(v2.total_migrated_supply(), one_dc(42));
        // Replay against the restored verifier must still be rejected.
        v2.update_state_root(OriginChain::Ethereum, root);
        let err = v2.verify_and_consume(&claim, 20).unwrap_err();
        assert_eq!(err.code(), ERR_NULLIFIER_USED);
    }

    #[test]
    fn multi_leaf_merkle_path_verifies() {
        // Two-level tree: leaf commitment hashed with one sibling.
        let chain = OriginChain::Xdc;
        let burn_id = [0x77; 32];
        let destination = [0xDD; 32];
        let amount = one_dc(3);
        let commitment = burn_commitment(chain.chain_id(), &burn_id, amount, &destination);

        let sibling = [0x11u8; 32];
        // Key bit 0 = 0 -> hash(current ‖ sibling)
        let leaf_hash = blake3::hash(&commitment);
        let mut h = blake3::Hasher::new();
        h.update(leaf_hash.as_bytes());
        h.update(&sibling);
        let root = *h.finalize().as_bytes();

        let proof = encode_proof(&root, &[0b0000_0000], &commitment, &[sibling]);
        let claim = BurnClaim {
            burn_id,
            origin_chain_id: chain.chain_id(),
            amount,
            destination,
            proof,
        };

        let v = MigrationVerifier::new(MigrationCaps::phase1());
        v.update_state_root(chain, root);
        v.verify_and_consume(&claim, 10).expect("two-level proof verifies");
    }

    #[test]
    fn error_codes_match_spec() {
        assert_eq!(MigrationError::InvalidProof(String::new()).code(), 2001);
        assert_eq!(MigrationError::NullifierUsed([0; 32]).code(), 2002);
        assert_eq!(MigrationError::BindingMismatch.code(), 2003);
        assert_eq!(MigrationError::UnknownOriginChain(0).code(), 2004);
        assert_eq!(MigrationError::Paused(String::new()).code(), 2005);
    }

    #[test]
    fn origin_chain_registry_is_the_verified_baseline() {
        assert_eq!(
            OriginChain::Ethereum.legacy_contract(),
            "0x0b44547be0a0df5dcd5327de8ea73680517c5a54"
        );
        assert_eq!(
            OriginChain::Xdc.legacy_contract(),
            "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a"
        );
        assert_eq!(OriginChain::Ethereum.chain_id(), 1);
        assert_eq!(OriginChain::Xdc.chain_id(), 50);
        assert_eq!(OriginChain::from_chain_id(1), Some(OriginChain::Ethereum));
        assert_eq!(OriginChain::from_chain_id(50), Some(OriginChain::Xdc));
        assert_eq!(OriginChain::from_chain_id(271828), None);
    }

    #[test]
    fn mint_outcome_classification_encodes_auto_pause_protocol() {
        // Tx success + nullifier consumed = the mint completed.
        assert_eq!(classify_mint_outcome(true, true), MigrationStatus::Minted);
        // Tx success + nullifier NOT consumed = the minter auto-paused on
        // the window cap and deferred the mint. Never report Minted here.
        assert_eq!(classify_mint_outcome(true, false), MigrationStatus::Deferred);
        // Tx failure but nullifier consumed = a prior tx already minted.
        assert_eq!(classify_mint_outcome(false, true), MigrationStatus::Minted);
        // Tx failure + nullifier untouched = plain failure.
        assert_eq!(classify_mint_outcome(false, false), MigrationStatus::Failed);
    }
}
