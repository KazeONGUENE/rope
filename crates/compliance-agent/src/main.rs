// =============================================================================
// compliance-agent — binary entrypoint
// =============================================================================
//
// Subcommands:
//
//   serve   — run the long-running ComplianceAgent service. Wires up:
//               * HTTP listener (axum) on `--listen`
//               * RPC client to `--rpc-url` (rope-node)
//               * Periodic reporter (default 15 min cadence)
//
//   show-config — print the resolved configuration as JSON. Helpful in
//               container builds where you want to verify env-var
//               wiring before bringing the server up.
//
// Examples:
//
//   $ compliance-agent serve --listen 0.0.0.0:9091 \
//                            --rpc-url http://127.0.0.1:8545 \
//                            --key-path /etc/datachain/compliance-agent.key
//
//   $ compliance-agent show-config
// =============================================================================

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::Notify;

use rope_compliance_agent::anchor::AnchorClient;
use rope_compliance_agent::config::{
    ComplianceAgentConfig, GdprPolicy, CANONICAL_COMPLIANCE_AGENT_WALLET, DEFAULT_LISTEN_ADDR,
    DEFAULT_MAX_DIGEST_EVENTS, DEFAULT_REPORTING_INTERVAL_SECS, DEFAULT_RPC_URL,
};
use rope_compliance_agent::gdpr::Article17Validator;
use rope_compliance_agent::metrics::ComplianceMetrics;
use rope_compliance_agent::orchestrator::UntieOrchestrator;
use rope_compliance_agent::reporting::PeriodicReporter;
use rope_compliance_agent::rpc::{HttpRopeRpcClient, RopeRpcClient};
use rope_compliance_agent::server::{build_router, ServerState};

#[derive(Parser, Debug)]
#[command(
    name = "compliance-agent",
    version,
    about = "Datachain Rope canonical ComplianceAgent — GDPR Art. 17 + MiFID II / DORA testimony"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Serve(ServeArgs),
    ShowConfig(ServeArgs),
}

#[derive(Parser, Debug)]
struct ServeArgs {
    /// HTTP listen address (host:port).
    #[arg(long, default_value = DEFAULT_LISTEN_ADDR, env = "COMPLIANCE_AGENT_LISTEN")]
    listen: String,

    /// JSON-RPC endpoint of the local rope-node.
    #[arg(long, default_value = DEFAULT_RPC_URL, env = "COMPLIANCE_AGENT_RPC_URL")]
    rpc_url: String,

    /// Optional path to a key file used to sign outbound testimonies.
    #[arg(long, env = "COMPLIANCE_AGENT_KEY_PATH")]
    key_path: Option<PathBuf>,

    /// Wallet whose string the agent anchors testimonies on.
    #[arg(long, default_value = CANONICAL_COMPLIANCE_AGENT_WALLET, env = "COMPLIANCE_AGENT_WALLET")]
    agent_wallet: String,

    /// Reporting cadence in seconds.
    #[arg(
        long,
        default_value_t = DEFAULT_REPORTING_INTERVAL_SECS,
        env = "COMPLIANCE_AGENT_REPORTING_INTERVAL_SECS"
    )]
    reporting_interval_secs: u64,

    /// Maximum events per digest batch.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_DIGEST_EVENTS,
        env = "COMPLIANCE_AGENT_MAX_DIGEST_EVENTS"
    )]
    max_digest_events: usize,

    /// Comma-separated ISO-3166 numeric country codes the agent will
    /// process. Empty = accept any jurisdiction.
    #[arg(
        long,
        default_value = "",
        env = "COMPLIANCE_AGENT_ALLOWED_JURISDICTIONS"
    )]
    allowed_jurisdictions: String,

    /// Whether to require a non-empty `requestor_proof` on every Art. 17
    /// request.
    #[arg(long, default_value_t = true, env = "COMPLIANCE_AGENT_REQUIRE_PROOF")]
    require_requestor_proof: bool,
}

impl ServeArgs {
    fn into_config(self) -> ComplianceAgentConfig {
        let mut allowed = BTreeSet::new();
        for tok in self.allowed_jurisdictions.split(',') {
            let trimmed = tok.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(c) = trimmed.parse::<u16>() {
                allowed.insert(c);
            } else {
                tracing::warn!(
                    target: "compliance::config",
                    code = trimmed,
                    "ignoring non-numeric jurisdiction code"
                );
            }
        }
        let gdpr = GdprPolicy {
            allowed_jurisdictions: allowed,
            require_requestor_proof: self.require_requestor_proof,
            ..GdprPolicy::default()
        };
        ComplianceAgentConfig {
            listen_addr: self.listen,
            rpc_url: self.rpc_url,
            agent_wallet: self.agent_wallet,
            key_path: self.key_path,
            gdpr,
            reporting_interval: Duration::from_secs(self.reporting_interval_secs),
            max_digest_events: self.max_digest_events,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing();

    match cli.command {
        Command::ShowConfig(args) => {
            let cfg = args.into_config();
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(())
        }
        Command::Serve(args) => serve(args.into_config()).await,
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,compliance=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}

async fn serve(cfg: ComplianceAgentConfig) -> anyhow::Result<()> {
    tracing::info!(
        target: "compliance::main",
        listen = %cfg.listen_addr,
        rpc_url = %cfg.rpc_url,
        agent_wallet = %cfg.agent_wallet,
        reporting_interval_secs = cfg.reporting_interval.as_secs(),
        max_digest_events = cfg.max_digest_events,
        require_requestor_proof = cfg.gdpr.require_requestor_proof,
        allowed_jurisdiction_count = cfg.gdpr.allowed_jurisdictions.len(),
        "starting ComplianceAgent"
    );

    if let Some(path) = &cfg.key_path {
        if path.exists() {
            tracing::info!(
                target: "compliance::main",
                key_path = %path.display(),
                "key file present (PHASE 1: signing not yet wired — file is logged only)"
            );
        } else {
            tracing::warn!(
                target: "compliance::main",
                key_path = %path.display(),
                "key file does not exist; testimonies will be unsigned"
            );
        }
    }

    let rpc: Arc<dyn RopeRpcClient> = Arc::new(HttpRopeRpcClient::new(cfg.rpc_url.clone()));
    let validator = Arc::new(Article17Validator::new(cfg.gdpr.clone()));
    let orchestrator = Arc::new(UntieOrchestrator::new(rpc.clone()));
    let anchor = AnchorClient::new(rpc.clone(), cfg.agent_wallet.clone());
    let reporter = PeriodicReporter::new(
        anchor.clone(),
        cfg.reporting_interval,
        cfg.max_digest_events,
    );
    let metrics = Arc::new(ComplianceMetrics::new());

    let state = ServerState {
        validator,
        orchestrator,
        anchor,
        reporter: reporter.clone(),
        metrics,
        agent_wallet: cfg.agent_wallet.clone(),
        agent_id: "compliance".to_string(),
    };

    let cancel = Arc::new(Notify::new());
    let cancel_for_reporter = cancel.clone();
    let reporter_task = tokio::spawn(async move {
        reporter.run(cancel_for_reporter).await;
    });

    let app = build_router(state);
    let listener = TcpListener::bind(&cfg.listen_addr).await?;
    tracing::info!(
        target: "compliance::main",
        listen = %cfg.listen_addr,
        "HTTP listener bound"
    );

    let cancel_for_axum = cancel.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tracing::info!(target: "compliance::main", "shutdown signal received");
            cancel_for_axum.notify_waiters();
        })
        .await?;

    let _ = reporter_task.await;
    tracing::info!(target: "compliance::main", "ComplianceAgent stopped cleanly");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install ctrl_c handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
