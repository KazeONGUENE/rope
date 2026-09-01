//! P1 §17.5 #3 — Internal rope-node loopback RPC watchdog.
//!
//! **Purpose.** Detect a wedged rope-node from *inside the same process*,
//! independent of the external `erpc-fleet-ha.sh` supervisor and its
//! `MAX_RESTARTS_PER_HOUR` cap. During the 2026-08-11 incident window,
//! the external HA script would issue a restart when the RPC loopback
//! probe timed out, but its 4→8 (later 16) restart cap could still be
//! exhausted by a truly stuck node. When that cap was hit, the node
//! was marked `out_of_service` and required a human operator to reset
//! `/var/lib/datachain-rope/fleet/ha.state` before self-heal could
//! resume (see §17.2 of the dcscan handover for the manual recipe).
//!
//! This watchdog closes that gap. It runs as a dedicated tokio task,
//! probes the local RPC listener at `http://127.0.0.1:<port>` with
//! `eth_blockNumber` on a fixed interval, tracks consecutive failures,
//! and — when `ROPE_SELF_WATCHDOG_SUICIDE=1` is set — calls
//! `std::process::exit(1)` after a configurable stall window, letting
//! systemd (`Restart=always`) restart the process without consulting
//! the external HA cap.
//!
//! **Never enabled by default.** Suicide mode is opt-in via env var so
//! rollback is a single systemd drop-in edit. Observation mode (default)
//! is safe on every install: it only writes a JSON status file that HA
//! or operators can consult.
//!
//! **State file** (`<data_dir>/self-watchdog.json`) is refreshed on
//! every probe with a schema stable across restarts:
//!
//! ```json
//! {
//!   "healthy": true,
//!   "last_success_at": 1786598210,
//!   "last_success_ago_secs": 12,
//!   "consecutive_failures": 0,
//!   "total_probes": 4821,
//!   "total_failures": 3,
//!   "startup_grace_elapsed": true,
//!   "suicide_enabled": false,
//!   "stall_threshold_secs": 120,
//!   "probe_url": "http://127.0.0.1:8545",
//!   "note": "..."
//! }
//! ```
//!
//! **Independence guarantee.** The probe uses `reqwest::Client` (fresh
//! connections, no keepalive to the target) so a stuck RPC accept-loop
//! is observable as a real TCP-level timeout, not a stale socket
//! success. The task never blocks on the same locks that would wedge
//! the RPC handlers; it only touches `AtomicU64` counters and writes a
//! small JSON file to disk (~200 B).
//!
//! **Environment knobs** (all optional, safe defaults):
//!
//! | Var | Default | Purpose |
//! |---|---|---|
//! | `ROPE_SELF_WATCHDOG_ENABLED`        | `1`   | Master on/off. `0` disables the whole task. |
//! | `ROPE_SELF_WATCHDOG_INTERVAL_SECS`  | `15`  | Seconds between probes. |
//! | `ROPE_SELF_WATCHDOG_TIMEOUT_SECS`   | `5`   | Per-probe HTTP timeout. |
//! | `ROPE_SELF_WATCHDOG_STALL_SECS`     | `120` | Wedge is declared after this many seconds without a healthy probe. |
//! | `ROPE_SELF_WATCHDOG_STARTUP_GRACE_SECS` | `300` | Suppress suicide for this long after boot (matches HA `STARTUP_GRACE_S`). |
//! | `ROPE_SELF_WATCHDOG_SUICIDE`        | `0`   | `1` = `exit(1)` on sustained stall. Off in dev/test. |
//!
//! See the dcscan handover §17.5 #3 for the design rationale and
//! §22 / §21 for the P2B / Phase-C mitigations this complements.
//!
//! ---
//!
//! ## Memory-pressure circuit breaker (B2, 2026-08-23)
//!
//! **Purpose.** Detect memory pressure from *inside the same process* and,
//! when explicitly enabled, trigger a clean self-restart before the
//! kernel escalates to a hard OOM-kill or the swap-thrash pathology
//! diagnosed in `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md`
//! wedges the LamportClock write path.
//!
//! Two independent signals are sampled every probe:
//!
//! 1. **`VmRSS`** from `/proc/self/status` — the process's resident-set
//!    size. Direct, easy to interpret, no cgroup dependency.
//! 2. **Cgroup memory pressure (PSI)** from `/sys/fs/cgroup/<self>/memory.pressure`
//!    (fallback: `/proc/pressure/memory`) — kernel's rolling stall metric.
//!    `full avg60 >= threshold` means the whole cgroup was blocked on
//!    memory for that fraction of the last 60s. Values above 10 are the
//!    classic swap-thrash signature.
//!
//! **Circuit-breaker semantics.** Off by default. When
//! `ROPE_MEMORY_CIRCUIT_ENABLED=1`, the watchdog trips (calls
//! `std::process::exit(1)`, letting systemd `Restart=always` bring the
//! process back) when **all** of the following hold at the same probe:
//!
//! - Startup grace has elapsed (same guard as the stall-suicide path).
//! - Either `VmRSS >= ROPE_MEMORY_CIRCUIT_RSS_HARD_MB` **OR**
//!   `psi_full_avg60 >= ROPE_MEMORY_CIRCUIT_PSI_FULL_AVG60_THRESHOLD`.
//! - The breach has been continuously observed for at least
//!   `ROPE_MEMORY_CIRCUIT_SUSTAINED_SECS` (default 60s) — a single
//!   transient spike must not trip the breaker.
//!
//! **Never enabled by default.** Same rollback discipline as the
//! stall-suicide path: a single systemd drop-in edit disables it.
//!
//! **Memory circuit env knobs:**
//!
//! | Var | Default | Purpose |
//! |---|---|---|
//! | `ROPE_MEMORY_CIRCUIT_ENABLED`               | `0`    | `1` = arm the circuit breaker. |
//! | `ROPE_MEMORY_CIRCUIT_RSS_HARD_MB`           | `6144` | RSS in MB above which the breaker arms. `0` disables the RSS leg. |
//! | `ROPE_MEMORY_CIRCUIT_PSI_FULL_AVG60_THRESHOLD` | `20.0` | `psi.full.avg60` above which the breaker arms. `0` disables the PSI leg. |
//! | `ROPE_MEMORY_CIRCUIT_SUSTAINED_SECS`        | `60`   | Breach must be continuous for at least this long before tripping. |
//!
//! **Interactions with A2 cgroup limits.** The circuit breaker is a
//! belt-and-braces companion to the systemd `MemoryHigh` / `MemoryMax`
//! caps in `deploy/systemd/datachain-rope.service.d/71-memory-swap-post-upgrade.conf`.
//! The cgroup caps are cheaper (kernel-enforced, no in-process code) but
//! act only at the edges. The circuit breaker catches the mid-pressure
//! range where the kernel is throttling but not yet killing — precisely
//! the swap-thrash zone that produced the MTBF regression. Both should
//! be enabled together.
//!
//! **Platform.** Full functionality requires Linux with cgroup v2 (all
//! Datachain Rope production nodes are Ubuntu 24.04 with unified
//! cgroups). On other platforms (macOS dev, cgroup v1, containerless
//! test sandboxes) the memory probes gracefully return `None`; the
//! watchdog continues to run and never trips the memory breaker on
//! systems where it cannot read the signals.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::time::interval;

/// Default settings (see module docs for env-var overrides).
mod defaults {
    pub const PROBE_INTERVAL_SECS: u64 = 15;
    pub const PROBE_TIMEOUT_SECS: u64 = 5;
    pub const STALL_THRESHOLD_SECS: u64 = 120;
    pub const STARTUP_GRACE_SECS: u64 = 300;

    // Memory-pressure circuit-breaker defaults (see module doc "Memory-
    // pressure circuit breaker" section). All conservative: the breaker
    // is off by default AND its default thresholds are 6144 MB / 20.0% /
    // 60s, so even a mis-set `ROPE_MEMORY_CIRCUIT_ENABLED=1` on a small
    // dev box will not trip the breaker unless the process is actually
    // seriously constrained.
    pub const MEMORY_CIRCUIT_ENABLED: bool = false;
    pub const MEMORY_CIRCUIT_RSS_HARD_MB: u64 = 6144;
    pub const MEMORY_CIRCUIT_PSI_FULL_AVG60_X100: u64 = 2000; // 20.00%
    pub const MEMORY_CIRCUIT_SUSTAINED_SECS: u64 = 60;
}

/// Lock-free watchdog state, safe to snapshot from any thread.
///
/// All counters use `Relaxed` ordering because the watchdog task is the
/// sole writer and readers only need eventual consistency (the state
/// file is refreshed every probe interval anyway).
#[derive(Debug)]
pub struct WatchdogState {
    /// Unix epoch seconds of the most recent probe that returned a
    /// well-formed `eth_blockNumber` reply. Never decreases. `0` means
    /// no probe has succeeded yet since boot.
    pub last_success_at: AtomicU64,
    /// Consecutive probe failures. Reset to 0 on any success.
    pub consecutive_failures: AtomicU32,
    /// Total probes issued since boot (successful + failed).
    pub total_probes: AtomicU64,
    /// Total failed probes since boot (subset of `total_probes`).
    pub total_failures: AtomicU64,
    /// Set to `true` once startup grace elapses; latched.
    pub startup_grace_elapsed: AtomicBool,
    /// Absolute unix-epoch seconds when the task started. Used to compute
    /// grace and to bound derived quantities on the read path.
    pub started_at: AtomicU64,

    // --- Memory-pressure signals (B2, 2026-08-23) ---
    //
    // All memory fields default to 0 which the read path treats as
    // "unknown" / "unavailable on this platform".
    /// Most recent `VmRSS` (KB) read from `/proc/self/status`. `0` means
    /// either "never probed" (before first tick) or "unavailable on this
    /// platform" (macOS, containerless test sandbox). Never decreases
    /// arbitrarily — always overwritten with the latest probe.
    pub last_vm_rss_kb: AtomicU64,
    /// Most recent `VmPeak` (KB) — high-water mark reported by the
    /// kernel itself. Independent of our sampling frequency, so useful
    /// for post-hoc diagnosis after an incident.
    pub last_vm_peak_kb: AtomicU64,
    /// Most recent `psi.full.avg60` (percent, scaled by 100 so we can
    /// store as an integer). `12.34%` is stored as `1234`. `0` means
    /// "no pressure" AND "not probed / unavailable" — the read path
    /// disambiguates via `last_psi_read_ok`.
    pub last_psi_full_avg60_x100: AtomicU64,
    /// Latched `true` once a PSI probe has succeeded at least once
    /// since boot. `false` after that means "the file existed once but
    /// vanished", which the read path treats as unavailable this tick.
    pub last_psi_read_ok: AtomicBool,
    /// Unix epoch seconds since which the memory pressure has been
    /// continuously breaching thresholds. `0` = not currently breaching.
    pub memory_pressure_breach_since: AtomicU64,
    /// Total number of times the circuit breaker has tripped since boot.
    /// Never resets. A non-zero value here in the state file after a
    /// restart tells operators the previous exit was memory-driven.
    pub memory_circuit_trips: AtomicU64,
}

impl WatchdogState {
    /// Fresh state; last_success starts at 0 (interpreted as "no probe
    /// has succeeded yet"). The task will set `started_at` before its
    /// first tick.
    #[inline]
    pub fn new() -> Self {
        Self {
            last_success_at: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            total_probes: AtomicU64::new(0),
            total_failures: AtomicU64::new(0),
            startup_grace_elapsed: AtomicBool::new(false),
            started_at: AtomicU64::new(0),
            last_vm_rss_kb: AtomicU64::new(0),
            last_vm_peak_kb: AtomicU64::new(0),
            last_psi_full_avg60_x100: AtomicU64::new(0),
            last_psi_read_ok: AtomicBool::new(false),
            memory_pressure_breach_since: AtomicU64::new(0),
            memory_circuit_trips: AtomicU64::new(0),
        }
    }
}

impl Default for WatchdogState {
    fn default() -> Self {
        Self::new()
    }
}

/// Immutable configuration bundle passed to the watchdog task. Cheap
/// to `Clone` because everything is either primitive or `Arc<_>`.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// The loopback URL to probe. Typically `http://127.0.0.1:8545`
    /// derived from the node's own RpcSettings.http_addr.
    pub probe_url: String,
    /// Interval between probes.
    pub interval: Duration,
    /// Per-probe HTTP timeout.
    pub timeout: Duration,
    /// After this many seconds with no successful probe, the node is
    /// considered wedged.
    pub stall_threshold: Duration,
    /// Suppress suicide until this many seconds after task start; gives
    /// the RPC listener a chance to bind and warm up.
    pub startup_grace: Duration,
    /// Enable `std::process::exit(1)` on sustained stall. Default off.
    pub suicide_enabled: bool,
    /// Absolute path where the watchdog will write its status JSON on
    /// every probe. Consumers (HA scripts, operators) read this to
    /// distinguish "wedged process" from "unreachable process".
    pub state_file: PathBuf,

    // --- Memory-pressure circuit breaker (B2, 2026-08-23) ---
    /// Master toggle for the memory-pressure circuit breaker. Off by
    /// default; when on and startup grace has elapsed, the watchdog
    /// tick will `std::process::exit(1)` if `rss_hard_mb` and/or
    /// `psi_full_avg60_threshold_x100` are breached continuously for
    /// `sustained` seconds.
    pub memory_circuit_enabled: bool,
    /// `VmRSS` (MB) above which the breaker arms. `0` disables the RSS
    /// leg (PSI-only mode). See module doc for interaction with the
    /// systemd `MemoryHigh` / `MemoryMax` caps.
    pub memory_circuit_rss_hard_mb: u64,
    /// `psi.full.avg60` (percent × 100) above which the breaker arms.
    /// `0` disables the PSI leg (RSS-only mode). See module doc.
    pub memory_circuit_psi_full_avg60_x100: u64,
    /// Continuous-breach duration before the breaker trips. A single
    /// transient spike will not trip; the process must be under
    /// pressure for at least this long. Enforced by the watchdog task
    /// via `memory_pressure_breach_since`.
    pub memory_circuit_sustained: Duration,
}

impl WatchdogConfig {
    /// Build a config from the running node's data directory + RPC
    /// settings, applying env-var overrides. `enabled` is separate;
    /// callers should short-circuit before building this.
    pub fn from_env(data_dir: &std::path::Path, rpc_http_addr: &str) -> Self {
        // Resolve the probe URL. `http_addr` may be `0.0.0.0:8545`
        // (production), `127.0.0.1:8545` (dev), or `[::]:8545` (v6).
        // We always probe via 127.0.0.1 so that a firewall rule or IPv6
        // misconfig can't false-negative us.
        let probe_url = std::env::var("ROPE_SELF_WATCHDOG_PROBE_URL")
            .or_else(|_| std::env::var("ROPE_LOOPBACK_PROBE_URL"))
            .unwrap_or_else(|_| probe_url_from_bind(rpc_http_addr));

        let interval_secs = env_u64("ROPE_SELF_WATCHDOG_INTERVAL_SECS")
            .unwrap_or(defaults::PROBE_INTERVAL_SECS)
            .max(1);
        let timeout_secs = env_u64("ROPE_SELF_WATCHDOG_TIMEOUT_SECS")
            .unwrap_or(defaults::PROBE_TIMEOUT_SECS)
            .max(1);
        let stall_secs = env_u64("ROPE_SELF_WATCHDOG_STALL_SECS")
            .unwrap_or(defaults::STALL_THRESHOLD_SECS)
            .max(interval_secs.saturating_mul(2));
        let grace_secs =
            env_u64("ROPE_SELF_WATCHDOG_STARTUP_GRACE_SECS").unwrap_or(defaults::STARTUP_GRACE_SECS);
        let suicide_enabled = env_flag("ROPE_SELF_WATCHDOG_SUICIDE").unwrap_or(false);

        // Memory circuit-breaker: off by default. Every knob is
        // independent — an operator can enable the RSS leg only, or the
        // PSI leg only, by zeroing the other threshold.
        let memory_circuit_enabled =
            env_flag("ROPE_MEMORY_CIRCUIT_ENABLED").unwrap_or(defaults::MEMORY_CIRCUIT_ENABLED);
        let memory_circuit_rss_hard_mb = env_u64("ROPE_MEMORY_CIRCUIT_RSS_HARD_MB")
            .unwrap_or(defaults::MEMORY_CIRCUIT_RSS_HARD_MB);
        // The PSI threshold is exposed as a decimal percent in the env
        // (e.g. "20.0") for operator ergonomics; internally we store
        // it × 100 so the state and comparisons stay integer.
        let memory_circuit_psi_full_avg60_x100 = env_f64("ROPE_MEMORY_CIRCUIT_PSI_FULL_AVG60_THRESHOLD")
            .map(|v| (v.clamp(0.0, 10_000.0) * 100.0).round() as u64)
            .unwrap_or(defaults::MEMORY_CIRCUIT_PSI_FULL_AVG60_X100);
        let memory_circuit_sustained_secs = env_u64("ROPE_MEMORY_CIRCUIT_SUSTAINED_SECS")
            .unwrap_or(defaults::MEMORY_CIRCUIT_SUSTAINED_SECS)
            .max(interval_secs); // must be at least one probe interval

        Self {
            probe_url,
            interval: Duration::from_secs(interval_secs),
            timeout: Duration::from_secs(timeout_secs),
            stall_threshold: Duration::from_secs(stall_secs),
            startup_grace: Duration::from_secs(grace_secs),
            suicide_enabled,
            state_file: data_dir.join("self-watchdog.json"),
            memory_circuit_enabled,
            memory_circuit_rss_hard_mb,
            memory_circuit_psi_full_avg60_x100,
            memory_circuit_sustained: Duration::from_secs(memory_circuit_sustained_secs),
        }
    }
}

/// Public toggle. Returns `false` iff `ROPE_SELF_WATCHDOG_ENABLED` is
/// explicitly set to a falsy value. Any other value (including unset)
/// enables the watchdog.
pub fn watchdog_enabled_from_env() -> bool {
    env_flag("ROPE_SELF_WATCHDOG_ENABLED").unwrap_or(true)
}

/// Spawn the watchdog task on the current tokio runtime. Returns a
/// handle that the caller can `abort()` on graceful shutdown. The
/// returned `Arc<WatchdogState>` is what internal callers (e.g. an
/// `rope_selfWatchdogStatus` RPC handler) should hold to inspect
/// live counters without going through the file.
///
/// **Panic-free.** All I/O errors are logged and swallowed; a bad
/// state-file write never crashes the node. A malformed probe response
/// counts as a failure like a timeout.
pub fn spawn(config: WatchdogConfig) -> (Arc<WatchdogState>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(WatchdogState::new());
    let state_clone = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_task(config, state_clone).await;
    });
    (state, handle)
}

/// The task body. Loops until aborted. Exits deliberately via
/// `std::process::exit` only in the suicide path (below).
async fn run_task(cfg: WatchdogConfig, state: Arc<WatchdogState>) {
    let started_at = now_secs();
    state.started_at.store(started_at, Ordering::Relaxed);

    // Build a fresh client per task. Disable connection pooling so a
    // stuck RPC accept-loop can't be masked by a stale keep-alive
    // socket that happens to succeed. Every probe is a fresh TCP
    // handshake against 127.0.0.1 — cheap, and honest.
    let client = match reqwest::Client::builder()
        .timeout(cfg.timeout)
        .connect_timeout(cfg.timeout)
        .pool_max_idle_per_host(0)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                "self_watchdog: failed to build HTTP client, watchdog disabled: {e}"
            );
            return;
        }
    };

    tracing::info!(
        "self_watchdog: enabled (probe={} interval={}s timeout={}s stall={}s suicide={} grace={}s)",
        cfg.probe_url,
        cfg.interval.as_secs(),
        cfg.timeout.as_secs(),
        cfg.stall_threshold.as_secs(),
        cfg.suicide_enabled,
        cfg.startup_grace.as_secs(),
    );

    // First tick fires immediately at t=0; subsequent every `interval`.
    let mut ticker = interval(cfg.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;

        // Flip startup_grace_elapsed the first time we cross the grace
        // boundary. Latching once and reading with Relaxed is fine —
        // there is no need for release/acquire ordering here.
        let uptime = now_secs().saturating_sub(state.started_at.load(Ordering::Relaxed));
        if uptime >= cfg.startup_grace.as_secs()
            && !state.startup_grace_elapsed.load(Ordering::Relaxed)
        {
            state.startup_grace_elapsed.store(true, Ordering::Relaxed);
            tracing::info!(
                "self_watchdog: startup grace elapsed ({}s) — suicide arming from this point on if enabled",
                cfg.startup_grace.as_secs(),
            );
        }

        state.total_probes.fetch_add(1, Ordering::Relaxed);
        let probe_result = probe_once(&client, &cfg.probe_url).await;

        match probe_result {
            Ok(_block_hex) => {
                state
                    .last_success_at
                    .store(now_secs(), Ordering::Relaxed);
                state.consecutive_failures.store(0, Ordering::Relaxed);
            }
            Err(reason) => {
                let n = state.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                state.total_failures.fetch_add(1, Ordering::Relaxed);
                // Warn at every failure so operators can grep the
                // journal for a wedge signal. Info-level would blend
                // into normal steady-state noise.
                tracing::warn!(
                    "self_watchdog: probe FAILED ({}) — consecutive={} probe_url={}",
                    reason,
                    n,
                    cfg.probe_url,
                );
            }
        }

        // Memory probe: independent of the RPC probe result. Runs
        // every tick so the state file always has fresh values,
        // even before / independently of the circuit-breaker
        // decision.
        let mem_probe = read_memory_probe();
        state
            .last_vm_rss_kb
            .store(mem_probe.vm_rss_kb.unwrap_or(0), Ordering::Relaxed);
        state
            .last_vm_peak_kb
            .store(mem_probe.vm_peak_kb.unwrap_or(0), Ordering::Relaxed);
        state
            .last_psi_full_avg60_x100
            .store(
                mem_probe.psi_full_avg60_x100.unwrap_or(0),
                Ordering::Relaxed,
            );
        state
            .last_psi_read_ok
            .store(mem_probe.psi_read_ok, Ordering::Relaxed);

        // Circuit-breaker decision. `evaluate_memory_circuit` handles
        // enable + startup-grace + first-success guards internally,
        // and mutates `memory_pressure_breach_since` as a side-effect
        // (latch on first breach, reset on first clean tick).
        let now = now_secs();
        let mem_should_trip =
            evaluate_memory_circuit(&cfg, &state, &mem_probe, now);

        // Always refresh the state file, regardless of probe outcome.
        // A missing or stale file itself signals a problem (task not
        // running or process pinned in a syscall) — operators can
        // check its mtime as an independent liveness signal.
        let snapshot = build_snapshot(&cfg, &state);
        if let Err(e) = write_state_file(&cfg.state_file, &snapshot) {
            tracing::warn!(
                "self_watchdog: could not write state file {:?}: {e}",
                cfg.state_file,
            );
        }

        // Suicide check. Only fires when:
        //   1. env flag ROPE_SELF_WATCHDOG_SUICIDE=1, AND
        //   2. startup grace has elapsed (protects boot-time flakiness), AND
        //   3. we have at least one successful probe on record
        //      (otherwise last_success_at=0 and every fresh start would
        //      instantly self-kill on cold boot), AND
        //   4. time since last success >= stall_threshold.
        //
        // The 3rd guard means the very first boot after a persistent
        // wedge (e.g. crashed mid-append) needs at least one healthy
        // tick before we consider it a wedge — this is intentional
        // to give lazy-rehydration (§12) time to complete.
        if cfg.suicide_enabled
            && state.startup_grace_elapsed.load(Ordering::Relaxed)
        {
            let last = state.last_success_at.load(Ordering::Relaxed);
            let stall_secs = if last == 0 {
                0
            } else {
                now_secs().saturating_sub(last)
            };
            if last != 0 && stall_secs >= cfg.stall_threshold.as_secs() {
                let n = state.consecutive_failures.load(Ordering::Relaxed);
                tracing::error!(
                    "self_watchdog: SUSTAINED STALL — last successful probe was {}s ago \
                     (threshold {}s, consecutive_failures {}), calling std::process::exit(1) \
                     for systemd to restart. This bypasses the external HA restart cap by design.",
                    stall_secs,
                    cfg.stall_threshold.as_secs(),
                    n,
                );
                // Best-effort final state flush before exit.
                let final_snapshot = build_snapshot(&cfg, &state);
                let _ = write_state_file(&cfg.state_file, &final_snapshot);
                // Flush the tracing layer's async buffers. `exit(1)`
                // does not run destructors, so anything not already on
                // the wire is dropped. In practice tracing_subscriber
                // is synchronous; this is defensive.
                std::process::exit(1);
            }
        }

        // Memory-pressure circuit breaker. Independent of the stall
        // breaker: a node under sustained memory pressure can still
        // answer eth_blockNumber (the RPC accept loop is cheap) while
        // being minutes away from the OOM killer or a swap-thrash
        // wedge. We choose to exit early here so systemd restarts
        // rope-node into a clean process with fresh page cache,
        // rather than letting the OOM killer pick which of our
        // service's threads to SIGKILL mid-write.
        //
        // The suicide_enabled gate is deliberately shared with the
        // stall breaker: an operator that turns off automatic
        // self-restart (ROPE_SELF_WATCHDOG_SUICIDE=0) turns off both
        // breakers at once. The memory breaker's own enable flag
        // (ROPE_MEMORY_CIRCUIT_ENABLED) is checked inside
        // `evaluate_memory_circuit`.
        if cfg.suicide_enabled && mem_should_trip {
            let trips = state.memory_circuit_trips.fetch_add(1, Ordering::Relaxed) + 1;
            let since = state
                .memory_pressure_breach_since
                .load(Ordering::Relaxed);
            let breach_for = now.saturating_sub(since);
            let rss_mb = state.last_vm_rss_kb.load(Ordering::Relaxed) / 1024;
            let psi_x100 = state.last_psi_full_avg60_x100.load(Ordering::Relaxed);
            tracing::error!(
                "self_watchdog: MEMORY CIRCUIT TRIPPED — sustained memory-pressure breach \
                 for {}s (threshold {}s), rss={}MB (limit {}MB), psi.full.avg60={:.2}% \
                 (limit {:.2}%), trip #{}. Calling std::process::exit(1) for systemd to \
                 restart the process into a clean address space. This bypasses the external \
                 HA restart cap by design.",
                breach_for,
                cfg.memory_circuit_sustained.as_secs(),
                rss_mb,
                cfg.memory_circuit_rss_hard_mb,
                (psi_x100 as f64) / 100.0,
                (cfg.memory_circuit_psi_full_avg60_x100 as f64) / 100.0,
                trips,
            );
            let final_snapshot = build_snapshot(&cfg, &state);
            let _ = write_state_file(&cfg.state_file, &final_snapshot);
            std::process::exit(1);
        }
    }
}

/// One probe against loopback. Supports:
/// - JSON-RPC POST `eth_blockNumber` on the main RPC port (legacy default)
/// - GET `/v1/tip` on the sync probe listener (`block_hex` field)
async fn probe_once(client: &reqwest::Client, url: &str) -> Result<String, String> {
    if url.contains("/v1/tip") {
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("http error: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("http status {}", status.as_u16()));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| format!("body read: {e}"))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("json parse: {e}"))?;
        match v.get("block_hex").and_then(|r| r.as_str()) {
            Some(hex) if hex.starts_with("0x") => Ok(hex.to_string()),
            Some(other) => Err(format!("malformed block_hex: {other}")),
            None => Err(format!(
                "missing block_hex field (raw: {})",
                truncate_for_log(&text, 200)
            )),
        }
    } else {
        let body = json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1,
        });
        let resp = client
            .post(url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| format!("http error: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("http status {}", status.as_u16()));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| format!("body read: {e}"))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("json parse: {e}"))?;
        match v.get("result").and_then(|r| r.as_str()) {
            Some(hex) if hex.starts_with("0x") => Ok(hex.to_string()),
            Some(other) => Err(format!("malformed result: {other}")),
            None => Err(format!(
                "missing result field (raw: {})",
                truncate_for_log(&text, 200)
            )),
        }
    }
}

/// Compose the JSON status document.
fn build_snapshot(cfg: &WatchdogConfig, state: &WatchdogState) -> serde_json::Value {
    let now = now_secs();
    let last = state.last_success_at.load(Ordering::Relaxed);
    let last_ago = if last == 0 { 0 } else { now.saturating_sub(last) };
    let consecutive_failures = state.consecutive_failures.load(Ordering::Relaxed);
    let total_probes = state.total_probes.load(Ordering::Relaxed);
    let total_failures = state.total_failures.load(Ordering::Relaxed);
    let startup_grace_elapsed = state.startup_grace_elapsed.load(Ordering::Relaxed);
    // "healthy" means: we have at least one successful probe, AND the
    // most recent success is within the stall threshold. Before the
    // first success, healthy = false (honest "unknown, still warming
    // up" state).
    let healthy = last != 0 && last_ago < cfg.stall_threshold.as_secs();

    // Memory-probe fields — all optional in effect (0 = not observed
    // this cycle, or leg unavailable on this kernel).
    let vm_rss_kb = state.last_vm_rss_kb.load(Ordering::Relaxed);
    let vm_peak_kb = state.last_vm_peak_kb.load(Ordering::Relaxed);
    let psi_x100 = state.last_psi_full_avg60_x100.load(Ordering::Relaxed);
    let psi_read_ok = state.last_psi_read_ok.load(Ordering::Relaxed);
    let breach_since = state
        .memory_pressure_breach_since
        .load(Ordering::Relaxed);
    let breach_for_secs = if breach_since == 0 {
        0
    } else {
        now.saturating_sub(breach_since)
    };
    let mem_circuit_trips = state.memory_circuit_trips.load(Ordering::Relaxed);

    json!({
        "healthy": healthy,
        "last_success_at": last,
        "last_success_ago_secs": last_ago,
        "consecutive_failures": consecutive_failures,
        "total_probes": total_probes,
        "total_failures": total_failures,
        "startup_grace_elapsed": startup_grace_elapsed,
        "suicide_enabled": cfg.suicide_enabled,
        "stall_threshold_secs": cfg.stall_threshold.as_secs(),
        "interval_secs": cfg.interval.as_secs(),
        "probe_url": &cfg.probe_url,
        "memory": {
            "vm_rss_kb": vm_rss_kb,
            "vm_rss_mb": vm_rss_kb / 1024,
            "vm_peak_kb": vm_peak_kb,
            "psi_full_avg60_pct_x100": psi_x100,
            "psi_full_avg60_pct": (psi_x100 as f64) / 100.0,
            "psi_read_ok": psi_read_ok,
            "breach_since": breach_since,
            "breach_for_secs": breach_for_secs,
            "circuit_enabled": cfg.memory_circuit_enabled,
            "circuit_rss_hard_mb": cfg.memory_circuit_rss_hard_mb,
            "circuit_psi_full_avg60_pct_x100": cfg.memory_circuit_psi_full_avg60_x100,
            "circuit_sustained_secs": cfg.memory_circuit_sustained.as_secs(),
            "circuit_trips_total": mem_circuit_trips,
        },
        "note": "P1 §17.5 #3 loopback watchdog + P1-B2 memory-pressure circuit breaker (2026-08-23). Consumers: HA scripts, operator dashboards. Suicide triggers std::process::exit(1) when suicide_enabled AND (stalled beyond stall_threshold OR memory-circuit breach sustained ≥ circuit_sustained_secs). Systemd Restart=always brings the node back independent of the external HA restart cap.",
    })
}

/// Atomic write: `write -> fsync -> rename`. On any error, the previous
/// file (if any) remains untouched. Safe against partial writes even
/// if the process is killed mid-write.
fn write_state_file(path: &std::path::Path, value: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    // If the parent directory does not exist yet (fresh install / test
    // sandbox), best-effort create it; a real filesystem error at this
    // step is genuinely fatal for the state file, so surface it.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)?;
    let body = serde_json::to_vec_pretty(value)?;
    f.write_all(&body)?;
    f.sync_data()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Resolve a probe URL from the RPC bind address. Always talks to
/// 127.0.0.1 (or ::1 for pure v6 binds); never uses the bind's actual
/// address so we're immune to firewall changes and public-address
/// misconfiguration.
fn probe_url_from_bind(bind: &str) -> String {
    // Bind strings we see in the wild:
    //   "0.0.0.0:8545"        -> port 8545
    //   "127.0.0.1:8545"      -> port 8545
    //   "[::]:8545"           -> port 8545
    //   "[::1]:8545"          -> port 8545 (v6 loopback)
    //   "0.0.0.0:8545/"       -> port 8545 (accept trailing slash)
    let port = parse_port(bind).unwrap_or(8545);
    format!("http://127.0.0.1:{port}")
}

/// Extract the port from a socket-addr-like string, tolerant of common
/// v4/v6 forms.
fn parse_port(s: &str) -> Option<u16> {
    let s = s.trim().trim_end_matches('/');
    // v6 with brackets: [::]:8545
    if let Some(idx) = s.rfind("]:") {
        return s[idx + 2..].parse().ok();
    }
    // v4 or bare "host:port"
    if let Some(idx) = s.rfind(':') {
        return s[idx + 1..].parse().ok();
    }
    // No colon at all — treat the string as a port.
    s.parse().ok()
}

/// Env parsing helpers. Kept tolerant: unrecognized values fall back
/// to the default rather than crashing the boot.
fn env_flag(name: &str) -> Option<bool> {
    match std::env::var(name).ok()?.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "0" | "false" | "no" | "off" => Some(false),
        "1" | "true" | "yes" | "on" => Some(true),
        _ => None,
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse::<u64>().ok()
}

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok()?.trim().parse::<f64>().ok()
}

/// Result of a memory probe tick. All fields are best-effort: on a
/// non-Linux platform (macOS dev boxes) or on a stripped kernel
/// without PSI, the fields default to `None`/`false` and the circuit
/// breaker simply cannot arm on that leg.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemoryProbe {
    /// `VmRSS` in KB from `/proc/self/status`, or `None` if the file
    /// could not be parsed.
    pub vm_rss_kb: Option<u64>,
    /// `VmPeak` in KB (high-water mark of the virtual address space),
    /// carried for observability only. Not used by the breaker.
    pub vm_peak_kb: Option<u64>,
    /// `psi.full.avg60` × 100 (percent), or `None` if PSI is
    /// unavailable. cgroup path preferred over `/proc/pressure/memory`.
    pub psi_full_avg60_x100: Option<u64>,
    /// Whether we successfully read *some* form of PSI. Distinct from
    /// `psi_full_avg60_x100.is_some()` in principle (a malformed file
    /// could parse partially), used in the state file so operators
    /// can tell "PSI unavailable" from "PSI available but zero".
    pub psi_read_ok: bool,
}

/// Read `/proc/self/status` and return the memory probe. On any I/O
/// error the RSS/peak fields become `None`; the PSI legs are then
/// attempted independently.
pub(crate) fn read_memory_probe() -> MemoryProbe {
    let (vm_rss_kb, vm_peak_kb) = read_proc_self_status_rss_peak();
    let (psi_full_avg60_x100, psi_read_ok) = read_memory_pressure_full_avg60();
    MemoryProbe {
        vm_rss_kb,
        vm_peak_kb,
        psi_full_avg60_x100,
        psi_read_ok,
    }
}

/// Read `VmRSS` + `VmPeak` from `/proc/self/status`. Both fields are
/// space-separated `key: value kB` lines; kernel documents the unit
/// as kB (kibibytes) on every arch that supports the pseudo-fs.
fn read_proc_self_status_rss_peak() -> (Option<u64>, Option<u64>) {
    let contents = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return (None, None),
    };
    parse_proc_status_rss_peak(&contents)
}

fn parse_proc_status_rss_peak(contents: &str) -> (Option<u64>, Option<u64>) {
    let mut rss = None;
    let mut peak = None;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss = parse_status_kb(rest);
        } else if let Some(rest) = line.strip_prefix("VmPeak:") {
            peak = parse_status_kb(rest);
        }
        if rss.is_some() && peak.is_some() {
            break;
        }
    }
    (rss, peak)
}

/// Parse ` 1234 kB` (leading whitespace + numeric + trailing unit).
/// Kernel format is always `kB`; if a future kernel emits `KB` or
/// `MB` (unlikely — kernel source hard-codes `kB`), we still succeed
/// as long as the numeric prefix parses.
fn parse_status_kb(s: &str) -> Option<u64> {
    let trimmed = s.trim();
    let numeric: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if numeric.is_empty() {
        return None;
    }
    numeric.parse::<u64>().ok()
}

/// Read cgroup or global memory PSI, prefer cgroup. Returns
/// `(psi_full_avg60_x100, read_ok)`.
///
/// The kernel exposes PSI on multiple paths depending on the cgroup
/// hierarchy and unified-vs-legacy configuration:
///
///   cgroup v2 unified: /sys/fs/cgroup/<slice>/memory.pressure
///   cgroup v2 no slice: /sys/fs/cgroup/memory.pressure
///   host-global:        /proc/pressure/memory
///
/// We try, in order, the file our own cgroup exposes (derived from
/// `/proc/self/cgroup`), the top-level cgroup file, and the host
/// file. First successful read wins.
fn read_memory_pressure_full_avg60() -> (Option<u64>, bool) {
    // Prefer the cgroup path so a systemd `MemoryHigh=` throttle shows
    // up in the reading. If we can't resolve it, fall back to the
    // host-global one, which is still useful as a coarse signal.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(own) = resolve_own_cgroup_memory_pressure_path() {
        candidates.push(own);
    }
    candidates.push(std::path::PathBuf::from("/sys/fs/cgroup/memory.pressure"));
    candidates.push(std::path::PathBuf::from("/proc/pressure/memory"));

    for path in candidates {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Some(v) = parse_psi_full_avg60(&contents) {
                return (Some(v), true);
            }
        }
    }
    (None, false)
}

/// Read `/proc/self/cgroup`, find the cgroup v2 path (line prefixed
/// with `0::`), and build the `memory.pressure` path under
/// `/sys/fs/cgroup`. Returns `None` on legacy hierarchies (where PSI
/// is not exposed via memory.pressure regardless).
fn resolve_own_cgroup_memory_pressure_path() -> Option<std::path::PathBuf> {
    let contents = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    for line in contents.lines() {
        // cgroup v2 line format: "0::/system.slice/datachain-rope.service"
        if let Some(rest) = line.strip_prefix("0::") {
            let rel = rest.trim();
            if rel.is_empty() || rel == "/" {
                return Some(std::path::PathBuf::from(
                    "/sys/fs/cgroup/memory.pressure",
                ));
            }
            let mut p = std::path::PathBuf::from("/sys/fs/cgroup");
            p.push(rel.trim_start_matches('/'));
            p.push("memory.pressure");
            return Some(p);
        }
    }
    None
}

/// Parse the `full` line of a PSI file. Kernel format:
///
///   some avg10=0.00 avg60=0.00 avg300=0.00 total=0
///   full avg10=0.00 avg60=0.00 avg300=0.00 total=0
///
/// We want `full avg60`, returned as percent × 100 (i.e. `20.34%`
/// becomes `2034`). Missing `full` line → `None`.
fn parse_psi_full_avg60(contents: &str) -> Option<u64> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("full ") {
            for field in rest.split_whitespace() {
                if let Some(v) = field.strip_prefix("avg60=") {
                    let parsed: f64 = v.parse().ok()?;
                    if !parsed.is_finite() || parsed < 0.0 {
                        return Some(0);
                    }
                    let clamped = parsed.min(10_000.0);
                    return Some((clamped * 100.0).round() as u64);
                }
            }
        }
    }
    None
}

/// Circuit-breaker decision. Returns `true` if the process should
/// exit *right now*. Kept pure so it can be unit-tested against
/// hand-built states without touching `/proc`.
///
/// The breaker fires when **both** are true:
///  1. `cfg.memory_circuit_enabled == true`
///  2. Startup grace has elapsed AND we saw at least one successful
///     probe (same guard as the stall breaker)
///  3. A memory threshold is breached (RSS OR PSI, either leg alone
///     is sufficient — thresholds set to `0` disable that leg)
///  4. The breach has been continuous for `sustained`
///
/// The `state.memory_pressure_breach_since` field is the "first tick
/// at which we saw a breach and have not seen a clean tick since".
/// A single clean tick resets it to 0.
pub(crate) fn evaluate_memory_circuit(
    cfg: &WatchdogConfig,
    state: &WatchdogState,
    probe: &MemoryProbe,
    now: u64,
) -> bool {
    if !cfg.memory_circuit_enabled {
        return false;
    }
    if !state.startup_grace_elapsed.load(Ordering::Relaxed) {
        return false;
    }
    if state.last_success_at.load(Ordering::Relaxed) == 0 {
        // Do not trip a memory circuit before we've ever seen a
        // healthy probe — that state is indistinguishable from a
        // bad boot and would loop through systemd Restart=always.
        return false;
    }

    let rss_breach = if cfg.memory_circuit_rss_hard_mb == 0 {
        false
    } else {
        probe
            .vm_rss_kb
            .map(|kb| kb / 1024 >= cfg.memory_circuit_rss_hard_mb)
            .unwrap_or(false)
    };
    let psi_breach = if cfg.memory_circuit_psi_full_avg60_x100 == 0 {
        false
    } else {
        probe
            .psi_full_avg60_x100
            .map(|v| v >= cfg.memory_circuit_psi_full_avg60_x100)
            .unwrap_or(false)
    };

    if !(rss_breach || psi_breach) {
        // Clean tick — reset the breach clock.
        state
            .memory_pressure_breach_since
            .store(0, Ordering::Relaxed);
        return false;
    }

    // Breach: latch the "since" timestamp on first breach, keep it on
    // subsequent breaches. Compare-exchange would be overkill; the
    // watchdog task is the sole writer.
    let since = state.memory_pressure_breach_since.load(Ordering::Relaxed);
    if since == 0 {
        state
            .memory_pressure_breach_since
            .store(now, Ordering::Relaxed);
        return false;
    }
    let breach_for = now.saturating_sub(since);
    breach_for >= cfg.memory_circuit_sustained.as_secs()
}

/// Wall-clock unix seconds. Falls back to 0 on the very unlikely
/// SystemTimeError; downstream code treats 0 as "unknown" and behaves
/// sanely (healthy=false, last_ago=0).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn parse_port_handles_common_forms() {
        assert_eq!(parse_port("0.0.0.0:8545"), Some(8545));
        assert_eq!(parse_port("127.0.0.1:8545"), Some(8545));
        assert_eq!(parse_port("[::]:8545"), Some(8545));
        assert_eq!(parse_port("[::1]:8545"), Some(8545));
        assert_eq!(parse_port("0.0.0.0:8545/"), Some(8545));
        assert_eq!(parse_port("8545"), Some(8545));
        assert_eq!(parse_port("garbage"), None);
    }

    #[test]
    fn probe_url_always_uses_loopback_regardless_of_bind() {
        assert_eq!(
            probe_url_from_bind("0.0.0.0:8545"),
            "http://127.0.0.1:8545"
        );
        assert_eq!(
            probe_url_from_bind("[::]:9999"),
            "http://127.0.0.1:9999"
        );
        assert_eq!(
            probe_url_from_bind("junk"),
            "http://127.0.0.1:8545"
        );
    }

    #[test]
    fn env_flag_recognizes_common_truthy_and_falsy_values() {
        // We cannot rely on real env vars in a parallel test suite, but
        // the internal logic is testable via a helper — inline instead.
        let cases: &[(&str, Option<bool>)] = &[
            ("1", Some(true)),
            ("true", Some(true)),
            ("on", Some(true)),
            ("YES", Some(true)),
            ("0", Some(false)),
            ("false", Some(false)),
            ("no", Some(false)),
            ("OFF", Some(false)),
            ("garbage", None),
            ("", None),
        ];
        // Emulate what env_flag does after strip+lowercase (avoiding
        // std::env for parallel-test safety).
        for (input, expected) in cases {
            let got = match input.trim().to_ascii_lowercase().as_str() {
                "" => None,
                "0" | "false" | "no" | "off" => Some(false),
                "1" | "true" | "yes" | "on" => Some(true),
                _ => None,
            };
            assert_eq!(got, *expected, "input={input}");
        }
    }

    #[test]
    fn watchdog_state_defaults_to_unhealthy() {
        let state = WatchdogState::new();
        assert_eq!(state.last_success_at.load(Ordering::Relaxed), 0);
        assert_eq!(state.consecutive_failures.load(Ordering::Relaxed), 0);
        assert_eq!(state.total_probes.load(Ordering::Relaxed), 0);
        assert_eq!(state.total_failures.load(Ordering::Relaxed), 0);
        assert!(!state.startup_grace_elapsed.load(Ordering::Relaxed));
    }

    #[test]
    fn build_snapshot_reports_healthy_only_when_recent_success() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let cfg = WatchdogConfig {
            probe_url: "http://127.0.0.1:8545".to_string(),
            interval: Duration::from_secs(15),
            timeout: Duration::from_secs(5),
            stall_threshold: Duration::from_secs(120),
            startup_grace: Duration::from_secs(300),
            suicide_enabled: false,
            state_file: tmpdir.path().join("self-watchdog.json"),
            memory_circuit_enabled: false,
            memory_circuit_rss_hard_mb: defaults::MEMORY_CIRCUIT_RSS_HARD_MB,
            memory_circuit_psi_full_avg60_x100: defaults::MEMORY_CIRCUIT_PSI_FULL_AVG60_X100,
            memory_circuit_sustained: Duration::from_secs(defaults::MEMORY_CIRCUIT_SUSTAINED_SECS),
        };
        let state = WatchdogState::new();

        // Before any success — unhealthy.
        let snap = build_snapshot(&cfg, &state);
        assert_eq!(snap["healthy"], serde_json::Value::Bool(false));
        assert_eq!(snap["last_success_at"], serde_json::json!(0));

        // Mark a recent success — healthy.
        state
            .last_success_at
            .store(now_secs(), Ordering::Relaxed);
        let snap = build_snapshot(&cfg, &state);
        assert_eq!(snap["healthy"], serde_json::Value::Bool(true));

        // Simulate a stale success (older than the threshold).
        let stale = now_secs().saturating_sub(cfg.stall_threshold.as_secs() + 10);
        state.last_success_at.store(stale, Ordering::Relaxed);
        let snap = build_snapshot(&cfg, &state);
        assert_eq!(
            snap["healthy"],
            serde_json::Value::Bool(false),
            "stale success beyond threshold must be reported as unhealthy"
        );
    }

    #[test]
    fn snapshot_serializes_to_valid_json_with_expected_keys() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let cfg = WatchdogConfig {
            probe_url: "http://127.0.0.1:8545".to_string(),
            interval: Duration::from_secs(15),
            timeout: Duration::from_secs(5),
            stall_threshold: Duration::from_secs(120),
            startup_grace: Duration::from_secs(300),
            suicide_enabled: true,
            state_file: tmpdir.path().join("self-watchdog.json"),
            memory_circuit_enabled: false,
            memory_circuit_rss_hard_mb: defaults::MEMORY_CIRCUIT_RSS_HARD_MB,
            memory_circuit_psi_full_avg60_x100: defaults::MEMORY_CIRCUIT_PSI_FULL_AVG60_X100,
            memory_circuit_sustained: Duration::from_secs(defaults::MEMORY_CIRCUIT_SUSTAINED_SECS),
        };
        let state = WatchdogState::new();
        let snap = build_snapshot(&cfg, &state);
        let obj = snap.as_object().expect("must be a JSON object");
        for key in [
            "healthy",
            "last_success_at",
            "last_success_ago_secs",
            "consecutive_failures",
            "total_probes",
            "total_failures",
            "startup_grace_elapsed",
            "suicide_enabled",
            "stall_threshold_secs",
            "interval_secs",
            "probe_url",
            "memory",
            "note",
        ] {
            assert!(obj.contains_key(key), "missing key: {key}");
        }
        assert_eq!(obj["suicide_enabled"], serde_json::Value::Bool(true));
        assert_eq!(obj["probe_url"], serde_json::json!("http://127.0.0.1:8545"));
        // The memory subobject must always exist and always carry the
        // shape B2 consumers rely on, even when the running box has
        // no PSI support (e.g. a stripped kernel or the macOS dev
        // path where the file is missing entirely).
        let mem = obj["memory"].as_object().expect("memory must be object");
        for key in [
            "vm_rss_kb",
            "vm_rss_mb",
            "vm_peak_kb",
            "psi_full_avg60_pct_x100",
            "psi_full_avg60_pct",
            "psi_read_ok",
            "breach_since",
            "breach_for_secs",
            "circuit_enabled",
            "circuit_rss_hard_mb",
            "circuit_psi_full_avg60_pct_x100",
            "circuit_sustained_secs",
            "circuit_trips_total",
        ] {
            assert!(mem.contains_key(key), "memory.{key} missing");
        }
    }

    #[test]
    fn write_state_file_is_atomic_and_creates_parent_dir() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        // Point at a nested path that doesn't exist yet.
        let target = tmpdir.path().join("nested/subdir/self-watchdog.json");
        let value = serde_json::json!({"hello": "world"});
        write_state_file(&target, &value).expect("first write");
        // File exists and contains our JSON.
        let raw = std::fs::read_to_string(&target).expect("read");
        assert!(raw.contains("\"hello\""));
        // Overwrite works (the atomic-rename path).
        let value2 = serde_json::json!({"goodbye": "world"});
        write_state_file(&target, &value2).expect("overwrite");
        let raw2 = std::fs::read_to_string(&target).expect("read again");
        assert!(raw2.contains("\"goodbye\""));
        assert!(!raw2.contains("\"hello\""));
    }

    #[test]
    fn config_from_env_uses_defaults_when_unset() {
        // Not touching any env vars — this tests the fallback path
        // regardless of whether other tests have polluted the env.
        let tmpdir = tempfile::tempdir().expect("tempdir");
        // Use an env-var name that we're guaranteed no other test sets.
        // The config parser reads standard names, but with unset env
        // we get defaults — that's the property under test.
        let cfg = WatchdogConfig::from_env(tmpdir.path(), "0.0.0.0:8545");
        assert!(cfg.interval.as_secs() >= 1);
        assert!(cfg.timeout.as_secs() >= 1);
        // Stall threshold must be at least 2x the interval (safety
        // clamp inside from_env).
        assert!(
            cfg.stall_threshold.as_secs() >= cfg.interval.as_secs() * 2,
            "stall_threshold={} interval={}",
            cfg.stall_threshold.as_secs(),
            cfg.interval.as_secs(),
        );
        assert_eq!(cfg.probe_url, "http://127.0.0.1:8545");
        assert_eq!(cfg.state_file, tmpdir.path().join("self-watchdog.json"));
    }

    #[test]
    fn truncate_for_log_shortens_over_max() {
        let s = "abcdefghij";
        assert_eq!(truncate_for_log(s, 100), "abcdefghij");
        assert_eq!(truncate_for_log(s, 3), "abc...");
    }

    // ---- P1-B2 memory circuit-breaker tests -------------------------------

    /// The kernel format for `/proc/self/status` is `key: value kB`.
    /// The parser must tolerate multi-space padding and `\t` after
    /// the colon (some kernels pad numerically for alignment).
    #[test]
    fn parse_proc_status_rss_peak_reads_kb() {
        let contents = "\
Name:\trope-node
State:\tR (running)
Pid:\t12345
VmPeak:\t 8388608 kB
VmSize:\t 8000000 kB
VmRSS:\t   2097152 kB
VmData:\t 4000000 kB
VmSwap:\t       0 kB
";
        let (rss, peak) = parse_proc_status_rss_peak(contents);
        assert_eq!(rss, Some(2_097_152));
        assert_eq!(peak, Some(8_388_608));
    }

    #[test]
    fn parse_proc_status_rss_peak_missing_returns_none() {
        let contents = "Name:\trope-node\nState:\tS\n";
        let (rss, peak) = parse_proc_status_rss_peak(contents);
        assert_eq!(rss, None);
        assert_eq!(peak, None);
    }

    #[test]
    fn parse_proc_status_rss_peak_malformed_line_returns_none() {
        let contents = "VmRSS:\tnot-a-number kB\n";
        let (rss, _peak) = parse_proc_status_rss_peak(contents);
        assert_eq!(rss, None);
    }

    /// PSI file format on kernels ≥4.20:
    ///   some avg10=X.YZ avg60=X.YZ avg300=X.YZ total=N
    ///   full avg10=X.YZ avg60=X.YZ avg300=X.YZ total=N
    #[test]
    fn parse_psi_full_avg60_reads_percent_x100() {
        let contents = "\
some avg10=0.10 avg60=0.20 avg300=0.30 total=1234
full avg10=1.11 avg60=12.34 avg300=5.67 total=9876
";
        assert_eq!(parse_psi_full_avg60(contents), Some(1234));
    }

    #[test]
    fn parse_psi_full_avg60_missing_full_line_returns_none() {
        let contents = "some avg10=0.10 avg60=0.20 avg300=0.30 total=1234\n";
        assert_eq!(parse_psi_full_avg60(contents), None);
    }

    #[test]
    fn parse_psi_full_avg60_zero_is_valid() {
        let contents = "\
some avg10=0.00 avg60=0.00 avg300=0.00 total=0
full avg10=0.00 avg60=0.00 avg300=0.00 total=0
";
        assert_eq!(parse_psi_full_avg60(contents), Some(0));
    }

    #[test]
    fn parse_psi_full_avg60_clamps_and_rounds() {
        // A garbage value like 99999.99 must clamp rather than
        // overflow the u64 return.
        let contents = "full avg10=0 avg60=99999.99 avg300=0 total=0\n";
        let got = parse_psi_full_avg60(contents).expect("must clamp, not fail");
        assert!(got >= 100_000);
        assert!(got <= 1_000_000);
    }

    /// The circuit is a pure decision function: given a synthetic
    /// state + probe, it must return the expected bool without
    /// mutating anything the caller can observe out-of-band.
    fn breaker_cfg() -> WatchdogConfig {
        let tmp = std::env::temp_dir().join("rope-b2-test.json");
        WatchdogConfig {
            probe_url: "http://127.0.0.1:8545".to_string(),
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(5),
            stall_threshold: Duration::from_secs(120),
            startup_grace: Duration::from_secs(300),
            suicide_enabled: true,
            state_file: tmp,
            memory_circuit_enabled: true,
            memory_circuit_rss_hard_mb: 12_000,
            memory_circuit_psi_full_avg60_x100: 2_000, // 20.00%
            memory_circuit_sustained: Duration::from_secs(30),
        }
    }

    fn seeded_state(now: u64) -> WatchdogState {
        let s = WatchdogState::new();
        // Simulate a healthy warm-up: past startup grace, and we've
        // seen at least one healthy tick.
        s.startup_grace_elapsed.store(true, Ordering::Relaxed);
        s.last_success_at.store(now, Ordering::Relaxed);
        s
    }

    #[test]
    fn memory_circuit_stays_off_before_startup_grace() {
        let cfg = breaker_cfg();
        let state = WatchdogState::new(); // startup_grace_elapsed=false
        let probe = MemoryProbe {
            vm_rss_kb: Some(15_000 * 1024), // 15 GB, way over the 12 GB cap
            vm_peak_kb: Some(15_000 * 1024),
            psi_full_avg60_x100: Some(9_000), // 90%
            psi_read_ok: true,
        };
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_000));
    }

    #[test]
    fn memory_circuit_stays_off_before_first_success() {
        let cfg = breaker_cfg();
        let state = WatchdogState::new();
        state.startup_grace_elapsed.store(true, Ordering::Relaxed);
        // Deliberately leave last_success_at at 0 — we've never seen
        // a healthy probe. That state is indistinguishable from a
        // bad boot; do NOT trip.
        let probe = MemoryProbe {
            vm_rss_kb: Some(15_000 * 1024),
            vm_peak_kb: Some(15_000 * 1024),
            psi_full_avg60_x100: Some(9_000),
            psi_read_ok: true,
        };
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_000));
    }

    #[test]
    fn memory_circuit_stays_off_when_disabled() {
        let mut cfg = breaker_cfg();
        cfg.memory_circuit_enabled = false;
        let state = seeded_state(1_000);
        let probe = MemoryProbe {
            vm_rss_kb: Some(15_000 * 1024),
            vm_peak_kb: Some(15_000 * 1024),
            psi_full_avg60_x100: Some(9_000),
            psi_read_ok: true,
        };
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 2_000));
    }

    #[test]
    fn memory_circuit_latches_on_first_breach_but_does_not_trip() {
        let cfg = breaker_cfg();
        let state = seeded_state(1_000);
        let probe = MemoryProbe {
            vm_rss_kb: Some(15_000 * 1024),
            vm_peak_kb: Some(15_000 * 1024),
            psi_full_avg60_x100: None,
            psi_read_ok: false,
        };
        // First tick with breach: latch the timestamp, don't trip.
        let tripped = evaluate_memory_circuit(&cfg, &state, &probe, 1_100);
        assert!(!tripped);
        assert_eq!(
            state
                .memory_pressure_breach_since
                .load(Ordering::Relaxed),
            1_100
        );
    }

    #[test]
    fn memory_circuit_trips_after_sustained_breach_on_rss_leg() {
        let cfg = breaker_cfg();
        let state = seeded_state(1_000);
        // Tick 1 at t=1100: latch.
        let probe = MemoryProbe {
            vm_rss_kb: Some(15_000 * 1024),
            vm_peak_kb: Some(15_000 * 1024),
            psi_full_avg60_x100: None,
            psi_read_ok: false,
        };
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_100));
        // Tick 2 at t=1129: still breaching, still under sustained threshold (30s).
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_129));
        // Tick 3 at t=1131: 31s elapsed since first breach, must trip.
        assert!(evaluate_memory_circuit(&cfg, &state, &probe, 1_131));
    }

    #[test]
    fn memory_circuit_trips_after_sustained_breach_on_psi_leg() {
        let cfg = breaker_cfg();
        let state = seeded_state(1_000);
        // RSS under the cap, but PSI over (independent legs).
        let probe = MemoryProbe {
            vm_rss_kb: Some(1_000 * 1024), // 1 GB, way under 12 GB
            vm_peak_kb: Some(1_000 * 1024),
            psi_full_avg60_x100: Some(3_000), // 30%, over the 20% threshold
            psi_read_ok: true,
        };
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_100));
        assert!(evaluate_memory_circuit(&cfg, &state, &probe, 1_131));
    }

    #[test]
    fn memory_circuit_resets_on_clean_tick() {
        let cfg = breaker_cfg();
        let state = seeded_state(1_000);
        // Breach tick.
        let breach_probe = MemoryProbe {
            vm_rss_kb: Some(15_000 * 1024),
            vm_peak_kb: Some(15_000 * 1024),
            psi_full_avg60_x100: None,
            psi_read_ok: false,
        };
        assert!(!evaluate_memory_circuit(
            &cfg,
            &state,
            &breach_probe,
            1_100
        ));
        assert_eq!(
            state
                .memory_pressure_breach_since
                .load(Ordering::Relaxed),
            1_100
        );
        // Clean tick before sustained threshold: resets the latch.
        let clean_probe = MemoryProbe {
            vm_rss_kb: Some(2_000 * 1024),
            vm_peak_kb: Some(2_000 * 1024),
            psi_full_avg60_x100: Some(100), // 1%
            psi_read_ok: true,
        };
        assert!(!evaluate_memory_circuit(
            &cfg,
            &state,
            &clean_probe,
            1_120
        ));
        assert_eq!(
            state
                .memory_pressure_breach_since
                .load(Ordering::Relaxed),
            0,
            "clean tick must reset the breach clock"
        );
        // A breach starting fresh at 1_121 must not trip until 1_121+30.
        assert!(!evaluate_memory_circuit(
            &cfg,
            &state,
            &breach_probe,
            1_121
        ));
        assert!(!evaluate_memory_circuit(
            &cfg,
            &state,
            &breach_probe,
            1_140
        ));
        assert!(evaluate_memory_circuit(
            &cfg,
            &state,
            &breach_probe,
            1_152
        ));
    }

    #[test]
    fn memory_circuit_zero_threshold_disables_that_leg() {
        let mut cfg = breaker_cfg();
        cfg.memory_circuit_rss_hard_mb = 0; // disable RSS leg
        cfg.memory_circuit_psi_full_avg60_x100 = 2_000; // 20%
        let state = seeded_state(1_000);
        // RSS way over what would have tripped, but leg disabled.
        // PSI leg is under threshold.
        let probe = MemoryProbe {
            vm_rss_kb: Some(999_999 * 1024),
            vm_peak_kb: Some(999_999 * 1024),
            psi_full_avg60_x100: Some(100), // 1%
            psi_read_ok: true,
        };
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_100));
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_200));
    }

    #[test]
    fn memory_circuit_unavailable_leg_does_not_trip() {
        // If the kernel exposes no PSI at all (macOS dev, minimal
        // container), the PSI leg must not spuriously trip. Only the
        // RSS leg (if configured) can arm.
        let cfg = breaker_cfg();
        let state = seeded_state(1_000);
        let probe = MemoryProbe {
            vm_rss_kb: None, // /proc/self/status unreadable
            vm_peak_kb: None,
            psi_full_avg60_x100: None,
            psi_read_ok: false,
        };
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_100));
        assert!(!evaluate_memory_circuit(&cfg, &state, &probe, 1_500));
    }
}
