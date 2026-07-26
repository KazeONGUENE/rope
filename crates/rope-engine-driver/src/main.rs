mod attester;
mod committee;
mod engine_client;
mod follower;
mod identity;
mod payload;
mod production;
mod proposer;
mod quorum_proto;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use engine_client::EngineClient;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "rope-engine-driver")]
#[command(about = "Engine-API driver that replaces Reth's --dev auto-miner with \
real multi-node Testimony-style quorum: every EVM block is proposed by one \
node and independently re-executed + signed by every committee member \
before any node (proposer included) finalizes it.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the attester HTTP service. One instance per committee node —
    /// including the proposer's own node. Never self-mines; only ever
    /// signs a block after independently re-executing it locally.
    Attester {
        #[arg(long, env = "LOCAL_ENGINE_URL", default_value = "http://127.0.0.1:8552")]
        local_engine_url: String,
        #[arg(long, env = "LOCAL_RPC_URL", default_value = "http://127.0.0.1:8595")]
        local_rpc_url: String,
        #[arg(long, env = "JWT_SECRET_PATH", default_value = "/opt/datachain-rope/reth/data/jwt.hex")]
        jwt_secret_path: String,
        #[arg(long, env = "VALIDATOR_KEY_PATH", default_value = "/home/ubuntu/.rope/validator_key.bin")]
        validator_key_path: String,
        #[arg(long, env = "COMMITTEE_PATH", default_value = "/opt/datachain-rope/config/evm-quorum-committee.json")]
        committee_path: String,
        #[arg(long, env = "ATTESTER_BIND", default_value = "0.0.0.0:9600")]
        bind: String,
        /// Plain RPC endpoint of a trusted node (the proposer, by
        /// convention) this attester backfills from if it discovers it's
        /// behind (e.g. after a restart). Omit to disable catch-up — a
        /// node that falls behind with no catch-up source configured
        /// will return SYNCING/INVALID until manually resynced.
        #[arg(long, env = "CATCH_UP_RPC_URL")]
        catch_up_rpc_url: Option<String>,
    },
    /// Run the proposer. Exactly one instance across the whole committee
    /// (BLUE, by roster convention) should run this at any time.
    Proposer {
        #[arg(long, env = "LOCAL_ENGINE_URL", default_value = "http://127.0.0.1:8552")]
        local_engine_url: String,
        #[arg(long, env = "LOCAL_RPC_URL", default_value = "http://127.0.0.1:8595")]
        local_rpc_url: String,
        #[arg(long, env = "JWT_SECRET_PATH", default_value = "/opt/datachain-rope/reth/data/jwt.hex")]
        jwt_secret_path: String,
        #[arg(long, env = "COMMITTEE_PATH", default_value = "/opt/datachain-rope/config/evm-quorum-committee.json")]
        committee_path: String,
        /// Milliseconds between quorum rounds. Defaults to mainnet's
        /// existing knot_interval (4200ms) per rope-node/src/config.rs.
        #[arg(long, env = "TICK_INTERVAL_MS", default_value_t = 4200)]
        tick_interval_ms: u64,
        #[arg(long, env = "FEE_RECIPIENT", default_value = "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195")]
        fee_recipient: String,
        #[arg(long, env = "ATTEST_TIMEOUT_MS", default_value_t = 2000)]
        attest_timeout_ms: u64,
        #[arg(long, env = "COMMIT_TIMEOUT_MS", default_value_t = 2000)]
        commit_timeout_ms: u64,
    },
    /// Legacy single-node fixed-interval driver (Option 1). Kept only as
    /// an emergency rollback path if the quorum protocol ever needs to be
    /// bypassed — normal operation is Attester+Proposer, not this.
    Production {
        #[arg(long, env = "LOCAL_ENGINE_URL", default_value = "http://127.0.0.1:8552")]
        local_engine_url: String,
        #[arg(long, env = "LOCAL_RPC_URL", default_value = "http://127.0.0.1:8595")]
        local_rpc_url: String,
        #[arg(long, env = "JWT_SECRET_PATH", default_value = "/opt/datachain-rope/reth/data/jwt.hex")]
        jwt_secret_path: String,
        #[arg(long, env = "TICK_INTERVAL_MS", default_value_t = 4200)]
        tick_interval_ms: u64,
        #[arg(long, env = "FEE_RECIPIENT", default_value = "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195")]
        fee_recipient: String,
    },
    /// Print this node's Ed25519 pubkey (hex) from its
    /// `validator_key.bin`, for enrolling it into the committee roster.
    /// Used by `onboard-evm-quorum-node.sh` so adding a node to the
    /// quorum is a data change, not a code change.
    PrintPubkey {
        #[arg(long, env = "VALIDATOR_KEY_PATH", default_value = "/home/ubuntu/.rope/validator_key.bin")]
        validator_key_path: String,
    },
    /// Legacy standalone follower (no quorum verification of its own —
    /// just trusts and mirrors whatever the upstream RPC serves). Superseded
    /// by running Attester + letting the Proposer's /commit calls drive it,
    /// which is what actually gives followers a Byzantine-resistant check.
    Follower {
        #[arg(long, env = "LOCAL_ENGINE_URL", default_value = "http://127.0.0.1:8552")]
        local_engine_url: String,
        #[arg(long, env = "LOCAL_RPC_URL", default_value = "http://127.0.0.1:8595")]
        local_rpc_url: String,
        #[arg(long, env = "JWT_SECRET_PATH", default_value = "/opt/datachain-rope/reth/data/jwt.hex")]
        jwt_secret_path: String,
        #[arg(long, env = "UPSTREAM_RPC_URL")]
        upstream_rpc_url: String,
        #[arg(long, env = "POLL_INTERVAL_MS", default_value_t = 1000)]
        poll_interval_ms: u64,
        #[arg(long, env = "MAX_BATCH", default_value_t = 500)]
        max_batch: u64,
    },
}

fn read_jwt(path: &str) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading jwt secret at {path}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::Attester {
            local_engine_url,
            local_rpc_url,
            jwt_secret_path,
            validator_key_path,
            committee_path,
            bind,
            catch_up_rpc_url,
        } => {
            let jwt = read_jwt(&jwt_secret_path)?;
            let engine = EngineClient::new(local_engine_url, local_rpc_url, &jwt)?;
            let node_identity =
                identity::load_from_validator_keystore(std::path::Path::new(&validator_key_path))
                    .context("loading this node's validator identity")?;
            let committee = committee::Committee::load(std::path::Path::new(&committee_path))
                .context("loading committee roster")?;

            if !committee.contains_pubkey(&node_identity.pubkey_hex()) {
                anyhow::bail!(
                    "this node's pubkey {} is not present in the committee roster at {} — \
                     add it and redistribute the roster before starting the attester",
                    node_identity.pubkey_hex(),
                    committee_path
                );
            }

            let catch_up_source = match catch_up_rpc_url {
                Some(url) => {
                    tracing::info!("catch-up source configured: {url}");
                    Some(EngineClient::new_readonly(url)?)
                }
                None => {
                    tracing::warn!(
                        "no --catch-up-rpc-url configured — this node will NOT be able to \
                         self-heal if it falls behind (e.g. after a restart)"
                    );
                    None
                }
            };

            tracing::info!(
                "starting ATTESTER pubkey={} committee_size={} quorum_threshold={}",
                node_identity.pubkey_hex(),
                committee.len(),
                committee.quorum_threshold()
            );

            let state = Arc::new(attester::new_state_with_catch_up(
                engine,
                node_identity,
                committee,
                catch_up_source,
            ));
            attester::serve(state, &bind).await
        }
        Command::Proposer {
            local_engine_url,
            local_rpc_url,
            jwt_secret_path,
            committee_path,
            tick_interval_ms,
            fee_recipient,
            attest_timeout_ms,
            commit_timeout_ms,
        } => {
            let jwt = read_jwt(&jwt_secret_path)?;
            let local = EngineClient::new(local_engine_url, local_rpc_url, &jwt)?;
            let committee = committee::Committee::load(std::path::Path::new(&committee_path))
                .context("loading committee roster")?;
            let http = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()?;

            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                * 1000;
            proposer::seed_round_counter(seed);

            proposer::run(
                &local,
                &http,
                &committee,
                proposer::ProposerConfig {
                    tick_interval: Duration::from_millis(tick_interval_ms),
                    fee_recipient,
                    attest_timeout: Duration::from_millis(attest_timeout_ms),
                    commit_timeout: Duration::from_millis(commit_timeout_ms),
                },
            )
            .await
        }
        Command::Production {
            local_engine_url,
            local_rpc_url,
            jwt_secret_path,
            tick_interval_ms,
            fee_recipient,
        } => {
            let jwt = read_jwt(&jwt_secret_path)?;
            let local = EngineClient::new(local_engine_url, local_rpc_url, &jwt)?;
            tracing::info!(
                "starting legacy PRODUCTION driver (no quorum), tick={}ms fee_recipient={fee_recipient}",
                tick_interval_ms
            );
            production::run(
                &local,
                production::ProductionConfig {
                    tick_interval: Duration::from_millis(tick_interval_ms),
                    fee_recipient,
                },
            )
            .await
        }
        Command::PrintPubkey { validator_key_path } => {
            let identity = identity::load_from_validator_keystore(std::path::Path::new(&validator_key_path))
                .context("loading validator key")?;
            println!("{}", identity.pubkey_hex());
            Ok(())
        }
        Command::Follower {
            local_engine_url,
            local_rpc_url,
            jwt_secret_path,
            upstream_rpc_url,
            poll_interval_ms,
            max_batch,
        } => {
            let local_jwt = read_jwt(&jwt_secret_path)?;
            let local = EngineClient::new(local_engine_url, local_rpc_url, &local_jwt)?;
            let upstream = EngineClient::new_readonly(upstream_rpc_url)?;
            tracing::info!(
                "starting legacy standalone FOLLOWER (no quorum verification), poll={}ms max_batch={max_batch}",
                poll_interval_ms
            );
            follower::run(
                &local,
                &upstream,
                follower::FollowerConfig {
                    poll_interval: Duration::from_millis(poll_interval_ms),
                    max_batch,
                },
            )
            .await
        }
    }
}
