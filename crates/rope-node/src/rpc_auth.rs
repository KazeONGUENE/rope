//! Public-RPC method authorization gate.
//!
//! See `docs/SECURITY_AUDIT_2026-06-11.md` (V11 Critical) for the full
//! threat model. Short version:
//!
//! Several `rope_*` JSON-RPC methods mutate user state (untie a knot,
//! erase a personal ledger, append a knot, create a personal ledger,
//! anchor a deployer attestation). Their existing PHASE-1 auth model
//! assumes the public proxy authenticates the caller. The current
//! production nginx config does not. So before this gate landed, anyone
//! on the open internet could call them via `https://erpc.datachain.network`.
//!
//! This module is the in-process belt-and-suspenders fix: any of those
//! "destructive" methods is denied at the JSON-RPC dispatch boundary
//! when the env flag `ROPE_PUBLIC_DESTRUCTIVE_DENY` is set to `1`
//! (default ON). Operators who need to call these methods do so on a
//! private listener (loopback) where the env flag is unset, or via an
//! authenticated proxy that upgrades the request to PHASE 2 (signed
//! payload — to be wired in `crates/rope-node` once `handle_json_rpc`
//! threads HTTP headers through; tracked under the v2.0 roadmap).
//!
//! The list of destructive methods is canon-defined here so it is
//! reviewable in one place. Adding a new mutator means adding it here.
//!
//! # Behaviour
//!
//! - `ROPE_PUBLIC_DESTRUCTIVE_DENY=1` (default): every call to a method
//!   in [`DESTRUCTIVE_METHODS`] returns the JSON-RPC error
//!   `{ code: -32401, message: "Method denied on public listener; see SECURITY_AUDIT_2026-06-11.md" }`
//!   *before* the method body runs. No state side-effect is possible.
//! - `ROPE_PUBLIC_DESTRUCTIVE_DENY=0`: the gate is a no-op. The methods
//!   run as before. Use only on private listeners or in dev.
//!
//! # Tests
//!
//! Unit tests in this file cover:
//! - the env default is treated as ON when unset,
//! - explicit `1`/`true`/`yes` values turn it ON,
//! - explicit `0`/`false`/`no` values turn it OFF,
//! - the membership check for each destructive method.
//!
//! Integration tests wire `handle_json_rpc` and live in `rpc_server.rs`.

use std::env;

/// Mechanically extracted, on every build, from `rpc_server.rs`'s own
/// dispatch match by `build.rs`. See that file's module docs for how the
/// extraction works and why it is trustworthy as an input to
/// [`verify_dispatcher_completeness`] below.
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/dispatcher_methods_generated.rs"));
}

/// JSON-RPC error code returned by the gate. -32401 is in the
/// implementation-defined server-error band (-32000..-32099 is the
/// JSON-RPC 2.0 reserved range; -32401 is in the application-defined
/// extension range we use elsewhere in this codebase, mirroring HTTP
/// 401 Unauthorized for ergonomics).
pub const DENIED_ERROR_CODE: i32 = -32401;

/// Human-readable reason returned to the caller.
pub const DENIED_ERROR_MESSAGE: &str =
    "Method denied on public listener; see SECURITY_AUDIT_2026-06-11.md";

/// The canonical list of methods that mutate user-visible state and
/// therefore must not be callable from the open internet without an
/// authenticated upgrade (PHASE 2). This set is locked down here so
/// that future contributors who add a new mutator can audit the list
/// in one place.
///
/// **Why each entry is here:**
///
/// - `rope_untieKnot`            — destroys an OES key shred, irreversibly
///                                  erasing the payload of one knot on
///                                  the target wallet's string. GDPR-Art.17
///                                  primitive. Forging a call would let
///                                  any internet user erase any user's
///                                  audit-trail entry.
/// - `rope_erasePersonalLedger`  — whole-wallet equivalent of `untieKnot`.
///                                  Erases every entry on the wallet's
///                                  string. Strictly more destructive.
/// - `rope_appendToLedger`       — writes a new knot onto a wallet's
///                                  string. A forger could spam any
///                                  user's ledger with arbitrary
///                                  attestations, polluting the audit
///                                  trail and triggering downstream
///                                  insurance / compliance / reputation
///                                  reactions.
/// - `rope_createPersonalLedger` — creates an empty string for a wallet.
///                                  Lower-stakes (no destruction), but
///                                  a spammer could create millions of
///                                  ledgers and exhaust the registry's
///                                  index memory.
/// - `rope_anchorDeployerAttestation`
///                               — re-anchors the node's local deployer
///                                  attestation onto the deployer's
///                                  ledger. The signature is the node's
///                                  own (cannot be forged by the caller),
///                                  but the call is a free knot-write
///                                  triggered by an unauthenticated
///                                  caller. Spam vector.
/// - `rope_submitTestimony`      — folds a peer testimony into the
///                                  finality tally. The testimony is
///                                  self-authenticating (hybrid
///                                  Ed25519+Dilithium3 signature verified
///                                  against the committee registry), so
///                                  forgery is cryptographically blocked,
///                                  but an open endpoint would still be a
///                                  free verification-work DoS vector.
///                                  Committee peers exchange testimonies
///                                  over libp2p gossip, not public RPC;
///                                  RPC submission is an operator path.
/// - `rope_registerValidator`    — adds a validator public key to the
///                                  committee registry at runtime.
///                                  Membership change is privileged:
///                                  only the operator (loopback/token)
///                                  may extend the roster. Persistent
///                                  membership comes from the
///                                  operator-distributed
///                                  `validator_set.json` roster.
/// - `rope_v2_appendKnot`        — Canon v2.0 Phase 4 DAG write. Same
///                                  threat profile as `rope_appendToLedger`
///                                  (arbitrary knot writes onto any
///                                  wallet's DAG), so same gating.
/// - `rope_v2_compact`           — triggers DAG tip compaction (a merge
///                                  knot write) on the target wallet.
///                                  State-mutating, spam vector; operator
///                                  or background-task path only.
/// - `rope_registerDevice`       — added 2026-07-25 (finding C7 of
///                                  `SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`).
///                                  Registers an arbitrary IoT device
///                                  bound to an arbitrary wallet with zero
///                                  authentication. Before this fix, any
///                                  internet caller could register phantom
///                                  devices against any wallet.
/// - `rope_ingestTelemetry`      — added 2026-07-25 (same finding). Writes
///                                  telemetry readings against a device
///                                  wallet with zero authentication.
///                                  Unbounded write amplification vector
///                                  (also see H3 body-size fix).
/// - `rope_subscribeAgentToWallet`
///                               — added 2026-07-25 (same finding). Grants
///                                  an AI agent a standing subscription to
///                                  a wallet with zero authentication —
///                                  any caller could subscribe any agent
///                                  to any wallet's events.
///
/// **Deliberately NOT in this list:** `rope_suspendNode`, `rope_isolateNode`,
/// `rope_eraseNode`. These already carry an independent per-call Ed25519
/// governance-signature check (`Governance::verify_action_signature`,
/// verified against the founder/master-node roster in
/// `master-nodes.toml`) that authenticates the *caller's* authority before
/// any state mutation runs — a strictly stronger guarantee than the
/// blanket Phase-1 gate. Blanket-denying them here would additionally
/// require plumbing that Ed25519 signature format into the Phase-2
/// `AuthVerifier` (which currently only recognises the wallet-EIP-191 and
/// deployer-attestation-Ed25519 shapes) before remote founder/master-node
/// governance calls over the public RPC could still succeed — doing so
/// without that plumbing would silently break legitimate remote incident
/// response. Tracked as follow-up work; see
/// `SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md` finding C7 notes.
pub const DESTRUCTIVE_METHODS: &[&str] = &[
    "rope_untieKnot",
    "rope_erasePersonalLedger",
    "rope_appendToLedger",
    "rope_createPersonalLedger",
    "rope_anchorDeployerAttestation",
    "rope_submitTestimony",
    "rope_registerValidator",
    "rope_v2_appendKnot",
    "rope_v2_compact",
    "rope_registerDevice",
    "rope_ingestTelemetry",
    "rope_subscribeAgentToWallet",
];

/// Methods that mutate state but carry their **own independent**
/// cryptographic authentication, verified inside the handler before any
/// side effect runs, so gating them again here would be redundant (and,
/// for `rope_suspendNode`/`rope_isolateNode`/`rope_eraseNode`, would break
/// legitimate remote incident response — see the doc comment above
/// [`DESTRUCTIVE_METHODS`]).
///
/// - `rope_suspendNode` / `rope_isolateNode` / `rope_eraseNode` — Ed25519
///   governance signature checked against the founder/master-node roster
///   (`Governance::verify_action_signature`) before `record_action` runs.
/// - `eth_sendRawTransaction` — the payload is an RLP-encoded, ECDSA-signed
///   Ethereum transaction. rope-node does not execute it itself; it is
///   forwarded to the EVM backend (Reth), which independently recovers and
///   validates the signer from the transaction's own signature before
///   accepting it. An attacker who cannot produce a valid signature for
///   the claimed sender cannot get a transaction accepted no matter how
///   they reach this RPC method.
///
/// This bucket exists so the boot-time dispatcher-completeness check
/// (see [`verify_dispatcher_completeness`]) can distinguish "mutates state,
/// unguarded — must be in DESTRUCTIVE_METHODS" from "mutates state, but
/// self-authenticating by construction — deliberately excluded" instead of
/// only having two buckets (destructive vs. safe) and forcing every
/// self-authenticated method to be miscategorised as one or the other.
pub const SELF_AUTHENTICATED_METHODS: &[&str] = &[
    "rope_suspendNode",
    "rope_isolateNode",
    "rope_eraseNode",
    "eth_sendRawTransaction",
];

/// Foundry-Anvil-compatibility and EVM-devnet debug/admin methods
/// (`anvil_*`, `evm_*`). `rpc_server.rs` forwards these **blindly** to
/// whatever EVM backend is configured — it does not implement any of them
/// itself. In production that backend is Reth, which does not register an
/// `anvil` or `evm` JSON-RPC namespace at all, so today these calls
/// uniformly fail with "method not found" upstream.
///
/// That is safety-by-absence, not safety-by-design: nothing in rope-node
/// stops a future backend swap, a Reth version that *does* add compatible
/// namespaces, or a misconfigured `EVM_RPC_URL` pointed at a real Anvil
/// instance from turning this pass-through into a direct
/// "set any address's balance/code/storage, impersonate any account, mine
/// arbitrary blocks" primitive — the worst-case blast radius of any method
/// in this file. Found during the dispatcher-completeness audit
/// (`SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md` §5.1 follow-up); not a
/// previously-numbered finding, but treated with the same fail-secure
/// posture as the rest of this module.
///
/// [`dev_only_evm_methods_enabled`] gates them: default **OFF** in
/// production. `rpc_server.rs` checks this flag *before* forwarding any
/// method in this list and returns "method not found" locally, without
/// ever reaching the EVM backend, when the flag is off. Set
/// `ROPE_ALLOW_EVM_DEV_METHODS=1` only on a local devnet.
pub const DEV_ONLY_EVM_METHODS: &[&str] = &[
    "anvil_impersonateAccount",
    "anvil_stopImpersonatingAccount",
    "anvil_setBalance",
    "anvil_setCode",
    "anvil_setNonce",
    "anvil_dumpState",
    "anvil_loadState",
    "anvil_mine",
    "anvil_setStorageAt",
    "anvil_reset",
    "evm_snapshot",
    "evm_revert",
    "evm_increaseTime",
    "evm_mine",
];

/// Every remaining dispatcher method: pure reads (chain state, registry
/// lookups, agent/device/testimony status, EVM-compat `eth_get*`/`eth_call`
/// reads) or introspection (`rpc_methods`, `rpc_modules`). None of these
/// mutate rope-node's own state; the ones that touch the EVM backend do so
/// via read-only JSON-RPC calls.
///
/// This list must be kept in sync with the dispatcher by hand — the
/// boot-time completeness check (which compares it, unioned with the three
/// buckets above, against [`generated::ALL_REGISTERED_METHODS`]) is what
/// enforces that "kept in sync" claim on every build+boot rather than
/// trusting the claim itself.
pub const SAFE_READ_ONLY_METHODS: &[&str] = &[
    "eth_accounts",
    "eth_blockNumber",
    "eth_call",
    "eth_chainId",
    "eth_estimateGas",
    "eth_feeHistory",
    "eth_gasPrice",
    "eth_getBalance",
    "eth_getBlockByHash",
    "eth_getBlockByNumber",
    "eth_getBlockTransactionCountByHash",
    "eth_getBlockTransactionCountByNumber",
    "eth_getCode",
    "eth_getLogs",
    "eth_getStorageAt",
    "eth_getTransactionByBlockNumberAndIndex",
    "eth_getTransactionByHash",
    "eth_getTransactionCount",
    "eth_getTransactionReceipt",
    "eth_getUncleCountByBlockHash",
    "eth_getUncleCountByBlockNumber",
    "eth_hashrate",
    "eth_maxPriorityFeePerGas",
    "eth_mining",
    "eth_protocolVersion",
    "eth_syncing",
    "net_listening",
    "net_peerCount",
    "net_version",
    "rope_committeeInfo",
    "rope_getAIAgentStatus",
    "rope_getAgentStatus",
    "rope_getDeviceStatus",
    "rope_getIoTGatewayStats",
    "rope_getKnotByHash",
    "rope_getKnotByIndex",
    "rope_getLedgerStatus",
    "rope_getNetworkInfo",
    "rope_getRecentDiagnoses",
    "rope_getString",
    "rope_getStringById",
    "rope_getStringWithKnots",
    "rope_getTestimonyStatus",
    "rope_globalStats",
    "rope_governanceInfo",
    "rope_knotIndex",
    "rope_listAgents",
    "rope_listApplications",
    "rope_listDeployerAttestations",
    "rope_listDevices",
    "rope_listEcosystems",
    "rope_listKnots",
    "rope_listMasterNodes",
    "rope_listRelations",
    "rope_listStrings",
    "rope_listStringsWithKnots",
    "rope_nodeIdentity",
    // Despite the name, this is a listing endpoint: it returns the
    // currently-registered agents and does not itself register anything.
    // See `rpc_server.rs`'s `rope_registerAgent` arm.
    "rope_registerAgent",
    // Read-only ledger reconstruction; internal-vs-public callers only
    // differ in whether the OES-decrypted payload is included, never in
    // whether a write happens (there is none). See `rpc_server.rs`'s
    // `rope_repatriatePersonalLedger` arm.
    "rope_repatriatePersonalLedger",
    "rope_resolveLabel",
    "rope_v2_stats",
    "rope_v2_tips",
    "rope_v2_walkString",
    "rope_validatorIdentity",
    "rpc_methods",
    "rpc_modules",
    "web3_clientVersion",
];

/// CERBER WATCH — `blocked_signers` wiring (2026-07-25 audit follow-up,
/// finding H1/C4). These are the dispatch methods whose first `params`
/// element is a plain wallet/owner address string identifying who the
/// call acts *as* (verified by reading each arm in `rpc_server.rs`).
/// `rpc_server.rs::handle_json_rpc_with_auth` checks `params[0]` against
/// `rope_security::guard::RequestGuard`'s signer blocklist for every
/// method in this list, on every caller (internal callers included —
/// a compromised key is compromised regardless of which listener the
/// call arrives on).
pub const WALLET_PARAM0_METHODS: &[&str] = &[
    "rope_createPersonalLedger",
    "rope_appendToLedger",
    "rope_untieKnot",
    "rope_erasePersonalLedger",
];

/// Same idea as [`WALLET_PARAM0_METHODS`], but the wallet address is the
/// *second* positional parameter. `rope_subscribeAgentToWallet(agent_id,
/// wallet)` is the only method in this shape today.
pub const WALLET_PARAM1_METHODS: &[&str] = &["rope_subscribeAgentToWallet"];

/// Returns true when Foundry-Anvil-compatibility devnet methods
/// (`anvil_*`, `evm_*` — see [`DEV_ONLY_EVM_METHODS`]) are allowed to be
/// forwarded to the configured EVM backend. Default **OFF** (fail-secure):
/// unset or any value other than `1`/`true`/`yes`/`on` keeps them
/// disabled. Intended for local devnets only; never set this in a
/// production `.env`/systemd unit.
pub fn dev_only_evm_methods_enabled() -> bool {
    match env::var("ROPE_ALLOW_EVM_DEV_METHODS") {
        Ok(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

/// Pull the wallet/signer address `rpc_server.rs` would use to key a
/// mutation for `method`, if any, out of the request's `params` value.
/// Returns `None` for methods outside [`WALLET_PARAM0_METHODS`] /
/// [`WALLET_PARAM1_METHODS`], or when the expected positional parameter
/// isn't present or isn't a string (the dispatch handler itself will
/// reject those with its own `-32602 Missing ... parameter` error; this
/// function only needs to recognise a well-formed call in order to gate
/// it, not to duplicate the handler's own validation).
pub fn wallet_param_for_method<'a>(
    method: &str,
    params: Option<&'a serde_json::Value>,
) -> Option<&'a str> {
    let index = if WALLET_PARAM0_METHODS.contains(&method) {
        0
    } else if WALLET_PARAM1_METHODS.contains(&method) {
        1
    } else {
        return None;
    };
    params.and_then(|p| p.get(index)).and_then(|v| v.as_str())
}

/// Run the CERBER boot-time dispatcher-completeness check (new capability
/// recommended by `SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md` §5.1) and
/// log the result. Returns `Ok(())` when every method the build-time
/// scanner found in `rpc_server.rs`'s dispatch block is accounted for in
/// exactly one of [`DESTRUCTIVE_METHODS`], [`SELF_AUTHENTICATED_METHODS`],
/// [`DEV_ONLY_EVM_METHODS`], or [`SAFE_READ_ONLY_METHODS`].
///
/// Callers (see `rpc_server.rs::RpcServer::run`) are expected to treat a
/// non-clean report as fatal by default — refuse to bind the public
/// listener — with an explicit, logged escape hatch
/// (`ROPE_ALLOW_DISPATCHER_DRIFT=1`) for operators who need to bring a node
/// up while a newly-added method is still being triaged. The escape hatch
/// is intentionally not silent: every boot with it set emits an `error!`
/// log line naming the exact unclassified/duplicated methods, so drift is
/// always visible in the journal even when the process didn't abort.
pub fn verify_dispatcher_completeness() -> Result<(), rope_security::dispatcher_completeness::CompletenessReport> {
    rope_security::dispatcher_completeness::verify(
        generated::ALL_REGISTERED_METHODS,
        &[
            DESTRUCTIVE_METHODS,
            SELF_AUTHENTICATED_METHODS,
            DEV_ONLY_EVM_METHODS,
            SAFE_READ_ONLY_METHODS,
        ],
    )
}

/// Header that lets a co-located caller (the canonical agents on the same
/// VPS) bypass the public-listener deny. The value MUST match
/// `ROPE_INTERNAL_RPC_TOKEN`; nginx strips any inbound copy of this header
/// from public traffic, so an attacker cannot forge it.
pub const INTERNAL_AUTH_HEADER: &str = "X-Rope-Internal-Token";

/// Read the configured internal RPC token. None when the env var is unset
/// or empty (gate has no bypass; only loopback-only listeners can call
/// destructive methods).
pub fn internal_rpc_token() -> Option<String> {
    match env::var("ROPE_INTERNAL_RPC_TOKEN") {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Constant-time compare of the configured token against a presented one.
/// Returns false if the token is unset or doesn't match.
pub fn internal_token_matches(presented: &str) -> bool {
    let Some(expected) = internal_rpc_token() else {
        return false;
    };
    if expected.len() != presented.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (a, b) in expected.as_bytes().iter().zip(presented.as_bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Returns true when the Phase-2 signed-payload verifier is enabled.
///
/// Phase-2 (`docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md`) replaces the
/// blanket public-listener deny with a per-call signature check. While
/// the flag is OFF (the default), the Phase-1 V11 deny gate is the only
/// thing protecting destructive methods on the public listener. Once the
/// flag is ON, public callers can invoke destructive methods iff they
/// present a fresh, signed auth envelope. The two paths coexist: the
/// Phase-1 deny still fires for unsigned calls; signed calls pass through
/// the verifier instead.
///
/// Default: **OFF** until Phase-2 is fully rolled out across the fleet.
/// Set `ROPE_PHASE2_SIGNED_DESTRUCTIVE=1` (or `true`/`yes`/`on`) to
/// enable.
pub fn phase2_signed_destructive_enabled() -> bool {
    match env::var("ROPE_PHASE2_SIGNED_DESTRUCTIVE") {
        Ok(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

/// Returns true when the public-listener deny flag is on. Defaults to
/// **ON** when the env var is unset (fail-secure).
pub fn public_destructive_deny_enabled() -> bool {
    match env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY") {
        Ok(s) => match s.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "no" | "off" => false,
            _ => true,
        },
        Err(_) => true,
    }
}

/// Returns true if a call to `method` should be denied right now.
///
/// The check is fail-secure: if the env var is unparseable or unset
/// the gate is ON, regardless of which method was requested.
pub fn should_deny(method: &str) -> bool {
    if !public_destructive_deny_enabled() {
        return false;
    }
    DESTRUCTIVE_METHODS.contains(&method)
}

/// Build the canonical JSON-RPC error response for a denied method.
///
/// Mirrors the shape of every other error path in `rpc_server.rs` so
/// callers cannot distinguish a denied call from an internal error
/// other than by the code (-32401) and the message text.
pub fn denied_response(id: &serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": DENIED_ERROR_CODE,
            "message": DENIED_ERROR_MESSAGE
        },
        "id": id,
    })
    .to_string()
}

/// JSON-RPC error code for the CERBER `blocked_signers` gate. Distinct
/// from [`DENIED_ERROR_CODE`] (-32401, "no auth presented at all") because
/// this is a different failure mode: the caller *is* naming a specific
/// wallet, and that wallet is explicitly denylisted regardless of what
/// auth the caller presents. -32402 is the next slot in the same
/// implementation-defined extension band.
pub const BLOCKED_SIGNER_ERROR_CODE: i32 = -32402;

/// Build the JSON-RPC error response for a request whose wallet parameter
/// matched the CERBER signer blocklist (`rope_security::guard::RequestGuard`).
pub fn blocked_signer_response(id: &serde_json::Value, signer: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": BLOCKED_SIGNER_ERROR_CODE,
            "message": format!(
                "Signer {signer} is denylisted by CERBER WATCH; see \
                 SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md finding H1/C4."
            )
        },
        "id": id,
    })
    .to_string()
}

/// Resolve the IP address that should be used as the rate-limiting /
/// audit-logging key for an inbound HTTP(S) request.
///
/// Added 2026-07-25 (finding H4 of
/// `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`). Context: the
/// pre-existing per-connection `RateLimiter` in `rpc_server.rs` keys on
/// the raw TCP peer address. In production every public request to
/// `erpc.datachain.network` / `dcscan.io` arrives via nginx, which
/// terminates the internet-facing TLS connection and opens its OWN
/// connection to rope-node from the same host — so the TCP peer address
/// rope-node actually sees for essentially all internet traffic is
/// nginx's own (loopback / docker-bridge) address, not the real
/// end-user's. Keying the limiter on that collapses every distinct
/// internet client into one shared bucket: a single abusive client can
/// exhaust the budget for every legitimate user behind the same proxy,
/// and conversely the limiter provides no per-attacker isolation at all.
///
/// This mirrors the trust model already used for the V11
/// destructive-method gate (see `is_internal` in `handle_connection`,
/// `rpc_server.rs`): nginx ALWAYS sets `X-Forwarded-For` (and/or
/// `X-Real-IP`) on public traffic, and nginx's connection to rope-node
/// always arrives from a loopback-classified peer. So:
///
/// - If the TCP peer is loopback AND an `X-Forwarded-For` or
///   `X-Real-IP` header is present, trust the FIRST hop of that header
///   as the real client IP. This is safe specifically because an
///   internet-side attacker can never BE the loopback peer — they
///   cannot connect directly to this node's public listener and also
///   present as `127.0.0.1`/`::1`, so they cannot forge this branch.
/// - Otherwise (no proxy in front, or a loopback caller with no XFF —
///   i.e. one of the canonical agents on the same box talking straight
///   to `127.0.0.1:8545`) fall back to the raw TCP peer IP. In
///   particular, an attacker who connects DIRECTLY (bypassing nginx,
///   e.g. because of a firewall misconfiguration) and forges an XFF
///   header gains nothing: `peer_is_loopback` is false for them, so
///   this function ignores the header and keys on their real source
///   IP.
pub fn effective_client_ip(peer_ip: &str, peer_is_loopback: bool, headers: &str) -> String {
    if !peer_is_loopback {
        return peer_ip.to_string();
    }
    if let Some(ip) = first_header_value(headers, "x-forwarded-for")
        .and_then(|v| v.split(',').next().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
    {
        return ip;
    }
    if let Some(ip) = first_header_value(headers, "x-real-ip").filter(|s| !s.is_empty()) {
        return ip;
    }
    peer_ip.to_string()
}

/// Case-insensitive `Header-Name: value` lookup over a raw HTTP header
/// block (lines separated by `\r\n` or `\n`). Returns the trimmed value
/// of the first matching header, if any.
fn first_header_value(headers: &str, name: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (key, val) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case(name) {
            Some(val.trim().to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Process-wide lock so tests that mutate the shared env var run
    /// serially even when `cargo test` parallelises across this module.
    /// Without this, a sibling test may overwrite the env var between
    /// our `set_var` and `should_deny` reads, causing a flaky assertion.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Separate lock for `ROPE_INTERNAL_RPC_TOKEN` so token-related tests
    /// don't race with the deny-flag tests above.
    static TOKEN_LOCK: Mutex<()> = Mutex::new(());

    fn with_token<F: FnOnce()>(value: Option<&str>, f: F) {
        let _guard = TOKEN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = env::var("ROPE_INTERNAL_RPC_TOKEN").ok();
        match value {
            Some(v) => env::set_var("ROPE_INTERNAL_RPC_TOKEN", v),
            None => env::remove_var("ROPE_INTERNAL_RPC_TOKEN"),
        }
        f();
        match prev {
            Some(p) => env::set_var("ROPE_INTERNAL_RPC_TOKEN", p),
            None => env::remove_var("ROPE_INTERNAL_RPC_TOKEN"),
        }
    }

    #[test]
    fn rpc_auth_internal_token_unset_never_matches() {
        with_token(None, || {
            assert!(!internal_token_matches("anything"));
            assert!(!internal_token_matches(""));
        });
    }

    #[test]
    fn rpc_auth_internal_token_empty_env_never_matches() {
        with_token(Some(""), || {
            assert!(!internal_token_matches(""));
            assert!(!internal_token_matches("anything"));
        });
    }

    #[test]
    fn rpc_auth_internal_token_exact_match() {
        with_token(Some("super-secret-deadbeef"), || {
            assert!(internal_token_matches("super-secret-deadbeef"));
            assert!(!internal_token_matches("super-secret-deadbeee"));
            assert!(!internal_token_matches("super-secret-deadbee"));
            assert!(!internal_token_matches("super-secret-deadbeefX"));
            assert!(!internal_token_matches(""));
        });
    }

    /// Helper: run a closure with a specific value for the env var,
    /// then restore the previous value (or unset it). Holds `ENV_LOCK`
    /// for the entire critical section.
    fn with_env<F: FnOnce()>(value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY").ok();
        match value {
            Some(v) => env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", v),
            None => env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
        f();
        match prev {
            Some(p) => env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", p),
            None => env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
    }

    #[test]
    fn rpc_auth_unset_env_defaults_to_on() {
        with_env(None, || {
            assert!(public_destructive_deny_enabled());
            assert!(should_deny("rope_untieKnot"));
            assert!(should_deny("rope_erasePersonalLedger"));
            assert!(!should_deny("rope_globalStats"));
            assert!(!should_deny("eth_blockNumber"));
        });
    }

    #[test]
    fn rpc_auth_explicit_one_is_on() {
        with_env(Some("1"), || {
            assert!(public_destructive_deny_enabled());
            assert!(should_deny("rope_untieKnot"));
        });
    }

    #[test]
    fn rpc_auth_explicit_true_is_on() {
        with_env(Some("true"), || {
            assert!(public_destructive_deny_enabled());
        });
    }

    #[test]
    fn rpc_auth_explicit_zero_is_off() {
        with_env(Some("0"), || {
            assert!(!public_destructive_deny_enabled());
            assert!(!should_deny("rope_untieKnot"));
            assert!(!should_deny("rope_erasePersonalLedger"));
        });
    }

    #[test]
    fn rpc_auth_false_is_off() {
        with_env(Some("false"), || {
            assert!(!public_destructive_deny_enabled());
        });
    }

    #[test]
    fn rpc_auth_no_is_off() {
        with_env(Some("no"), || {
            assert!(!public_destructive_deny_enabled());
        });
    }

    #[test]
    fn rpc_auth_off_is_off() {
        with_env(Some("off"), || {
            assert!(!public_destructive_deny_enabled());
        });
    }

    #[test]
    fn rpc_auth_garbage_value_treated_as_on() {
        // Fail-secure: an unrecognised value defaults to ON.
        with_env(Some("maybe"), || {
            assert!(public_destructive_deny_enabled());
        });
    }

    #[test]
    fn rpc_auth_destructive_list_locked() {
        // If you add a new mutator to the dispatcher in `rpc_server.rs`
        // without listing it here, this test should fail in code review.
        let expected = [
            "rope_untieKnot",
            "rope_erasePersonalLedger",
            "rope_appendToLedger",
            "rope_createPersonalLedger",
            "rope_anchorDeployerAttestation",
            "rope_submitTestimony",
            "rope_registerValidator",
            "rope_v2_appendKnot",
            "rope_v2_compact",
            "rope_registerDevice",
            "rope_ingestTelemetry",
            "rope_subscribeAgentToWallet",
        ];
        for method in expected.iter() {
            assert!(
                DESTRUCTIVE_METHODS.contains(method),
                "destructive method missing from gate list: {method}",
            );
        }
        assert_eq!(DESTRUCTIVE_METHODS.len(), expected.len());
    }

    #[test]
    fn rpc_auth_governance_methods_intentionally_excluded() {
        // rope_suspendNode / rope_isolateNode / rope_eraseNode carry their
        // own independent Ed25519 governance-signature check inside the
        // handler and are deliberately NOT blanket-gated here (see the
        // doc comment above DESTRUCTIVE_METHODS). This test pins that
        // decision so a future edit can't silently add them without
        // updating the doc comment and this test together.
        for method in ["rope_suspendNode", "rope_isolateNode", "rope_eraseNode"] {
            assert!(
                !DESTRUCTIVE_METHODS.contains(&method),
                "{method} should stay excluded from the blanket gate (has its own auth)",
            );
        }
    }

    #[test]
    fn effective_client_ip_trusts_xff_only_from_loopback() {
        // Loopback peer (nginx) + XFF present -> trust the first hop.
        assert_eq!(
            effective_client_ip("127.0.0.1", true, "X-Forwarded-For: 203.0.113.7, 10.0.0.1\r\n"),
            "203.0.113.7"
        );
        // Non-loopback peer (direct connection, no proxy in front) ->
        // NEVER trust a client-supplied XFF, even if present.
        assert_eq!(
            effective_client_ip(
                "198.51.100.9",
                false,
                "X-Forwarded-For: 1.2.3.4\r\n"
            ),
            "198.51.100.9"
        );
    }

    #[test]
    fn effective_client_ip_falls_back_to_peer_when_no_proxy_headers() {
        // Loopback caller with no XFF/X-Real-IP -> canonical agent on the
        // same box, key on the loopback address itself.
        assert_eq!(effective_client_ip("127.0.0.1", true, "Host: erpc\r\n"), "127.0.0.1");
    }

    #[test]
    fn effective_client_ip_prefers_xff_over_x_real_ip() {
        let headers = "X-Real-IP: 9.9.9.9\r\nX-Forwarded-For: 203.0.113.7\r\n";
        assert_eq!(effective_client_ip("127.0.0.1", true, headers), "203.0.113.7");
    }

    #[test]
    fn effective_client_ip_uses_x_real_ip_when_no_xff() {
        assert_eq!(
            effective_client_ip("127.0.0.1", true, "X-Real-IP: 203.0.113.42\r\n"),
            "203.0.113.42"
        );
    }

    #[test]
    fn effective_client_ip_ignores_empty_xff_value() {
        assert_eq!(
            effective_client_ip("127.0.0.1", true, "X-Forwarded-For: \r\n"),
            "127.0.0.1"
        );
    }

    #[test]
    fn effective_client_ip_case_insensitive_header_match() {
        assert_eq!(
            effective_client_ip("127.0.0.1", true, "x-forwarded-for: 203.0.113.7\r\n"),
            "203.0.113.7"
        );
    }

    #[test]
    fn dispatcher_completeness_is_clean_against_the_live_dispatcher() {
        // The load-bearing test: build.rs re-extracts the dispatcher's own
        // method literals on every build. If a future edit adds a new
        // "rope_*"/"eth_*"/etc match arm to rpc_server.rs without also
        // triaging it into one of the four buckets in this file, this
        // test fails immediately in CI/local `cargo test` — long before
        // it could ship as an unauthenticated mutator (the exact shape of
        // finding C7).
        let report = verify_dispatcher_completeness();
        assert!(
            report.is_ok(),
            "dispatcher-completeness check failed: {}",
            report.err().map(|r| r.summary()).unwrap_or_default()
        );
    }

    #[test]
    fn dispatcher_completeness_catches_a_synthetic_gap() {
        // Directly exercise the underlying rope_security check with a
        // deliberately incomplete bucket set, independent of the live
        // generated list, so this test doesn't depend on the current
        // dispatcher's exact shape.
        let all = ["rope_a", "rope_b", "rope_c_unclassified"];
        let bucket_a: &[&str] = &["rope_a"];
        let bucket_b: &[&str] = &["rope_b"];
        let report = rope_security::dispatcher_completeness::verify(&all, &[bucket_a, bucket_b])
            .expect_err("expected a completeness violation");
        assert_eq!(report.unclassified, vec!["rope_c_unclassified".to_string()]);
    }

    #[test]
    fn dev_only_evm_methods_default_off() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = env::var("ROPE_ALLOW_EVM_DEV_METHODS").ok();
        env::remove_var("ROPE_ALLOW_EVM_DEV_METHODS");
        assert!(!dev_only_evm_methods_enabled());
        if let Some(p) = prev {
            env::set_var("ROPE_ALLOW_EVM_DEV_METHODS", p);
        }
    }

    #[test]
    fn dev_only_evm_methods_explicit_on() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = env::var("ROPE_ALLOW_EVM_DEV_METHODS").ok();
        env::set_var("ROPE_ALLOW_EVM_DEV_METHODS", "1");
        assert!(dev_only_evm_methods_enabled());
        match prev {
            Some(p) => env::set_var("ROPE_ALLOW_EVM_DEV_METHODS", p),
            None => env::remove_var("ROPE_ALLOW_EVM_DEV_METHODS"),
        }
    }

    #[test]
    fn no_method_appears_in_more_than_one_hand_curated_bucket() {
        // Belt-and-suspenders on top of verify_dispatcher_completeness:
        // pins that the four buckets are pairwise disjoint even before
        // considering what the live dispatcher contains.
        let buckets: &[&[&str]] = &[
            DESTRUCTIVE_METHODS,
            SELF_AUTHENTICATED_METHODS,
            DEV_ONLY_EVM_METHODS,
            SAFE_READ_ONLY_METHODS,
        ];
        let mut seen = std::collections::HashSet::new();
        for bucket in buckets {
            for method in *bucket {
                assert!(
                    seen.insert(*method),
                    "method {method} appears in more than one hand-curated bucket"
                );
            }
        }
    }

    #[test]
    fn wallet_param0_and_param1_methods_are_subsets_of_destructive_methods() {
        // Every method whose wallet parameter we gate against the CERBER
        // signer blocklist must also be a method the V11/Phase-1/Phase-2
        // gates above already treat as state-mutating. If a future
        // maintainer adds a read-only method to either WALLET_PARAM*
        // list by mistake, this pins the invariant that would otherwise
        // silently regress.
        for method in WALLET_PARAM0_METHODS.iter().chain(WALLET_PARAM1_METHODS) {
            assert!(
                DESTRUCTIVE_METHODS.contains(method),
                "{method} is in a WALLET_PARAM* list but not in DESTRUCTIVE_METHODS"
            );
        }
    }

    #[test]
    fn wallet_param0_and_param1_methods_are_disjoint() {
        for m in WALLET_PARAM0_METHODS {
            assert!(
                !WALLET_PARAM1_METHODS.contains(m),
                "{m} listed in both WALLET_PARAM0_METHODS and WALLET_PARAM1_METHODS"
            );
        }
    }

    #[test]
    fn wallet_param_for_method_reads_param0() {
        let params = serde_json::json!(["0xabc", "unused"]);
        assert_eq!(
            wallet_param_for_method("rope_appendToLedger", Some(&params)),
            Some("0xabc")
        );
    }

    #[test]
    fn wallet_param_for_method_reads_param1() {
        let params = serde_json::json!(["agent-1", "0xdef"]);
        assert_eq!(
            wallet_param_for_method("rope_subscribeAgentToWallet", Some(&params)),
            Some("0xdef")
        );
    }

    #[test]
    fn wallet_param_for_method_is_none_for_unrelated_methods() {
        let params = serde_json::json!(["0xabc"]);
        assert_eq!(wallet_param_for_method("rope_globalStats", Some(&params)), None);
    }

    #[test]
    fn wallet_param_for_method_is_none_when_params_missing() {
        assert_eq!(wallet_param_for_method("rope_appendToLedger", None), None);
    }

    #[test]
    fn rpc_auth_denied_response_shape() {
        let r = denied_response(&serde_json::json!(42));
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 42);
        assert_eq!(v["error"]["code"], DENIED_ERROR_CODE);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("public listener"));
    }
}
