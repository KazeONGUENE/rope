#!/usr/bin/env python3
"""
Build unsigned raw EIP-1559 transactions for the 2026-06-22 recovery operations.

The rescue wallet 0xCF884C81…082Eb is the signer (air-gapped, never via this
script). This script does NOT sign anything; it only produces a hex blob that
the operator will sign on the air-gapped laptop using whichever wallet UI is
installed there (MetaMask offline, ethers.js script, foundry `cast wallet sign-tx`,
etc.). The signed hex is then pasted back and broadcast via `eth_sendRawTransaction`.

USAGE — three modes, one for each tx in Phase D / F:

  # 1) Deploy UntieRegistry (the constructor arg is the rescue wallet itself)
  python3 build_raw_tx.py deploy \
      --chain-id 271828 \
      --rpc https://erpc.datachain.network \
      --from 0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb \
      --bytecode-file ./UntieRegistry.bin \
      --constructor-args 0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb

  # 2) recordUntie at the deployed UntieRegistry
  python3 build_raw_tx.py record-untie \
      --chain-id 271828 \
      --rpc https://erpc.datachain.network \
      --from 0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb \
      --untie-registry 0xUNTIE_REGISTRY_DEPLOYED_ADDRESS \
      --tier 0 \
      --executive-authority-hash 0x0000000000000000000000000000000000000000000000000000000000000000 \
      --federation-commitment-hash 0x0000000000000000000000000000000000000000000000000000000000000000 \
      --state-scope 0 \
      --asset-contract 0x0000000000000000000000000000000000000000 \
      --debit-from 0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591 \
      --credit-to 0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb \
      --wei-amount 8790904873290392000000000000 \
      --prev-state-root 0xCURRENT_HEAD_STATE_ROOT \
      --expected-post-state-root 0x0000000000000000000000000000000000000000000000000000000000000000 \
      # ^ Option 1 (chosen 2026-07-01): pass bytes32(0) as an intentional
      #   placeholder; the actual post-root is emitted later via
      #   confirmStateDelta after rope-state-edit runs. See INCIDENT §4-quater.
      --justification-cid 0xBYTES32_OF_IPFS_CID_OR_RAW_HASH \
      --justification-summary "Foundation treasury drain recovery — see INCIDENT post-mortem 2026-06-22"

  # 3) confirmStateDelta
  python3 build_raw_tx.py confirm-state-delta \
      --chain-id 271828 \
      --rpc https://erpc.datachain.network \
      --from 0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb \
      --untie-registry 0xUNTIE_REGISTRY_DEPLOYED_ADDRESS \
      --record-index 0 \
      --actual-post-state-root 0xACTUAL_POST_STATE_ROOT_AFTER_RESTART

The script outputs:

  TYPE        = 0x02  (EIP-1559)
  CHAIN_ID    = 271828
  NONCE       = <fetched from RPC eth_getTransactionCount>
  MAX_FEE     = <fetched from RPC eth_gasPrice + buffer>
  MAX_PRIO    = 1 gwei
  GAS_LIMIT   = <estimated via eth_estimateGas + 30% safety margin>
  TO          = <as appropriate; null for deploy>
  VALUE       = 0
  DATA        = <encoded calldata>
  ACCESS_LIST = []

  UNSIGNED_RLP_HEX = 0x02f9...   <-- this is what the operator signs

  HUMAN_READABLE_PREFLIGHT:
    method         = ...
    decoded_args   = ...
    gas_estimate   = ...
    cost_at_max    = ... wei  (~... FAT)
    sender_balance = ... wei  (~... FAT)

Air-gap signing recipe (foundry cast):

  cast wallet sign-tx --interactive \
      --rpc-url https://erpc.datachain.network \
      <UNSIGNED_RLP_HEX>

Then paste the resulting 0xf86c0182... back into this terminal; the agent
broadcasts it via eth_sendRawTransaction.
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.request
from typing import Any


# ----------------------------------------------------------------------------
# RPC helpers
# ----------------------------------------------------------------------------


def rpc_call(rpc_url: str, method: str, params: list[Any]) -> Any:
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(
        rpc_url,
        data=body,
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        out = json.loads(resp.read().decode())
    if "error" in out:
        raise RuntimeError(f"RPC error: {out['error']}")
    return out["result"]


def hex_to_int(h: str) -> int:
    if h.startswith("0x") or h.startswith("0X"):
        return int(h, 16)
    return int(h)


def int_to_hex(v: int) -> str:
    return hex(v)


# ----------------------------------------------------------------------------
# ABI encoding (minimal, for the calldata we need)
# ----------------------------------------------------------------------------


def encode_uint256(v: int) -> bytes:
    if v < 0:
        raise ValueError("uint256 must be non-negative")
    return v.to_bytes(32, "big")


def encode_address(addr: str) -> bytes:
    a = addr.lower().removeprefix("0x")
    if len(a) != 40:
        raise ValueError(f"invalid address: {addr}")
    return bytes(12) + bytes.fromhex(a)


def encode_bytes32(b: str) -> bytes:
    a = b.lower().removeprefix("0x")
    if len(a) != 64:
        raise ValueError(f"invalid bytes32: {b} (got {len(a)} hex chars, want 64)")
    return bytes.fromhex(a)


def encode_uint8(v: int) -> bytes:
    if not 0 <= v < 256:
        raise ValueError("uint8 out of range")
    return encode_uint256(v)


def encode_string(s: str) -> tuple[bytes, bytes]:
    """Returns (head, tail) for a dynamic string in solidity ABI."""
    data = s.encode("utf-8")
    pad = (-len(data)) % 32
    tail = encode_uint256(len(data)) + data + bytes(pad)
    # head is a 32-byte offset placeholder, filled in by the caller using
    # the total head-size and the cumulative tail offset.
    return (b"<<offset>>", tail)


# Function selectors (keccak256("name(types)")[0:4]):
#   UntieRegistry.recordUntie(uint8,bytes32,bytes32,uint8,address,address,address,uint256,bytes32,bytes32,bytes32,string)
#     -> compute below
#   UntieRegistry.confirmStateDelta(uint256,bytes32) -> compute below

def keccak256(data: bytes) -> bytes:
    """Pure-Python keccak-256 implementation for selector computation.
    Tiny, single-purpose; we only need it for function selectors and the
    string-tagged CID hash. Implementation follows NIST FIPS 202 Keccak-256.
    """
    # Implementation borrowed from a public-domain reference; kept inline so
    # this script has zero external dependencies (the air-gap signing step
    # must work on a fresh machine with no pip install).
    RATE = 136  # 1088 bits
    ROUNDS = 24
    RC = [
        0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
        0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
        0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
        0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
        0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
        0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
    ]
    R = [
        [ 0, 36,  3, 41, 18],
        [ 1, 44, 10, 45,  2],
        [62,  6, 43, 15, 61],
        [28, 55, 25, 21, 56],
        [27, 20, 39,  8, 14],
    ]

    def rol(x: int, n: int) -> int:
        n = n % 64
        return ((x << n) | (x >> (64 - n))) & 0xFFFFFFFFFFFFFFFF

    def keccak_f(s: list[list[int]]) -> None:
        for i in range(ROUNDS):
            C = [s[x][0] ^ s[x][1] ^ s[x][2] ^ s[x][3] ^ s[x][4] for x in range(5)]
            D = [C[(x - 1) % 5] ^ rol(C[(x + 1) % 5], 1) for x in range(5)]
            for x in range(5):
                for y in range(5):
                    s[x][y] ^= D[x]
            B = [[0] * 5 for _ in range(5)]
            for x in range(5):
                for y in range(5):
                    B[y][(2 * x + 3 * y) % 5] = rol(s[x][y], R[x][y])
            for x in range(5):
                for y in range(5):
                    s[x][y] = B[x][y] ^ ((~B[(x + 1) % 5][y]) & B[(x + 2) % 5][y]) & 0xFFFFFFFFFFFFFFFF
            s[0][0] ^= RC[i]

    # absorb
    state = [[0] * 5 for _ in range(5)]
    # pad: append 0x01 then 0x80 at the last byte of the rate-aligned block
    padded = bytearray(data)
    padded.append(0x01)
    while len(padded) % RATE != 0:
        padded.append(0x00)
    padded[-1] |= 0x80
    for block_start in range(0, len(padded), RATE):
        block = padded[block_start : block_start + RATE]
        for i in range(RATE // 8):
            lane = int.from_bytes(block[i * 8 : (i + 1) * 8], "little")
            x = i % 5
            y = i // 5
            state[x][y] ^= lane
        keccak_f(state)
    # squeeze 32 bytes
    out = bytearray()
    for i in range(32 // 8):
        x = i % 5
        y = i // 5
        out += state[x][y].to_bytes(8, "little")
    return bytes(out)


def selector(sig: str) -> bytes:
    return keccak256(sig.encode("utf-8"))[:4]


# ----------------------------------------------------------------------------
# RLP encoding (minimal)
# ----------------------------------------------------------------------------


def rlp_encode_bytes(b: bytes) -> bytes:
    if len(b) == 1 and b[0] < 0x80:
        return b
    if len(b) <= 55:
        return bytes([0x80 + len(b)]) + b
    length_bytes = len(b).to_bytes((len(b).bit_length() + 7) // 8 or 1, "big")
    return bytes([0xB7 + len(length_bytes)]) + length_bytes + b


def rlp_encode_int(v: int) -> bytes:
    if v < 0:
        raise ValueError("rlp int must be non-negative")
    if v == 0:
        return rlp_encode_bytes(b"")
    return rlp_encode_bytes(v.to_bytes((v.bit_length() + 7) // 8, "big"))


def rlp_encode_list(items: list[bytes]) -> bytes:
    payload = b"".join(items)
    if len(payload) <= 55:
        return bytes([0xC0 + len(payload)]) + payload
    length_bytes = len(payload).to_bytes((len(payload).bit_length() + 7) // 8 or 1, "big")
    return bytes([0xF7 + len(length_bytes)]) + length_bytes + payload


# ----------------------------------------------------------------------------
# EIP-1559 unsigned tx encoding
# ----------------------------------------------------------------------------


def build_eip1559_unsigned(
    chain_id: int,
    nonce: int,
    max_priority_fee_per_gas: int,
    max_fee_per_gas: int,
    gas_limit: int,
    to: bytes | None,  # None for contract creation
    value: int,
    data: bytes,
) -> str:
    parts = [
        rlp_encode_int(chain_id),
        rlp_encode_int(nonce),
        rlp_encode_int(max_priority_fee_per_gas),
        rlp_encode_int(max_fee_per_gas),
        rlp_encode_int(gas_limit),
        rlp_encode_bytes(to if to is not None else b""),
        rlp_encode_int(value),
        rlp_encode_bytes(data),
        rlp_encode_list([]),  # empty accessList
    ]
    payload = rlp_encode_list(parts)
    return "0x02" + payload.hex()


# ----------------------------------------------------------------------------
# Subcommands
# ----------------------------------------------------------------------------


def cmd_deploy(args: argparse.Namespace) -> None:
    with open(args.bytecode_file, "r") as f:
        bytecode_hex = f.read().strip()
    bytecode = bytes.fromhex(bytecode_hex.removeprefix("0x"))
    # Append constructor args (single address argument: consensusOracle)
    ctor_args = encode_address(args.constructor_args)
    init_code = bytecode + ctor_args

    nonce = args.nonce if args.nonce is not None else hex_to_int(rpc_call(args.rpc, "eth_getTransactionCount", [args.from_addr, "pending"]))
    gas_price = hex_to_int(rpc_call(args.rpc, "eth_gasPrice", []))
    max_priority_fee_per_gas = 1_000_000_000  # 1 gwei
    max_fee_per_gas = max(gas_price * 2, max_priority_fee_per_gas + 1)

    gas_estimate = hex_to_int(rpc_call(args.rpc, "eth_estimateGas", [{
        "from": args.from_addr,
        "data": "0x" + init_code.hex(),
    }]))
    gas_limit = int(gas_estimate * 1.3)  # 30% safety margin

    sender_balance = hex_to_int(rpc_call(args.rpc, "eth_getBalance", [args.from_addr, "latest"]))
    max_cost = gas_limit * max_fee_per_gas

    unsigned = build_eip1559_unsigned(
        chain_id=args.chain_id,
        nonce=nonce,
        max_priority_fee_per_gas=max_priority_fee_per_gas,
        max_fee_per_gas=max_fee_per_gas,
        gas_limit=gas_limit,
        to=None,
        value=0,
        data=init_code,
    )

    print_preflight(
        kind="DEPLOY",
        chain_id=args.chain_id,
        nonce=nonce,
        max_fee=max_fee_per_gas,
        max_prio=max_priority_fee_per_gas,
        gas_limit=gas_limit,
        to="null (contract creation)",
        value=0,
        data_summary=f"UntieRegistry deploy bytecode ({len(bytecode)} bytes) + constructor(address={args.constructor_args})",
        sender=args.from_addr,
        sender_balance=sender_balance,
        max_cost=max_cost,
        unsigned_rlp_hex=unsigned,
    )


def cmd_record_untie(args: argparse.Namespace) -> None:
    sel = selector(
        "recordUntie(uint8,bytes32,bytes32,uint8,address,address,address,uint256,bytes32,bytes32,bytes32,string)"
    )

    # Build calldata. The string parameter is dynamic, so we put a 32-byte
    # offset in the head and the encoded length+bytes in the tail.
    head_size = 32 * 12  # 12 fixed-size head slots
    summary_offset = head_size  # tail starts right after head

    head = (
        encode_uint8(args.tier)
        + encode_bytes32(args.executive_authority_hash)
        + encode_bytes32(args.federation_commitment_hash)
        + encode_uint8(args.state_scope)
        + encode_address(args.asset_contract)
        + encode_address(args.debit_from)
        + encode_address(args.credit_to)
        + encode_uint256(args.wei_amount)
        + encode_bytes32(args.prev_state_root)
        + encode_bytes32(args.expected_post_state_root)
        + encode_bytes32(args.justification_cid)
        + encode_uint256(summary_offset)
    )

    summary_bytes = args.justification_summary.encode("utf-8")
    pad = (-len(summary_bytes)) % 32
    tail = encode_uint256(len(summary_bytes)) + summary_bytes + bytes(pad)

    calldata = sel + head + tail

    nonce = args.nonce if args.nonce is not None else hex_to_int(rpc_call(args.rpc, "eth_getTransactionCount", [args.from_addr, "pending"]))
    gas_price = hex_to_int(rpc_call(args.rpc, "eth_gasPrice", []))
    max_priority_fee_per_gas = 1_000_000_000
    max_fee_per_gas = max(gas_price * 2, max_priority_fee_per_gas + 1)

    if args.gas_limit is not None:
        gas_limit = args.gas_limit
    else:
        # eth_estimateGas requires the target contract to already exist.
        # When batching (deploy then recordUntie in one air-gap session)
        # the target does not yet exist — supply --gas-limit explicitly then.
        gas_estimate = hex_to_int(rpc_call(args.rpc, "eth_estimateGas", [{
            "from": args.from_addr,
            "to": args.untie_registry,
            "data": "0x" + calldata.hex(),
        }]))
        gas_limit = int(gas_estimate * 1.3)

    sender_balance = hex_to_int(rpc_call(args.rpc, "eth_getBalance", [args.from_addr, "latest"]))
    max_cost = gas_limit * max_fee_per_gas

    to_bytes = bytes.fromhex(args.untie_registry.lower().removeprefix("0x"))
    unsigned = build_eip1559_unsigned(
        chain_id=args.chain_id,
        nonce=nonce,
        max_priority_fee_per_gas=max_priority_fee_per_gas,
        max_fee_per_gas=max_fee_per_gas,
        gas_limit=gas_limit,
        to=to_bytes,
        value=0,
        data=calldata,
    )

    print_preflight(
        kind="RECORD_UNTIE",
        chain_id=args.chain_id,
        nonce=nonce,
        max_fee=max_fee_per_gas,
        max_prio=max_priority_fee_per_gas,
        gas_limit=gas_limit,
        to=args.untie_registry,
        value=0,
        data_summary=(
            f"recordUntie(tier={args.tier}, exec_hash={args.executive_authority_hash[:18]}..., "
            f"fed_hash={args.federation_commitment_hash[:18]}..., scope={args.state_scope}, "
            f"asset={args.asset_contract}, debit_from={args.debit_from}, credit_to={args.credit_to}, "
            f"amount={args.wei_amount} wei ({args.wei_amount/1e18:.6f} FAT), "
            f"prev_root={args.prev_state_root[:18]}..., post_root={args.expected_post_state_root[:18]}..., "
            f"cid={args.justification_cid[:18]}..., summary=\"{args.justification_summary[:50]}...\")"
        ),
        sender=args.from_addr,
        sender_balance=sender_balance,
        max_cost=max_cost,
        unsigned_rlp_hex=unsigned,
    )


def cmd_confirm_state_delta(args: argparse.Namespace) -> None:
    sel = selector("confirmStateDelta(uint256,bytes32)")
    calldata = (
        sel
        + encode_uint256(args.record_index)
        + encode_bytes32(args.actual_post_state_root)
    )

    nonce = args.nonce if args.nonce is not None else hex_to_int(rpc_call(args.rpc, "eth_getTransactionCount", [args.from_addr, "pending"]))
    gas_price = hex_to_int(rpc_call(args.rpc, "eth_gasPrice", []))
    max_priority_fee_per_gas = 1_000_000_000
    max_fee_per_gas = max(gas_price * 2, max_priority_fee_per_gas + 1)

    if args.gas_limit is not None:
        gas_limit = args.gas_limit
    else:
        gas_estimate = hex_to_int(rpc_call(args.rpc, "eth_estimateGas", [{
            "from": args.from_addr,
            "to": args.untie_registry,
            "data": "0x" + calldata.hex(),
        }]))
        gas_limit = int(gas_estimate * 1.3)

    sender_balance = hex_to_int(rpc_call(args.rpc, "eth_getBalance", [args.from_addr, "latest"]))
    max_cost = gas_limit * max_fee_per_gas

    to_bytes = bytes.fromhex(args.untie_registry.lower().removeprefix("0x"))
    unsigned = build_eip1559_unsigned(
        chain_id=args.chain_id,
        nonce=nonce,
        max_priority_fee_per_gas=max_priority_fee_per_gas,
        max_fee_per_gas=max_fee_per_gas,
        gas_limit=gas_limit,
        to=to_bytes,
        value=0,
        data=calldata,
    )

    print_preflight(
        kind="CONFIRM_STATE_DELTA",
        chain_id=args.chain_id,
        nonce=nonce,
        max_fee=max_fee_per_gas,
        max_prio=max_priority_fee_per_gas,
        gas_limit=gas_limit,
        to=args.untie_registry,
        value=0,
        data_summary=f"confirmStateDelta(recordIndex={args.record_index}, actualPostStateRoot={args.actual_post_state_root})",
        sender=args.from_addr,
        sender_balance=sender_balance,
        max_cost=max_cost,
        unsigned_rlp_hex=unsigned,
    )


# ----------------------------------------------------------------------------
# Pretty-print
# ----------------------------------------------------------------------------


def print_preflight(
    *,
    kind: str,
    chain_id: int,
    nonce: int,
    max_fee: int,
    max_prio: int,
    gas_limit: int,
    to: str,
    value: int,
    data_summary: str,
    sender: str,
    sender_balance: int,
    max_cost: int,
    unsigned_rlp_hex: str,
) -> None:
    print(f"\n========== UNSIGNED EIP-1559 TX ({kind}) ==========")
    print(f"  TYPE          = 0x02 (EIP-1559)")
    print(f"  CHAIN_ID      = {chain_id}")
    print(f"  NONCE         = {nonce}")
    print(f"  MAX_FEE       = {max_fee} wei  ({max_fee/1e9:.2f} gwei)")
    print(f"  MAX_PRIO      = {max_prio} wei  ({max_prio/1e9:.2f} gwei)")
    print(f"  GAS_LIMIT     = {gas_limit}")
    print(f"  TO            = {to}")
    print(f"  VALUE         = {value}")
    print(f"  DATA          = {data_summary}")
    print(f"")
    print(f"  SENDER        = {sender}")
    print(f"  SENDER_BAL    = {sender_balance} wei  ({sender_balance/1e18:.6f} FAT)")
    print(f"  MAX_TX_COST   = {max_cost} wei  ({max_cost/1e18:.6f} FAT)")
    if sender_balance < max_cost:
        print(f"  WARNING       : sender balance < max tx cost; gas may underprice on a busy block")
    else:
        coverage = sender_balance / max_cost if max_cost > 0 else float("inf")
        print(f"  COVERAGE      : {coverage:.1f}× the max tx cost")
    print()
    print(f"  UNSIGNED_RLP_HEX (paste this into your air-gap signer):")
    print(f"    {unsigned_rlp_hex}")
    print()
    print("========== AIR-GAP SIGNING ==========")
    print("On your air-gapped laptop, run one of:")
    print()
    print(f"  cast wallet sign-tx \\")
    print(f"      --interactive \\")
    print(f"      --rpc-url {repr('inline')} \\")
    print(f"      <UNSIGNED_RLP_HEX_FROM_ABOVE>")
    print()
    print("Or (foundry alternative if interactive doesn't work):")
    print()
    print(f"  cast wallet sign-tx --private-key <hex_key> <UNSIGNED_RLP_HEX>")
    print()
    print("Paste the resulting 0xf86c... back here; the agent will broadcast it via")
    print("eth_sendRawTransaction over https://erpc.datachain.network.")
    print()


# ----------------------------------------------------------------------------
# Argparse
# ----------------------------------------------------------------------------


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    def add_common(sp: argparse.ArgumentParser) -> None:
        sp.add_argument("--chain-id", type=int, default=271_828)
        sp.add_argument("--rpc", default="https://erpc.datachain.network")
        sp.add_argument("--from", dest="from_addr", required=True)
        sp.add_argument(
            "--nonce",
            type=int,
            default=None,
            help="Override nonce (default: fetch pending nonce from RPC). "
            "Use when batching multiple txs from the same sender that have "
            "not yet been broadcast/confirmed.",
        )
        sp.add_argument(
            "--gas-limit",
            type=int,
            default=None,
            help="Override gas limit (default: eth_estimateGas + 30%% buffer). "
            "Use when the target contract does not exist yet (batched txs).",
        )

    sp_deploy = sub.add_parser("deploy", help="Deploy UntieRegistry")
    add_common(sp_deploy)
    sp_deploy.add_argument("--bytecode-file", required=True, help="Path to UntieRegistry init bytecode (hex)")
    sp_deploy.add_argument("--constructor-args", required=True, help="Address arg for the constructor (consensusOracle)")
    sp_deploy.set_defaults(func=cmd_deploy)

    sp_record = sub.add_parser("record-untie", help="Call UntieRegistry.recordUntie")
    add_common(sp_record)
    sp_record.add_argument("--untie-registry", required=True)
    sp_record.add_argument("--tier", type=int, required=True, choices=[0, 1, 2])
    sp_record.add_argument("--executive-authority-hash", required=True)
    sp_record.add_argument("--federation-commitment-hash", required=True)
    sp_record.add_argument("--state-scope", type=int, required=True)
    sp_record.add_argument("--asset-contract", required=True)
    sp_record.add_argument("--debit-from", required=True)
    sp_record.add_argument("--credit-to", required=True)
    sp_record.add_argument("--wei-amount", type=int, required=True)
    sp_record.add_argument("--prev-state-root", required=True)
    sp_record.add_argument("--expected-post-state-root", required=True)
    sp_record.add_argument("--justification-cid", required=True)
    sp_record.add_argument("--justification-summary", required=True)
    sp_record.set_defaults(func=cmd_record_untie)

    sp_confirm = sub.add_parser("confirm-state-delta", help="Call UntieRegistry.confirmStateDelta")
    add_common(sp_confirm)
    sp_confirm.add_argument("--untie-registry", required=True)
    sp_confirm.add_argument("--record-index", type=int, required=True)
    sp_confirm.add_argument("--actual-post-state-root", required=True)
    sp_confirm.set_defaults(func=cmd_confirm_state_delta)

    args = p.parse_args()
    try:
        args.func(args)
    except (RuntimeError, ValueError) as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
