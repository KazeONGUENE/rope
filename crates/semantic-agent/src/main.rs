//! `semantic-agent` binary — runs the SemanticAgent as a long-lived
//! service.
//!
//! Subcommands:
//!
//! - `serve`    — start the indexer + checkpoint loops + HTTP server
//! - `index`    — run a single indexer pass and exit (useful for cron)
//! - `checkpoint` — build (and optionally anchor) one checkpoint and exit

use clap::{Parser, Subcommand};
use semantic_agent::config::{AgentConfig, AgentIdentity};
use semantic_agent::{server, SemanticAgent};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser, Debug)]
#[command(
    name = "semantic-agent",
    version,
    about = "Datachain Rope SemanticAgent — knot indexing + semantic search + auditable checkpoints"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the full agent: indexer loop + checkpoint loop + HTTP server.
    Serve(ServeArgs),
    /// Run one indexer pass and exit (no HTTP server, no anchor).
    Index(SharedArgs),
    /// Build one checkpoint. With `--anchor`, also submits it via
    /// `rope_appendToLedger`. Without, prints the merkle root + body
    /// to stdout.
    Checkpoint(CheckpointArgs),
}

#[derive(Parser, Debug)]
struct SharedArgs {
    #[arg(long, env = "RPC_URL", default_value = "http://127.0.0.1:8545")]
    rpc_url: String,
    #[arg(long, default_value_t = 10)]
    rpc_timeout_secs: u64,
    #[arg(long, env = "INDEX_PATH", default_value = "./semantic-agent-index")]
    index_path: PathBuf,
    #[arg(long, env = "AGENT_WALLET", default_value = semantic_agent::CANONICAL_AGENT_WALLET)]
    wallet: String,
    #[arg(long, env = "AGENT_ID", default_value = semantic_agent::CANONICAL_AGENT_ID)]
    agent_id: String,
    #[arg(long, default_value_t = 200)]
    list_strings_limit: u32,
    #[arg(long, default_value_t = 5_000)]
    max_knots_per_poll: usize,
}

#[derive(Parser, Debug)]
struct ServeArgs {
    #[command(flatten)]
    shared: SharedArgs,
    #[arg(long, default_value = "0.0.0.0:9092")]
    listen: String,
    #[arg(long, default_value_t = 30)]
    poll_interval_secs: u64,
    #[arg(long, default_value_t = 600)]
    checkpoint_interval_secs: u64,
    #[arg(long)]
    read_only: bool,
}

#[derive(Parser, Debug)]
struct CheckpointArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// When set, also submit the checkpoint via `rope_appendToLedger`.
    #[arg(long)]
    anchor: bool,
}

fn install_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,semantic_agent=debug"));
    fmt().with_env_filter(filter).with_target(false).init();
}

fn build_config(
    shared: &SharedArgs,
    listen: &str,
    poll: u64,
    checkpoint: u64,
    read_only: bool,
) -> AgentConfig {
    AgentConfig {
        rpc_url: shared.rpc_url.clone(),
        rpc_timeout: Duration::from_secs(shared.rpc_timeout_secs),
        index_path: shared.index_path.clone(),
        listen_addr: listen.to_string(),
        poll_interval: Duration::from_secs(poll),
        checkpoint_interval: Duration::from_secs(checkpoint),
        list_strings_limit: shared.list_strings_limit,
        max_knots_per_poll: shared.max_knots_per_poll,
        identity: AgentIdentity {
            agent_id: shared.agent_id.clone(),
            wallet: shared.wallet.clone(),
        },
        read_only,
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    install_tracing();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve(args) => {
            let cfg = build_config(
                &args.shared,
                &args.listen,
                args.poll_interval_secs,
                args.checkpoint_interval_secs,
                args.read_only,
            );
            let agent = Arc::new(SemanticAgent::new(cfg)?);
            tracing::info!(
                rpc = %agent.config.rpc_url,
                index = ?agent.config.index_path,
                listen = %agent.config.listen_addr,
                "SemanticAgent starting"
            );
            let _indexer_handle = agent.indexer.clone().spawn_poll_loop();
            let _checkpoint_handle = agent.anchor.clone().spawn_checkpoint_loop();
            server::serve(agent).await?;
            Ok(())
        }
        Cmd::Index(shared) => {
            let cfg = build_config(&shared, "127.0.0.1:0", 30, 600, true);
            let agent = SemanticAgent::new(cfg)?;
            let outcome = agent.indexer.poll_once().await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "strings_walked": outcome.strings_walked,
                    "knots_indexed": outcome.knots_indexed,
                    "knots_skipped": outcome.knots_skipped,
                    "doc_count": agent.search.doc_count(),
                }))?
            );
            Ok(())
        }
        Cmd::Checkpoint(args) => {
            let cfg = build_config(
                &args.shared,
                "127.0.0.1:0",
                30,
                600,
                !args.anchor, // read_only iff --anchor not set
            );
            let agent = SemanticAgent::new(cfg)?;
            let last = agent.metrics.read().last_indexed_string_id.clone();
            let result = agent.anchor.build_and_anchor(last).await?;
            match result {
                Some(outcome) => println!("{}", serde_json::to_string_pretty(&outcome)?),
                None => {
                    let last = agent.metrics.read().last_indexed_string_id.clone();
                    let builder = semantic_agent::CheckpointBuilder::new(
                        agent.config.clone(),
                        agent.search.clone(),
                    );
                    let (testimony, _root) = builder.build(last)?;
                    println!("{}", serde_json::to_string_pretty(&testimony)?);
                }
            }
            Ok(())
        }
    }
}
