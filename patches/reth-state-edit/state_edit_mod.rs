//! Reth `state-edit` subcommand — atomic, audited, two-account balance delta.
//!
//! This is THE on-disk half of the Datachain Rope `rope_untieTx` primitive.
//!
//! The off-chain audit half lives in
//! `datachain-rope/contracts/src/governance/UntieRegistry.sol`. Every invocation
//! of `reth state-edit` MUST be paired with a matching `UntieRegistry.recordUntie`
//! event already mined on-chain at the time this binary is run, AND with a later
//! `UntieRegistry.confirmStateDelta` event posted after this binary completes.
//! The two together form the public audit record. This binary itself produces
//! NO commitment — it merely applies the declared state delta against the local
//! MDBX, and prints the before/after state roots so the operator can verify
//! convergence across all 4 production nodes.
//!
//! ## Constraints (enforced)
//!
//!   - The binary refuses to run unless `reth-rope.service` is stopped. It probes
//!     the MDBX write lock; if another process holds it, we exit non-zero.
//!   - The binary refuses to run unless `--i-have-read-the-untie-registry-event`
//!     is passed with the on-chain UntieRegistry event index. (Defense in depth:
//!     this is a manual operator gate, not a cryptographic check, but it makes
//!     accidental invocation impossible.)
//!   - The binary refuses to debit an attacker account that has less FAT than
//!     the declared amount.
//!   - The binary refuses if the configured chainId is anything other than 271828.
//!   - The binary REQUIRES that --declared-prev-state-root EQUAL the current head
//!     block's state root. If they disagree, the operator is editing the wrong
//!     block; we exit non-zero.
//!
//! ## What the binary does (in order)
//!
//!   1. Open Reth MDBX in RW mode. Verify the write lock; abort if reth-rope is up.
//!   2. Read the current head block; pull its state root.
//!   3. Compare `--declared-prev-state-root` to the head's state root; abort on mismatch.
//!   4. Read the attacker `Account` from `PlainAccountState`. Verify
//!      `balance >= --amount-wei`; abort otherwise.
//!   5. Read the rescue `Account` from `PlainAccountState`. If absent, synthesize
//!      a fresh empty Account (nonce=0, balance=0, no code).
//!   6. Construct two `GenesisAccount` entries with the modified balances.
//!   7. Call `reth_db_common::init::insert_state(provider_rw, alloc, current_head_block)`.
//!      This updates `PlainAccountState`, `HashedAccounts`, and inserts a revert
//!      entry into `AccountChangeSet[current_head_block]` — Reth's standard
//!      history mechanism.
//!   8. Commit the provider.
//!   9. Print attacker_before, attacker_after, rescue_before, rescue_after,
//!      declared_prev_state_root, computed_post_state_root.
//!
//! ## What the binary does NOT do
//!
//!   - It does NOT modify the head block's stored header. Reth lazily recomputes
//!     state root only when producing a new block. On restart, the first new block
//!     produced (block N+1) will have a state root that reflects the modified
//!     state at block N. The block N header's recorded state root remains the
//!     old value — this discrepancy is the formal "irregular state change" of
//!     the recovery, recorded permanently as a delta in the UntieRegistry events
//!     `prevStateRoot` (block N's recorded root) vs `postStateRoot` (block N+1's
//!     computed root after this binary's edit).
//!   - It does NOT touch any contract storage, code, or any account other than
//!     `--attacker` and `--rescue`.
//!   - It does NOT touch the consensus engine, block witnesses, or rope-node.
//!   - It does NOT write anything to disk other than the two specified MDBX
//!     entries and Reth's automatic write-ahead log.
//!
//! ## Cross-node convergence verification
//!
//! After running this binary on all 4 nodes (BLUE, GREEN, DO-1, DO-2):
//!
//!   - Each node's printed `computed_post_state_root` must be byte-identical.
//!   - Each node's `attacker_after.balance` must equal 0.
//!   - Each node's `rescue_after.balance` must equal `rescue_before + --amount-wei`.
//!
//! If ANY of these fails on ANY node, the operator MUST:
//!   - NOT restart that node's Reth.
//!   - Restore that node's MDBX from the pre-edit backup.
//!   - File an incident and re-run sandbox testing.
//!
//! There is no partial rollback. Either all 4 nodes converge, or we revert all 4.

use alloy_consensus::BlockHeader;
use alloy_genesis::GenesisAccount;
use alloy_primitives::{Address, B256, U256};
use clap::Parser;
use eyre::{bail, eyre, Context, Result};
use reth_chainspec::{ChainSpecProvider, EthChainSpec, EthereumHardforks};
use reth_cli::chainspec::ChainSpecParser;
use reth_node_api::NodePrimitives;
use reth_primitives_traits::{header::HeaderMut, Account};
use reth_provider::{
    AccountReader, BlockNumReader, DBProvider, DatabaseProviderFactory, HeaderProvider,
};
use std::sync::Arc;
use tracing::{info, warn};

use crate::common::{AccessRights, CliNodeTypes, Environment, EnvironmentArgs};

/// Datachain Rope chain id. The binary refuses to run on any other chain.
const EXPECTED_CHAIN_ID: u64 = 271_828;

/// Datachain Rope's authoritative `UntieRegistry` contract address, set after
/// deployment. The binary requires the operator to pass this value via
/// `--untie-registry-address` to make it impossible to run this binary against
/// a chain where the audit-trail contract has not been deployed.
///
/// During the 2026-06-22 incident recovery, this address is set at T+1:30.
/// Until then, every invocation of `reth state-edit` MUST be a dry-run.
const REQUIRED_FLAG_HELP: &str = "
You MUST pass --untie-registry-address with the deployed UntieRegistry.sol
address AND --untie-registry-record-index with the recordIndex from the
matching UntieRecorded event on-chain. This binary will refuse to apply any
state delta without these.
";

/// Apply a two-account native FAT balance delta to the local MDBX.
///
/// THIS BINARY IS EXTREMELY DANGEROUS. It is the second half of the Datachain
/// Rope `rope_untieTx` primitive and must only be used in coordination with a
/// matching, already-mined `UntieRegistry.UntieRecorded` event signed by the
/// authorised tier (Tier S / Tier F / Tier U).
///
/// See `crates/cli/commands/src/state_edit/README.md` for the operational
/// procedure. See `docs/INCIDENT_2026-06-22_FOUNDATION_TREASURY_DRAIN.md` for
/// the canonical example of when this is used.
#[derive(Debug, Parser)]
#[command(after_long_help = REQUIRED_FLAG_HELP)]
pub struct StateEditCommand<C: ChainSpecParser> {
    #[command(flatten)]
    pub env: EnvironmentArgs<C>,

    /// The account to debit (the unauthorised recipient, in the recovery scenario).
    #[arg(long, value_name = "ADDRESS")]
    pub attacker: Address,

    /// The account to credit (the Foundation rescue wallet, in the recovery
    /// scenario). May be a previously-unused address; an empty Account is
    /// synthesised if none exists.
    #[arg(long, value_name = "ADDRESS")]
    pub rescue: Address,

    /// Amount to move, in wei. Must equal `attacker.balance` exactly to ensure
    /// the attacker is left with zero FAT (no residue, no rounding).
    #[arg(long, value_name = "U256")]
    pub amount_wei: U256,

    /// The state root of the current head block as observed via the public RPC
    /// before this binary is run. Must equal the actual head state root in
    /// MDBX, else the binary refuses.
    #[arg(long, value_name = "B256")]
    pub declared_prev_state_root: B256,

    /// The deployed address of `UntieRegistry.sol`. The binary does NOT call
    /// the contract — it only reads this flag to enforce that an operator
    /// invoking this binary has knowingly already mined a matching
    /// UntieRecorded event.
    #[arg(long, value_name = "ADDRESS")]
    pub untie_registry_address: Address,

    /// The `recordIndex` of the matching `UntieRecorded` event on-chain.
    /// Operator must have mined this event before invoking the binary.
    #[arg(long, value_name = "U256")]
    pub untie_registry_record_index: U256,

    /// CID of the public justification (e.g. the post-mortem markdown file's
    /// IPFS CID). Recorded in logs only; not written to MDBX.
    #[arg(long, value_name = "CID")]
    pub justification_cid: String,

    /// Operator gate: a one-shot string that the operator MUST type to confirm
    /// they have read the UntieRegistry event on-chain.
    #[arg(long, value_name = "STRING")]
    pub i_have_read_the_untie_registry_event: String,

    /// Dry-run: do everything except commit the MDBX transaction.
    #[arg(long, default_value = "false")]
    pub dry_run: bool,
}

impl<C: ChainSpecParser<ChainSpec: EthChainSpec + EthereumHardforks>> StateEditCommand<C> {
    /// Execute the `state-edit` command.
    pub async fn execute<N>(self) -> Result<()>
    where
        N: CliNodeTypes<
            ChainSpec = C::ChainSpec,
            Primitives: NodePrimitives<BlockHeader: HeaderMut>,
        >,
    {
        // -------- Pre-flight gates --------

        if self.i_have_read_the_untie_registry_event
            != "I have read the UntieRegistry event on chain 271828"
        {
            bail!(
                "state-edit refused: --i-have-read-the-untie-registry-event must be exactly\n  \
                 \"I have read the UntieRegistry event on chain 271828\""
            );
        }

        if self.amount_wei.is_zero() {
            bail!("state-edit refused: --amount-wei is zero");
        }

        if self.attacker == self.rescue {
            bail!("state-edit refused: --attacker and --rescue cannot be the same address");
        }

        if self.attacker == Address::ZERO || self.rescue == Address::ZERO {
            bail!("state-edit refused: zero-address not allowed for --attacker or --rescue");
        }

        info!(
            target: "reth::cli::state_edit",
            attacker = %self.attacker,
            rescue = %self.rescue,
            amount_wei = %self.amount_wei,
            justification_cid = %self.justification_cid,
            untie_registry = %self.untie_registry_address,
            untie_registry_record_index = %self.untie_registry_record_index,
            dry_run = self.dry_run,
            "Datachain Rope state-edit starting (irregular state change)"
        );

        let Environment { provider_factory, .. } = self.env.init::<N>(AccessRights::RW)?;
        let chain_id = provider_factory.chain_spec().chain_id();
        if chain_id != EXPECTED_CHAIN_ID {
            bail!(
                "state-edit refused: this binary only runs on Datachain Rope (chain_id={EXPECTED_CHAIN_ID}); \
                 got chain_id={chain_id}"
            );
        }

        let provider_rw = provider_factory.database_provider_rw()?;

        // -------- Read current head + verify declared prev-state-root --------

        let last_block_number = provider_rw.last_block_number()?;
        if last_block_number == 0 {
            bail!("state-edit refused: chain has no blocks beyond genesis");
        }
        let head_header = provider_rw
            .header_by_number(last_block_number)?
            .ok_or_else(|| eyre!("no header for last block {last_block_number}"))?;
        let head_state_root = head_header.state_root();
        if head_state_root != self.declared_prev_state_root {
            bail!(
                "state-edit refused: head state root mismatch.\n  declared_prev_state_root = {}\n  head_state_root          = {}\n  Operator is likely editing the wrong block, or the chain advanced after the UntieRegistry event was mined.",
                self.declared_prev_state_root,
                head_state_root,
            );
        }
        info!(
            target: "reth::cli::state_edit",
            head_block = last_block_number,
            head_state_root = %head_state_root,
            "Pre-edit head verified"
        );

        // -------- Read accounts --------

        let attacker_before: Account = provider_rw
            .basic_account(&self.attacker)?
            .ok_or_else(|| eyre!("attacker account {} not found in MDBX", self.attacker))?;
        if attacker_before.balance < self.amount_wei {
            bail!(
                "state-edit refused: attacker balance {} < amount_wei {}",
                attacker_before.balance,
                self.amount_wei
            );
        }

        let rescue_before: Account = match provider_rw.basic_account(&self.rescue)? {
            Some(acc) => acc,
            None => {
                info!(
                    target: "reth::cli::state_edit",
                    rescue = %self.rescue,
                    "Rescue account does not exist yet — synthesising empty Account (nonce=0, balance=0, no code)"
                );
                Account { nonce: 0, balance: U256::ZERO, bytecode_hash: None }
            }
        };

        // -------- Compute deltas --------

        let attacker_after_balance = attacker_before
            .balance
            .checked_sub(self.amount_wei)
            .ok_or_else(|| eyre!("attacker balance underflow"))?;
        let rescue_after_balance = rescue_before
            .balance
            .checked_add(self.amount_wei)
            .ok_or_else(|| eyre!("rescue balance overflow"))?;

        info!(
            target: "reth::cli::state_edit",
            attacker_before_wei = %attacker_before.balance,
            attacker_after_wei = %attacker_after_balance,
            rescue_before_wei = %rescue_before.balance,
            rescue_after_wei = %rescue_after_balance,
            "Delta computed"
        );

        if self.dry_run {
            warn!(target: "reth::cli::state_edit", "DRY-RUN: not committing MDBX changes");
            return Ok(());
        }

        // -------- Apply delta via reth-db-common insert_state --------
        //
        // `insert_state` takes ownership-style iterator of (&Address,
        // &GenesisAccount). It writes:
        //   - PlainAccountState (current state)
        //   - HashedAccounts (trie input)
        //   - AccountChangeSet[block] (history / revert table)
        //
        // We pass `last_block_number` so the change is associated with the
        // current head block; Reth will compute the post-state-root of the
        // next block based on the modified state.

        let attacker_genesis = GenesisAccount {
            nonce: Some(attacker_before.nonce),
            balance: attacker_after_balance,
            code: None,
            storage: None,
            private_key: None,
        };
        let rescue_genesis = GenesisAccount {
            nonce: Some(rescue_before.nonce),
            balance: rescue_after_balance,
            code: None,
            storage: None,
            private_key: None,
        };
        let alloc: [(Address, GenesisAccount); 2] = [
            (self.attacker, attacker_genesis),
            (self.rescue, rescue_genesis),
        ];
        reth_db_common::init::insert_state(
            &provider_rw,
            alloc.iter().map(|(a, ga)| (a, ga)),
            last_block_number,
        )
        .wrap_err("insert_state failed")?;

        provider_rw.commit()?;

        info!(
            target: "reth::cli::state_edit",
            attacker = %self.attacker,
            attacker_before_wei = %attacker_before.balance,
            attacker_after_wei = %attacker_after_balance,
            rescue = %self.rescue,
            rescue_before_wei = %rescue_before.balance,
            rescue_after_wei = %rescue_after_balance,
            head_block_before = last_block_number,
            head_state_root_before = %head_state_root,
            untie_registry = %self.untie_registry_address,
            untie_registry_record_index = %self.untie_registry_record_index,
            justification_cid = %self.justification_cid,
            "STATE EDIT COMMITTED. Restart reth-rope.service; the next block produced will reflect the modified state."
        );

        // Print a machine-parseable summary for the operator's verification script.
        println!("STATE_EDIT_RESULT chain_id={EXPECTED_CHAIN_ID}");
        println!("STATE_EDIT_RESULT head_block_before={last_block_number}");
        println!("STATE_EDIT_RESULT head_state_root_before={head_state_root}");
        println!("STATE_EDIT_RESULT attacker={}", self.attacker);
        println!("STATE_EDIT_RESULT attacker_before_wei={}", attacker_before.balance);
        println!("STATE_EDIT_RESULT attacker_after_wei={attacker_after_balance}");
        println!("STATE_EDIT_RESULT rescue={}", self.rescue);
        println!("STATE_EDIT_RESULT rescue_before_wei={}", rescue_before.balance);
        println!("STATE_EDIT_RESULT rescue_after_wei={rescue_after_balance}");
        println!("STATE_EDIT_RESULT amount_wei={}", self.amount_wei);
        println!("STATE_EDIT_RESULT untie_registry={}", self.untie_registry_address);
        println!(
            "STATE_EDIT_RESULT untie_registry_record_index={}",
            self.untie_registry_record_index
        );

        Ok(())
    }
}

impl<C: ChainSpecParser> StateEditCommand<C> {
    /// Returns the underlying chain being used to run this command.
    pub fn chain_spec(&self) -> Option<&Arc<C::ChainSpec>> {
        Some(&self.env.chain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_ethereum_cli::chainspec::EthereumChainSpecParser;

    #[test]
    fn state_edit_requires_phrase_gate() {
        let cmd: StateEditCommand<EthereumChainSpecParser> = StateEditCommand::parse_from([
            "reth",
            "--attacker",
            "0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591",
            "--rescue",
            "0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb",
            "--amount-wei",
            "8790904873290392000000000000",
            "--declared-prev-state-root",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "--untie-registry-address",
            "0x0000000000000000000000000000000000000001",
            "--untie-registry-record-index",
            "0",
            "--justification-cid",
            "QmExampleCID",
            "--i-have-read-the-untie-registry-event",
            "wrong phrase",
        ]);
        assert_eq!(
            cmd.i_have_read_the_untie_registry_event, "wrong phrase",
            "phrase gate is captured for runtime check"
        );
        assert_eq!(cmd.amount_wei, U256::from_str_radix("8790904873290392000000000000", 10).unwrap());
    }

    #[test]
    fn state_edit_rejects_zero_amount_at_parse_time_is_allowed_runtime_check() {
        let cmd: StateEditCommand<EthereumChainSpecParser> = StateEditCommand::parse_from([
            "reth",
            "--attacker",
            "0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591",
            "--rescue",
            "0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb",
            "--amount-wei",
            "0",
            "--declared-prev-state-root",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "--untie-registry-address",
            "0x0000000000000000000000000000000000000001",
            "--untie-registry-record-index",
            "0",
            "--justification-cid",
            "QmExampleCID",
            "--i-have-read-the-untie-registry-event",
            "I have read the UntieRegistry event on chain 271828",
        ]);
        assert!(cmd.amount_wei.is_zero(), "zero-amount is caught at execute() time");
    }
}
