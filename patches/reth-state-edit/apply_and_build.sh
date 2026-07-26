#!/usr/bin/env bash
#
# Apply the Datachain Rope reth `state-edit` subcommand patch to a Reth source
# tree at /tmp/reth-fork (the canonical sandbox on rope-vps), and build the
# patched `reth` binary.
#
# Reth v1.11.2 patch points (verified against the cloned source 2026-06-30):
#   1) crates/cli/commands/src/lib.rs                   -- add `pub mod state_edit;`
#   2) crates/ethereum/cli/src/interface.rs             -- 3 edits:
#        2a) imports: add `state_edit,` to the `reth_cli_commands::{...}` block
#        2b) variant: add `StateEdit(state_edit::StateEditCommand<C>)` after InitState
#        2c) chain_spec arm: add `Self::StateEdit(cmd) => cmd.chain_spec()`
#   3) crates/ethereum/cli/src/app.rs                   -- dispatch arm after Commands::InitState
#
# Idempotent: re-running this script is safe; it detects each patch already
# applied and skips.
#
# Usage:
#   ssh rope-vps
#   bash /tmp/reth-state-edit-patch/apply_and_build.sh
#
# Exit codes:
#   0 — patch applied (or already applied) and build succeeded
#   1 — Reth source tree not found at /tmp/reth-fork
#   2 — patch source files missing
#   3 — wiring substitution failed (Reth Commands enum shape changed)
#   4 — cargo build failed

set -euo pipefail

RETH_FORK="${RETH_FORK:-/tmp/reth-fork}"
PATCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_BIN="${OUTPUT_BIN:-${HOME}/datachain-rope/target/release/reth-rope-state-edit}"

export PATH="${HOME}/.cargo/bin:$PATH"

if [[ ! -d "$RETH_FORK" ]]; then
    echo "FATAL: Reth source not found at $RETH_FORK"
    echo "Clone first: git clone --depth 100 --branch v1.11.2 https://github.com/paradigmxyz/reth.git $RETH_FORK"
    exit 1
fi

if [[ ! -f "$PATCH_DIR/state_edit_mod.rs" ]]; then
    echo "FATAL: state_edit_mod.rs not found at $PATCH_DIR"
    exit 2
fi

echo "==> Step 1/6: copy state_edit_mod.rs into Reth tree"
DEST_MOD_DIR="$RETH_FORK/crates/cli/commands/src/state_edit"
mkdir -p "$DEST_MOD_DIR"
cp "$PATCH_DIR/state_edit_mod.rs" "$DEST_MOD_DIR/mod.rs"
echo "  wrote $DEST_MOD_DIR/mod.rs ($(wc -l < "$DEST_MOD_DIR/mod.rs") lines)"

echo "==> Step 2/6: add alloy-genesis dep to crates/cli/commands/Cargo.toml"
CARGO_TOML="$RETH_FORK/crates/cli/commands/Cargo.toml"
if grep -q "^alloy-genesis" "$CARGO_TOML"; then
    echo "  already present"
else
    if grep -q "^alloy-consensus.workspace = true" "$CARGO_TOML"; then
        sed -i.bak '/^alloy-consensus.workspace = true/a\
alloy-genesis.workspace = true
' "$CARGO_TOML"
        echo "  inserted alloy-genesis.workspace = true after alloy-consensus.workspace = true"
    else
        echo "FATAL: could not find alloy-consensus.workspace anchor in $CARGO_TOML"
        exit 3
    fi
fi

echo "==> Step 3/6: add 'pub mod state_edit;' to crates/cli/commands/src/lib.rs"
LIB_RS="$RETH_FORK/crates/cli/commands/src/lib.rs"
if grep -q "^pub mod state_edit;" "$LIB_RS"; then
    echo "  already present"
else
    if grep -q "^pub mod stage;" "$LIB_RS"; then
        sed -i.bak '/^pub mod stage;/i\
pub mod state_edit;
' "$LIB_RS"
        echo "  inserted before 'pub mod stage;'"
    else
        echo "FATAL: could not find 'pub mod stage;' in $LIB_RS"
        exit 3
    fi
fi

echo "==> Step 4/6: patch crates/ethereum/cli/src/interface.rs (imports + variant + chain_spec arm)"
IFACE_RS="$RETH_FORK/crates/ethereum/cli/src/interface.rs"
if grep -q "StateEdit(state_edit::StateEditCommand" "$IFACE_RS"; then
    echo "  already patched"
else
    python3 - "$IFACE_RS" <<'PY'
import re, sys, pathlib

p = pathlib.Path(sys.argv[1])
src = p.read_text()

# 3a) Add `state_edit,` to the reth_cli_commands::{...} import block.
import_pat = re.compile(
    r'(config_cmd, db, download, dump_genesis, export_era, import, import_era, init_cmd, init_state,)',
    re.MULTILINE,
)
if not import_pat.search(src):
    print("FATAL: could not find reth_cli_commands import block to patch", file=sys.stderr)
    sys.exit(3)
src = import_pat.sub(
    lambda m: m.group(1) + " state_edit,",
    src,
    count=1,
)

# 3b) Add StateEdit variant right after InitState variant.
variant_pat = re.compile(
    r'(    /// Initialize the database from a state dump file\.\s*\n    #\[command\(name = "init-state"\)\]\s*\n    InitState\(init_state::InitStateCommand<C>\),)',
    re.MULTILINE,
)
if not variant_pat.search(src):
    print("FATAL: InitState variant pattern not found in interface.rs", file=sys.stderr)
    sys.exit(3)
state_edit_variant = (
    "    /// Datachain Rope `rope_untieTx` execution layer -- atomic, audited\n"
    "    /// two-account native FAT balance delta. EXTREMELY DANGEROUS; see\n"
    "    /// `crates/cli/commands/src/state_edit/mod.rs`.\n"
    "    #[command(name = \"state-edit\")]\n"
    "    StateEdit(state_edit::StateEditCommand<C>),"
)
src = variant_pat.sub(
    lambda m: m.group(1) + "\n" + state_edit_variant,
    src,
    count=1,
)

# 3c) Add Self::StateEdit chain_spec arm right after Self::InitState arm.
chainspec_pat = re.compile(
    r'(            Self::InitState\(cmd\) => cmd\.chain_spec\(\),)',
    re.MULTILINE,
)
if not chainspec_pat.search(src):
    print("FATAL: Self::InitState chain_spec arm not found in interface.rs", file=sys.stderr)
    sys.exit(3)
src = chainspec_pat.sub(
    lambda m: m.group(1) + "\n            Self::StateEdit(cmd) => cmd.chain_spec(),",
    src,
    count=1,
)

p.write_text(src)
print("  interface.rs patched (imports + variant + chain_spec arm)")
PY
fi

echo "==> Step 5/6: patch crates/ethereum/cli/src/app.rs (dispatch arm)"
APP_RS="$RETH_FORK/crates/ethereum/cli/src/app.rs"
if grep -q "Commands::StateEdit(command)" "$APP_RS"; then
    echo "  already patched"
else
    python3 - "$APP_RS" <<'PY'
import re, sys, pathlib

p = pathlib.Path(sys.argv[1])
src = p.read_text()

# Add Commands::StateEdit dispatch right after Commands::InitState dispatch.
disp_pat = re.compile(
    r'(        Commands::InitState\(command\) => runner\.run_blocking_until_ctrl_c\(command\.execute::<N>\(\)\),)',
    re.MULTILINE,
)
if not disp_pat.search(src):
    print("FATAL: Commands::InitState dispatch arm not found in app.rs", file=sys.stderr)
    sys.exit(3)
src = disp_pat.sub(
    lambda m: m.group(1) + "\n        Commands::StateEdit(command) => runner.run_blocking_until_ctrl_c(command.execute::<N>()),",
    src,
    count=1,
)

p.write_text(src)
print("  app.rs patched (dispatch arm)")
PY
fi

echo "==> Step 6/6: build patched reth (30-45 min on first build)"
cd "$RETH_FORK"
export CARGO_INCREMENTAL=1
if ! cargo build --release -p reth 2>&1 | tail -60; then
    echo "FATAL: cargo build failed"
    exit 4
fi

if [[ ! -x target/release/reth ]]; then
    echo "FATAL: target/release/reth missing after build"
    exit 4
fi

mkdir -p "$(dirname "$OUTPUT_BIN")"
cp target/release/reth "$OUTPUT_BIN"
echo "==> Patched reth binary at $OUTPUT_BIN"
echo
echo "==> state-edit subcommand help:"
"$OUTPUT_BIN" state-edit --help 2>&1 | head -40
echo
echo "==> DONE."
