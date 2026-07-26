# Reth `state-edit` subcommand patch — Datachain Rope

Companion to `contracts/src/governance/UntieRegistry.sol` and the
2026-06-22 incident post-mortem at
`docs/INCIDENT_2026-06-22_FOUNDATION_TREASURY_DRAIN.md`.

## What this is

A minimal patch to the upstream Reth source (`v1.11.2`) that adds one
new subcommand to the `reth` binary:

```
reth state-edit \
    --datadir <path>                                       \
    --attacker  <address>                                  \
    --rescue    <address>                                  \
    --amount-wei <U256>                                    \
    --declared-prev-state-root <B256>                      \
    --untie-registry-address     <address>                 \
    --untie-registry-record-index <U256>                   \
    --justification-cid <CID>                              \
    --i-have-read-the-untie-registry-event \
        "I have read the UntieRegistry event on chain 271828"  \
    [--dry-run]
```

It atomically debits the attacker account and credits the rescue
account by `--amount-wei`, using Reth's own
`reth_db_common::init::insert_state` (the same code path Reth uses for
genesis initialisation). This updates `PlainAccountState`, `HashedAccounts`,
and `AccountChangeSet[head_block]` consistently — including Reth's
internal change-set so block history reflects the irregular state change.

## Why a Reth subcommand instead of a standalone binary

`insert_state` is the canonical, audited path for writing accounts into
Reth's MDBX. It handles trie consistency, change-set history, static
files, and the provider transaction model in one well-tested function.
Re-implementing that from outside the Reth tree would require pinning
`reth-db-api`, `reth-provider`, `reth-trie`, `reth-primitives-traits`,
`reth-db-common`, `reth-node-api`, and `reth-cli` to exactly v1.11.2,
duplicating Reth's commit/static-file logic, and re-verifying it
against Reth's invariants. Adding a single subcommand to Reth itself
is one file + two lines of wiring and reuses the entire upstream code
path.

## Files in this patch

| File | Destination in Reth tree |
|---|---|
| `state_edit_mod.rs` | `crates/cli/commands/src/state_edit/mod.rs` |
| `lib_addition.rs.diff` | `crates/cli/commands/src/lib.rs` (add `pub mod state_edit;`) |
| `cli_wiring.rs.diff` | `bin/reth/src/cli/mod.rs` (add the subcommand to the `Commands` enum) |

The two `.diff` files are tiny (one line each); see the next section.

## Wiring

In `crates/cli/commands/src/lib.rs`, add to the existing list of
`pub mod` declarations (alphabetically between `re_execute` and `stage`):

```rust
pub mod re_execute;
+ pub mod state_edit;
pub mod stage;
```

In `bin/reth/src/cli/mod.rs`, locate the `Commands` enum (whose variants
match the existing subcommands: `Node`, `Init`, `InitState`, `Import`,
`Db`, `Stage`, `P2P`, `Config`, `Debug`, `Recover`, `Prune`,
`ReExecute`, `Download`, `DumpGenesis`, `ExportEra`, `ImportEra`,
`TestVectors`) and add one variant near `InitState`:

```rust
    /// Initialize the database from a genesis-style state dump.
    InitState(reth_cli_commands::init_state::InitStateCommand<C>),
+
+   /// Apply a single, audited, two-account native-balance delta to the local
+   /// MDBX. Datachain Rope `rope_untieTx` execution layer. EXTREMELY DANGEROUS;
+   /// see `state_edit_mod.rs` doc-comment.
+   StateEdit(reth_cli_commands::state_edit::StateEditCommand<C>),

    /// Import a chain from a file.
    Import(reth_cli_commands::import::ImportCommand<C>),
```

…and add a matching dispatch arm where `Commands::InitState(cmd) =>
runner.run_blocking_until_ctrl_c(cmd.execute::<N>())` is:

```rust
    Commands::InitState(cmd) => runner.run_blocking_until_ctrl_c(cmd.execute::<N>()),
+   Commands::StateEdit(cmd) => runner.run_blocking_until_ctrl_c(cmd.execute::<N>()),
    Commands::Import(cmd)    => runner.run_blocking_until_ctrl_c(cmd.execute::<N, _>(executor)),
```

Both diffs are mechanical; the apply step does them via `sed`.

## Apply, build, deploy

The script `apply_and_build.sh` (next to this README) does:

  1. `cp state_edit_mod.rs /tmp/reth-fork/crates/cli/commands/src/state_edit/mod.rs`
  2. Add the `pub mod state_edit;` line to `lib.rs`.
  3. Add the two CLI wiring snippets to `bin/reth/src/cli/mod.rs`.
  4. `cd /tmp/reth-fork && cargo build --release -p reth`.
  5. On success: copy the built binary to `~/datachain-rope/target/release/reth-rope`.

Build time on rope-vps (8 GB RAM, ~14 GB swap, recent toolchain): 30-45 minutes
first build, 2-5 minutes incremental.

## Operational gate

This binary refuses to run unless:

  - `--i-have-read-the-untie-registry-event` is exactly
    `"I have read the UntieRegistry event on chain 271828"` (one-shot
    operator gate; prevents accidental invocation).
  - chain_id is exactly `271828`.
  - `--declared-prev-state-root` equals the MDBX head's state root
    (catches stale invocation if the chain advanced after the
    UntieRegistry event was mined).
  - The attacker's existing balance is >= `--amount-wei`.
  - `--attacker != --rescue` and neither is the zero address.
  - `--amount-wei != 0`.

All checks happen before any MDBX write. If any fails, the binary
exits non-zero with a precise message and the database is untouched.

`--dry-run` runs all checks and prints the would-be result without
committing the transaction. Run dry-run first on a copy of MDBX, then
again on each production node before the real invocation.

## Cross-node convergence

After applying the edit on all four nodes, the printed
`STATE_EDIT_RESULT` lines from each node must be byte-identical for
the `attacker_after_wei`, `rescue_after_wei`, and (after restart) the
next block's state root. If any node diverges, all four must be
reverted from pre-edit MDBX backup before any node's Reth is
restarted.

## Audit trail

This binary writes NO commitment of its own. The on-chain
`UntieRegistry.UntieRecorded` event (mined BEFORE the binary is run)
is the authorisation declaration; the on-chain
`UntieRegistry.UntieStateDeltaConfirmed` event (mined AFTER the binary
runs and Reth restarts on the new state) is the confirmation that the
actual post-state root matches the declared one. The binary's stdout
`STATE_EDIT_RESULT` lines are the ephemeral operator log only; they
are not the audit record.

— Datachain Foundation, 2026-06-30
