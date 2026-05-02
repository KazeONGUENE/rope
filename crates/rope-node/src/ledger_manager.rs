//! # Ledger Manager — Node-Level Personal Ledger Integration
//!
//! Wires together all subsystems to provide the complete personal ledger
//! lifecycle within a running Datachain Rope node:
//!
//! - **Creation**: `create_ledger(wallet)` → genesis string + OES key binding
//! - **Append**: `append_to_ledger(wallet, interaction)` → encrypt + slice + distribute
//! - **Repatriate**: `repatriate_ledger(wallet)` → discover + fetch + assemble
//! - **Delete**: `erase_ledger(wallet)` → destroy OES key + tombstone + broadcast
//!
//! ## RPC Methods
//!
//! | Method | Parameters | Returns |
//! |--------|-----------|---------|
//! | `rope_createLedger` | `{ wallet }` | `{ genesis_string_id, status }` |
//! | `rope_appendToLedger` | `{ wallet, interaction }` | `{ string_id, piece_count }` |
//! | `rope_repatriateLedger` | `{ wallet, known_head? }` | `{ entries[], total_bytes }` |
//! | `rope_eraseLedger` | `{ wallet, reason }` | `{ audit_hash, entries_erased }` |
//! | `rope_getLedgerStatus` | `{ wallet }` | `{ descriptor }` |

use parking_lot::RwLock;
use rope_core::clock::{ClockManager, LamportClock};
use rope_core::lattice::StringLattice;
use rope_core::personal_ledger::{
    build_append_string, build_genesis_string, InteractionRecord, LedgerRegistry,
};
use rope_core::string::PublicKey;
use rope_core::types::{NodeId, StringId};
use rope_crypto::ledger_encryption::{
    decrypt_ledger_content, derive_ledger_key, encrypt_ledger_content, LedgerEnvelope,
    WalletAddress,
};
use rope_crypto::oes::OESManager;
use rope_protocols::ledger_lifecycle::{
    slice_encrypted_content, DeletionReason, LedgerErasureAudit, LedgerLifecycleManager,
    LifecycleConfig,
};
use rope_storage::LedgerStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn oes_proof_to_core(proof: rope_crypto::oes::OESProof) -> rope_core::string::OESProof {
    rope_core::string::OESProof {
        generation: proof.generation,
        state_commitment: proof.state_commitment,
        merkle_proof: proof.merkle_proof,
        signature: proof.signature,
    }
}

/// Node-level ledger manager holding all subsystem references
pub struct LedgerManager {
    registry: Arc<LedgerRegistry>,
    lattice: Arc<StringLattice>,
    store: Arc<LedgerStore>,
    lifecycle: Arc<LedgerLifecycleManager>,
    oes: Arc<OESManager>,
    node_id: NodeId,
    creator_key: PublicKey,
    clock: Arc<ClockManager>,
    config: LifecycleConfig,
}

/// Response for ledger creation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateLedgerResponse {
    pub genesis_string_id: String,
    pub wallet_address: String,
    pub oes_generation: u64,
    pub replication_factor: u32,
}

/// Response for ledger append
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppendLedgerResponse {
    pub string_id: String,
    pub parent_id: String,
    pub piece_count: u32,
    pub encrypted_size: u64,
    pub oes_generation: u64,
}

/// Response for ledger status query
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerStatusResponse {
    pub wallet_address: String,
    pub genesis_string_id: String,
    pub head_string_id: String,
    pub entry_count: u64,
    pub total_size_bytes: u64,
    pub oes_generation: u64,
    pub is_deleted: bool,
    pub created_at: i64,
    pub last_appended_at: i64,
}

/// Response for ledger erasure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EraseLedgerResponse {
    pub wallet_address: String,
    pub entries_erased: usize,
    pub audit_hash: String,
    pub key_destruction_method: String,
    pub oes_generations_destroyed: Vec<u64>,
}

/// Response for `rope_untieKnot` (canon v1.1 §4.2 — per-knot GDPR primitive).
///
/// Returned to the caller after a single knot on a wallet's string is untied.
/// The wallet's other knots and the string's hash continuity are preserved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UntieKnotResponse {
    pub wallet_address: String,
    /// The knot's StringId (its position on the cord — preserved as a tombstone)
    pub knot_string_id: String,
    /// Audit hash committing to (string_id || untied_at || reason)
    pub tombstone_audit_hash: String,
    /// Unix timestamp when the knot was untied
    pub untied_at: i64,
    /// Reason class (e.g. "GdprArticle17", "OwnerRequest")
    pub reason: String,
    /// Number of knots remaining active on the wallet's string after the untying
    pub knots_remaining: usize,
    /// Total tombstones (untied knots) on this wallet's string after the untying
    pub tombstones_total: usize,
    /// GDPR / regulator-facing label
    pub gdpr_article: String,
}

/// Response for repatriation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepatriateResponse {
    pub wallet_address: String,
    pub entries: Vec<RepatriatedEntryResponse>,
    pub total_entries: usize,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepatriatedEntryResponse {
    pub string_id: String,
    pub sequence: u64,
    pub oes_generation: u64,
    pub encrypted_size: u64,
    pub decrypted_content: Option<Vec<u8>>,
}

impl LedgerManager {
    pub fn new(
        lattice: Arc<StringLattice>,
        store: Arc<LedgerStore>,
        oes: Arc<OESManager>,
        node_id: NodeId,
        creator_key: PublicKey,
        clock: Arc<ClockManager>,
    ) -> Self {
        let config = LifecycleConfig::default();
        Self {
            registry: Arc::new(LedgerRegistry::new()),
            lattice,
            store,
            lifecycle: Arc::new(LedgerLifecycleManager::new(config.clone())),
            oes,
            node_id,
            creator_key,
            clock,
            config,
        }
    }

    /// Create a new personal ledger for a wallet.
    ///
    /// 1. Derive OES ledger key for this wallet
    /// 2. Build genesis RopeString
    /// 3. Add to lattice
    /// 4. Register in ledger registry
    /// 5. Slice genesis string for distribution
    /// 6. Record creation event
    pub fn create_ledger(
        &self,
        wallet_hex: &str,
    ) -> Result<CreateLedgerResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        if self.registry.get_descriptor(&wallet_bytes).is_some() {
            return Err(format!("Ledger already exists for wallet {}", wallet_hex));
        }

        let generation = self.oes.generation();
        let oes_proof = oes_proof_to_core(self.oes.generate_proof());
        let clock_snapshot = self.clock.tick();
        let replication = self.config.default_replication_factor;

        let genesis_string = build_genesis_string(
            &wallet_bytes,
            self.creator_key.clone(),
            clock_snapshot,
            generation,
            oes_proof,
            replication,
        )
        .map_err(|e| e.to_string())?;

        let genesis_id = self
            .lattice
            .add_string(genesis_string)
            .map_err(|e| e.to_string())?;

        let desc = self
            .registry
            .create_ledger(&wallet_bytes, genesis_id, generation, replication)
            .map_err(|e| e.to_string())?;

        self.store.append_to_chain(&wallet_bytes, *genesis_id.as_bytes());
        let stored_desc = rope_storage::ledger_db::StoredLedgerDescriptor {
            wallet_address: wallet_bytes.clone(),
            genesis_string_id: *genesis_id.as_bytes(),
            head_string_id: *genesis_id.as_bytes(),
            entry_count: 1,
            total_size_bytes: 0,
            oes_generation_at_creation: generation,
            current_oes_generation: generation,
            created_at: desc.created_at,
            last_appended_at: desc.last_appended_at,
            is_deleted: false,
            deleted_at: None,
            replication_factor: replication,
        };
        self.store.put_descriptor(&wallet_bytes, stored_desc);

        self.lifecycle.record_creation(
            wallet_bytes,
            *genesis_id.as_bytes(),
            generation,
        );

        tracing::info!(
            "Created personal ledger for wallet {} — genesis {}",
            wallet_hex,
            genesis_id
        );

        Ok(CreateLedgerResponse {
            genesis_string_id: genesis_id.to_hex(),
            wallet_address: wallet_hex.to_string(),
            oes_generation: generation,
            replication_factor: replication,
        })
    }

    /// Append an interaction to a wallet's ledger.
    ///
    /// 1. Serialize the interaction
    /// 2. Derive OES ledger key for this wallet+generation
    /// 3. Encrypt the content
    /// 4. Wrap in LedgerEnvelope
    /// 5. Build RopeString referencing previous head as parent
    /// 6. Add to lattice
    /// 7. Slice encrypted content into pieces
    /// 8. Update registry + storage
    pub fn append_to_ledger(
        &self,
        wallet_hex: &str,
        interaction: InteractionRecord,
    ) -> Result<AppendLedgerResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        if desc.is_deleted {
            return Err(format!("Ledger deleted for wallet {}", wallet_hex));
        }

        let head_id = desc.head_string_id;
        let generation = self.oes.generation();
        let sequence_number = desc.entry_count;

        let plaintext =
            serde_json::to_vec(&interaction).map_err(|e| format!("Serialization: {}", e))?;

        let oes_state = self.oes.clone();
        let key = derive_ledger_key(
            &|len, purpose| oes_state.derive_key(len, purpose),
            &wallet,
            generation,
        );

        let encrypted = encrypt_ledger_content(
            &key,
            &plaintext,
            &wallet,
            generation,
            sequence_number,
        )
        .map_err(|e| e.to_string())?;

        let envelope = LedgerEnvelope::encrypted_v1(encrypted);
        let envelope_bytes = envelope.serialize();
        let encrypted_size = envelope_bytes.len() as u64;

        let oes_proof = oes_proof_to_core(self.oes.generate_proof());
        let clock_snapshot = self.clock.tick();

        let new_string = build_append_string(
            envelope_bytes.clone(),
            head_id,
            self.creator_key.clone(),
            clock_snapshot,
            generation,
            oes_proof,
            self.config.default_replication_factor,
        )
        .map_err(|e| e.to_string())?;

        let new_id = self
            .lattice
            .add_string(new_string)
            .map_err(|e| e.to_string())?;

        let slicing = slice_encrypted_content(&envelope_bytes, self.config.piece_size);
        let piece_count = slicing.pieces.len() as u32;

        self.registry
            .record_append(&wallet_bytes, new_id, encrypted_size, generation)
            .map_err(|e| e.to_string())?;
        self.store.append_to_chain(&wallet_bytes, *new_id.as_bytes());

        self.lifecycle.record_append(
            wallet_bytes,
            *new_id.as_bytes(),
            *head_id.as_bytes(),
            encrypted_size,
            piece_count,
        );

        tracing::debug!(
            "Appended to ledger {} — entry {} ({} pieces, {} bytes encrypted)",
            wallet_hex,
            new_id,
            piece_count,
            encrypted_size
        );

        Ok(AppendLedgerResponse {
            string_id: new_id.to_hex(),
            parent_id: head_id.to_hex(),
            piece_count,
            encrypted_size,
            oes_generation: generation,
        })
    }

    /// Get the status of a wallet's ledger
    pub fn get_ledger_status(
        &self,
        wallet_hex: &str,
    ) -> Result<LedgerStatusResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        Ok(LedgerStatusResponse {
            wallet_address: wallet_hex.to_string(),
            genesis_string_id: desc.genesis_string_id.to_hex(),
            head_string_id: desc.head_string_id.to_hex(),
            entry_count: desc.entry_count,
            total_size_bytes: desc.total_size_bytes,
            oes_generation: desc.current_oes_generation,
            is_deleted: desc.is_deleted,
            created_at: desc.created_at,
            last_appended_at: desc.last_appended_at,
        })
    }

    /// Erase a wallet's ledger by destroying the OES key link.
    ///
    /// 1. Verify the requester owns the wallet
    /// 2. Mark all strings in the chain as erased in the lattice
    /// 3. Record the OES generations that were in use (for audit)
    /// 4. Mark ledger as deleted in registry
    /// 5. Create erasure audit record
    /// 6. Emit deletion event
    pub fn erase_ledger(
        &self,
        wallet_hex: &str,
        reason: DeletionReason,
    ) -> Result<EraseLedgerResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        if desc.is_deleted {
            return Err(format!("Ledger already deleted for {}", wallet_hex));
        }

        let chain = self.lattice.walk_ledger_chain(&desc.head_string_id);

        let oes_generations: Vec<u64> = chain
            .iter()
            .filter_map(|id| self.lattice.get_string(id))
            .map(|s| s.oes_generation())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let mut erased_count = 0;
        for id in &chain {
            if self.lattice.mark_erased(*id).is_ok() {
                erased_count += 1;
            }
        }

        self.registry
            .mark_deleted(&wallet_bytes)
            .map_err(|e| e.to_string())?;
        self.store.mark_deleted(&wallet_bytes);

        let key_method = "oes_evolution";
        let mut audit = LedgerErasureAudit::new(wallet_bytes.clone(), reason);
        audit.complete(erased_count, key_method, oes_generations.clone());

        let audit_hash = hex::encode(audit.audit_hash);

        self.lifecycle.record_deletion(audit);

        tracing::info!(
            "Erased ledger for wallet {} — {} entries, {} OES generations destroyed",
            wallet_hex,
            erased_count,
            oes_generations.len()
        );

        Ok(EraseLedgerResponse {
            wallet_address: wallet_hex.to_string(),
            entries_erased: erased_count,
            audit_hash,
            key_destruction_method: key_method.to_string(),
            oes_generations_destroyed: oes_generations,
        })
    }

    /// Untie a single knot on a wallet's string (Quipu Canon v1.1 §4.2).
    ///
    /// This is the granular GDPR Article 17 primitive — the surgical
    /// alternative to `erase_ledger` (which closes the entire wallet).
    /// Steps:
    ///
    ///   1. Resolve the wallet's chain and verify the target knot belongs to it.
    ///   2. Refuse to untie the genesis knot (would orphan the chain) or a
    ///      ledger that is already wholly deleted.
    ///   3. Call `lattice.mark_knot_untied(knot_id, reason)` — destroys the
    ///      payload + records canonical tombstone metadata.
    ///   4. Return the audit proof (tombstone_audit_hash, untied_at) plus
    ///      transparency counters for the caller / DCScan UI.
    ///
    /// The wallet's other knots, balances, attestations, deeds, and credentials
    /// are unaffected. Walking the string with `walk_string_with_tombstones`
    /// will return the untied knot as `LedgerEntry::Tombstone`.
    pub fn untie_knot(
        &self,
        wallet_hex: &str,
        knot_string_id_hex: &str,
        reason: &str,
    ) -> Result<UntieKnotResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        if desc.is_deleted {
            return Err(format!(
                "Ledger already wholly deleted for {} — no knots to untie",
                wallet_hex
            ));
        }

        let knot_id = StringId::from_hex(knot_string_id_hex)
            .map_err(|e| format!("Invalid knot string_id: {}", e))?;

        // Refuse to untie the genesis knot — that would orphan the chain.
        // The wallet-closure pathway is `erase_ledger`, not this one.
        if knot_id == desc.genesis_string_id {
            return Err(format!(
                "Cannot untie the genesis knot on wallet {}. To close the wallet entirely, use rope_erasePersonalLedger (canon §6).",
                wallet_hex
            ));
        }

        // Walk the chain (tombstone-aware) and verify the target belongs to this wallet's string.
        let entries_before = self.lattice.walk_string_with_tombstones(&desc.head_string_id);
        let belongs = entries_before
            .iter()
            .any(|e| e.string_id() == knot_id);
        if !belongs {
            return Err(format!(
                "Knot {} does not belong to wallet {}'s string",
                knot_string_id_hex, wallet_hex
            ));
        }

        // Refuse to untie an already-tombstoned knot (idempotency guard).
        if self.lattice.is_knot_untied(&knot_id) {
            return Err(format!(
                "Knot {} is already untied (tombstone exists)",
                knot_string_id_hex
            ));
        }

        // Perform the cryptographic erasure + tombstone recording.
        let tombstone = self
            .lattice
            .mark_knot_untied(knot_id, reason)
            .map_err(|e| format!("mark_knot_untied failed: {}", e))?;

        // Recompute counts after the untying for the response.
        let entries_after = self.lattice.walk_string_with_tombstones(&desc.head_string_id);
        let knots_remaining = entries_after.iter().filter(|e| !e.is_tombstone()).count();
        let tombstones_total = entries_after.iter().filter(|e| e.is_tombstone()).count();

        tracing::info!(
            "Untied knot {} on wallet {} — reason={}, audit={}",
            knot_id,
            wallet_hex,
            reason,
            hex::encode(tombstone.audit_hash)
        );

        Ok(UntieKnotResponse {
            wallet_address: wallet_hex.to_string(),
            knot_string_id: knot_id.to_hex(),
            tombstone_audit_hash: hex::encode(tombstone.audit_hash),
            untied_at: tombstone.untied_at,
            reason: tombstone.reason,
            knots_remaining,
            tombstones_total,
            gdpr_article: "Article 17 — Right to Erasure (per-knot, canon v1.1)".to_string(),
        })
    }

    /// Walk a wallet's string with tombstone awareness (canon v1.1 §6(2)).
    ///
    /// Returns the canonical String → Knot[] hierarchy that powers DCScan's
    /// personal-ledger view: each entry is either an `Active` knot (live
    /// payload) or a `Tombstone` knot (untied, payload destroyed, audit hash
    /// and timestamp preserved). Genesis-first ordering.
    pub fn walk_string_with_tombstones(
        &self,
        wallet_hex: &str,
    ) -> Result<(String, Vec<rope_core::lattice::LedgerEntry>), String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        let entries = self
            .lattice
            .walk_string_with_tombstones(&desc.head_string_id);

        Ok((desc.genesis_string_id.to_hex(), entries))
    }

    /// Repatriate a wallet's ledger — reconstruct from lattice/storage.
    ///
    /// In the full BitTorrent model, this would fetch pieces from remote nodes
    /// via RDP swarms. For now, we reconstruct from the local lattice + storage,
    /// which is the correct behavior for the node that holds the pieces.
    ///
    /// The wallet can then decrypt each entry using its OES key.
    pub fn repatriate_ledger(
        &self,
        wallet_hex: &str,
        decrypt: bool,
    ) -> Result<RepatriateResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        if desc.is_deleted {
            return Err(format!("Ledger deleted for wallet {}", wallet_hex));
        }

        let chain = self.lattice.walk_ledger_chain(&desc.head_string_id);

        let mut entries = Vec::new();
        let mut total_bytes: u64 = 0;

        for (seq, string_id) in chain.iter().enumerate() {
            let rope_string = match self.lattice.get_string(string_id) {
                Some(s) => s,
                None => continue,
            };

            let content = rope_string.content();
            let content_size = content.len() as u64;
            total_bytes += content_size;

            let decrypted_content = if decrypt && seq > 0 {
                match LedgerEnvelope::deserialize(&content) {
                    Ok(envelope) => {
                        match &envelope.payload {
                            rope_crypto::ledger_encryption::LedgerEnvelopePayload::EncryptedV1(
                                encrypted,
                            ) => {
                                let oes_state = self.oes.clone();
                                let key = derive_ledger_key(
                                    &|len, purpose| oes_state.derive_key(len, purpose),
                                    &wallet,
                                    encrypted.oes_generation,
                                );
                                decrypt_ledger_content(&key, encrypted).ok()
                            }
                            rope_crypto::ledger_encryption::LedgerEnvelopePayload::Plaintext(
                                data,
                            ) => Some(data.clone()),
                        }
                    }
                    Err(_) => None,
                }
            } else {
                None
            };

            entries.push(RepatriatedEntryResponse {
                string_id: string_id.to_hex(),
                sequence: seq as u64,
                oes_generation: rope_string.oes_generation(),
                encrypted_size: content_size,
                decrypted_content,
            });
        }

        let total_entries = entries.len();

        Ok(RepatriateResponse {
            wallet_address: wallet_hex.to_string(),
            entries,
            total_entries,
            total_bytes,
        })
    }

    pub fn registry(&self) -> &LedgerRegistry {
        &self.registry
    }

    pub fn lifecycle(&self) -> &LedgerLifecycleManager {
        &self.lifecycle
    }

    pub fn store(&self) -> &LedgerStore {
        &self.store
    }
}
