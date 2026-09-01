//! `rope-ecosystem-discovery` binary - long-running daemon that
//! periodically walks every enabled scanner and rewrites the
//! ecosystem-overlay JSONL file consumed by `rope-explorer`.
//!
//! Modes:
//!
//! - Default: read config, then loop forever - each iteration runs
//!   `lib::run_once` and then sleeps `run_interval_secs`. SIGINT and
//!   SIGTERM flip a shared stop flag; the daemon finishes the current
//!   pass (if any) and exits with code 0.
//! - `--once`: read config, run exactly one pass, exit 0 on success or
//!   1 on any scanner-independent fatal error (config, writer,
//!   permissions).
//! - `--dry-run`: parse config + init tracing + print the resolved
//!   settings, then exit 0 without touching the network or the fs.
//!
//! The binary NEVER panics on a scanner error - `run_once` already
//! swallows per-scanner failures into warnings so a broken partner API
//! cannot take the daemon down. The only errors that propagate here
//! are configuration / IO / writer errors that indicate the daemon
//! cannot make forward progress at all.
//!
//! Systemd deploy: pair this binary with a `Type=simple`,
//! `Restart=always`, `RestartSec=10s` unit. The daemon's built-in loop
//! removes the need for a companion `.timer`.

use anyhow::{Context, Result};
use clap::Parser;
use rope_ecosystem_discovery::{
    run_once, DiscoveryConfig, DEFAULT_CONFIG_PATH,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tracing::{error, info, warn};

#[derive(Debug, Parser)]
#[command(
    name = "rope-ecosystem-discovery",
    about = "Autonomous ecosystem discovery daemon for Datachain Rope",
    long_about = "\
Runs the ecosystem discovery scanners (handover / on-chain / \
partner-api) on a schedule and writes their merged output to a JSONL \
overlay file that rope-explorer consumes. Deploy as a systemd \
Type=simple unit; the daemon's internal loop replaces a companion \
timer.",
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    /// Run exactly one discovery pass, then exit. Useful for
    /// ad-hoc invocation, systemd `.timer` deployments, and tests.
    #[arg(long, default_value_t = false)]
    once: bool,

    /// Load + validate the config, log the resolved settings, then
    /// exit without touching the network or the fs. Zero side effects.
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Log level filter, forwarded to `tracing_subscriber`. Accepts
    /// standard values like `error`, `warn`, `info`, `debug`, `trace`.
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.log_level);

    info!(
        target: "rope_ecosystem_discovery",
        config = %cli.config.display(),
        once = cli.once,
        dry_run = cli.dry_run,
        "starting rope-ecosystem-discovery daemon"
    );

    let cfg = DiscoveryConfig::from_file(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    log_resolved_config(&cfg);

    if cli.dry_run {
        info!(target: "rope_ecosystem_discovery", "dry-run mode: exiting before any scan");
        return Ok(());
    }

    if cli.once {
        return run_single_pass(&cfg).await;
    }

    let stop = Arc::new(AtomicBool::new(false));
    spawn_signal_handler(stop.clone());
    run_forever(cfg, stop).await
}

/// Run one discovery pass. Returns `Err` only if `run_once` itself
/// fails (writer / config errors); per-scanner failures are already
/// logged and swallowed inside `run_once`.
async fn run_single_pass(cfg: &DiscoveryConfig) -> Result<()> {
    match run_once(cfg).await {
        Ok(summary) => {
            info!(
                target: "rope_ecosystem_discovery",
                input = summary.input_count,
                written = summary.written_count,
                deduped = summary.deduped_count,
                bytes = summary.bytes_written,
                "single-pass discovery complete"
            );
            Ok(())
        }
        Err(e) => {
            error!(
                target: "rope_ecosystem_discovery",
                error = %e,
                "single-pass discovery failed"
            );
            Err(anyhow::anyhow!(e))
        }
    }
}

/// Long-running loop. Each iteration runs one discovery pass, then
/// sleeps `run_interval_secs`. A failing pass is logged and skipped -
/// the loop keeps going. SIGINT / SIGTERM flip `stop` and the loop
/// exits cleanly after the current pass finishes (or immediately if
/// caught during sleep).
async fn run_forever(cfg: DiscoveryConfig, stop: Arc<AtomicBool>) -> Result<()> {
    let interval = cfg.run_interval();
    info!(
        target: "rope_ecosystem_discovery",
        interval_secs = interval.as_secs(),
        "entering daemon loop"
    );

    let mut iteration: u64 = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            info!(
                target: "rope_ecosystem_discovery",
                iterations = iteration,
                "stop flag set before iteration - exiting cleanly"
            );
            return Ok(());
        }

        iteration = iteration.saturating_add(1);
        let started = Instant::now();

        info!(
            target: "rope_ecosystem_discovery",
            iteration,
            "beginning discovery pass"
        );

        match run_once(&cfg).await {
            Ok(summary) => info!(
                target: "rope_ecosystem_discovery",
                iteration,
                elapsed_ms = started.elapsed().as_millis() as u64,
                input = summary.input_count,
                written = summary.written_count,
                deduped = summary.deduped_count,
                bytes = summary.bytes_written,
                "discovery pass complete"
            ),
            Err(e) => warn!(
                target: "rope_ecosystem_discovery",
                iteration,
                elapsed_ms = started.elapsed().as_millis() as u64,
                error = %e,
                "discovery pass failed - will retry after interval"
            ),
        }

        if stop.load(Ordering::Relaxed) {
            info!(
                target: "rope_ecosystem_discovery",
                iterations = iteration,
                "stop flag set after iteration - exiting cleanly"
            );
            return Ok(());
        }

        interruptible_sleep(interval, stop.clone()).await;
    }
}

/// Sleep in short chunks so a SIGTERM during the interval wakes us
/// within a second rather than waiting the full `run_interval_secs`.
async fn interruptible_sleep(interval: Duration, stop: Arc<AtomicBool>) {
    const CHUNK: Duration = Duration::from_secs(1);
    let deadline = Instant::now() + interval;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let step = if remaining < CHUNK { remaining } else { CHUNK };
        if step.is_zero() {
            break;
        }
        sleep(step).await;
    }
}

fn spawn_signal_handler(stop: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let ctrl_c = async {
            if let Err(e) = tokio::signal::ctrl_c().await {
                warn!(
                    target: "rope_ecosystem_discovery::signals",
                    error = %e,
                    "SIGINT handler install failed"
                );
                std::future::pending::<()>().await;
            } else {
                info!(target: "rope_ecosystem_discovery::signals", "received SIGINT");
            }
        };

        #[cfg(unix)]
        let sigterm = async {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut s) => {
                    let _ = s.recv().await;
                    info!(target: "rope_ecosystem_discovery::signals", "received SIGTERM");
                }
                Err(e) => {
                    warn!(
                        target: "rope_ecosystem_discovery::signals",
                        error = %e,
                        "SIGTERM handler install failed"
                    );
                    std::future::pending::<()>().await;
                }
            }
        };
        #[cfg(not(unix))]
        let sigterm = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm => {}
        }

        stop.store(true, Ordering::Relaxed);
        info!(
            target: "rope_ecosystem_discovery::signals",
            "stop flag set; daemon will exit after current pass"
        );
    });
}

fn init_tracing(level: &str) {
    use tracing_subscriber::{fmt, EnvFilter};

    // Env-var override wins if set: RUST_LOG=debug rope-ecosystem-discovery ...
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    let subscriber = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_level(true)
        .with_ansi(false)
        .compact();

    // If the subscriber is already installed (test harness, embedded
    // use), keep going rather than aborting.
    let _ = subscriber.try_init();
}

fn log_resolved_config(cfg: &DiscoveryConfig) {
    info!(
        target: "rope_ecosystem_discovery::config",
        output_path = %cfg.output_path.display(),
        run_interval_secs = cfg.run_interval_secs,
        http_timeout_secs = cfg.http_timeout_secs,
        handover_enabled = cfg.handover.enabled,
        handover_roots = cfg.handover.roots.len(),
        onchain_enabled = cfg.onchain.enabled,
        onchain_dcscan_base = cfg.onchain.dcscan_base.as_deref().unwrap_or("<default>"),
        partner_api_enabled = cfg.partner_api.enabled,
        partner_api_allowed_hosts = cfg.partner_api.allowed_hosts.len(),
        partner_api_endpoints = cfg.partner_api.endpoints.len(),
        "resolved configuration"
    );
}
