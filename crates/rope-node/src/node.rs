//! Datachain Rope Node implementation
//!
//! Full node implementation with integrated libp2p swarm networking
//! and string production.

use crate::config::{NodeConfig, NodeMode};
use crate::consensus_orchestrator::{ConsensusOrchestrator, OrchestratorConfig};
use crate::evm_backend::{EvmBackend, EvmBackendConfig};
use crate::genesis;
use crate::ledger_manager::LedgerManager;
use crate::metrics::MetricsServer;
use crate::rpc_server::RpcServer;
use crate::string_producer::{ProductionEvent, StringProducer, StringProducerConfig};
use rope_ai_framework::{AgentFramework, AgentFrameworkConfig};
use rope_iot_gateway::{IoTGateway, IoTGatewayConfig};

use parking_lot::RwLock;
use rope_core::clock::ClockManager;
use rope_core::lattice::StringLattice;
use rope_core::string::PublicKey;
use rope_core::types::{NodeId, StringId};
use rope_crypto::oes::OESManager;
use rope_storage::LedgerStore;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio::sync::{broadcast, mpsc};

use rope_network::{
    swarm::{GossipSubConfig, KademliaConfig, RequestResponseConfig},
    RopeSwarmRuntime, SwarmCommand, SwarmConfig, SwarmNetworkEvent, TransportConfig,
};

/// Node state
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeState {
    /// Node is starting up
    Starting,
    /// Node is syncing
    Syncing,
    /// Node is running normally
    Running,
    /// Node is shutting down
    Stopping,
    /// Node has stopped
    Stopped,
}

/// Datachain Rope Node
///
/// Architecture: rope-node IS Datachain Rope. It runs consensus, produces
/// strings, manages testimony, finality, and AI agents natively.
/// An optional EVM execution layer (Reth in production, per
/// `reth-blue-green-ipfs-architecture.mdc`) can be attached as a verifier:
/// when present, rope-node delegates EVM state queries to it; when absent,
/// rope-node runs fully on its own.
pub struct RopeNode {
    config: NodeConfig,
    data_dir: PathBuf,
    state: Arc<RwLock<NodeState>>,
    shutdown_tx: Option<mpsc::Sender<()>>,
    swarm_runtime: Arc<RwLock<Option<RopeSwarmRuntime>>>,
    network_event_rx: Arc<RwLock<Option<broadcast::Receiver<SwarmNetworkEvent>>>>,
    identity_seed: Option<[u8; 32]>,
    node_id: Option<NodeId>,
    producer_shutdown_tx: Option<mpsc::Sender<()>>,
    current_round: Arc<RwLock<u64>>,
    /// EVM backend — optional EVM execution-layer verifier (Reth in production)
    evm_backend: Option<Arc<EvmBackend>>,
    /// Consensus orchestrator — native consensus pipeline
    orchestrator: Option<Arc<ConsensusOrchestrator>>,
    /// Personal ledger manager — one String per wallet
    ledger_manager: Option<Arc<LedgerManager>>,
    /// IoT Gateway — bridges MQTT/CoAP/HTTP to personal Strings
    iot_gateway: Option<Arc<IoTGateway>>,
    /// AI Agent Framework — pluggable domain-specific agents
    ai_framework: Option<Arc<AgentFramework>>,
}

impl RopeNode {
    pub async fn new(config: NodeConfig, data_dir: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            config,
            data_dir,
            state: Arc::new(RwLock::new(NodeState::Starting)),
            shutdown_tx: None,
            swarm_runtime: Arc::new(RwLock::new(None)),
            network_event_rx: Arc::new(RwLock::new(None)),
            identity_seed: None,
            node_id: None,
            producer_shutdown_tx: None,
            current_round: Arc::new(RwLock::new(0)),
            evm_backend: None,
            orchestrator: None,
            ledger_manager: None,
            iot_gateway: None,
            ai_framework: None,
        })
    }

    /// Get current state
    pub fn state(&self) -> NodeState {
        self.state.read().clone()
    }

    /// Get current block/anchor number
    pub fn block_number(&self) -> u64 {
        *self.current_round.read()
    }

    /// Get swarm command sender for external control
    pub fn swarm_command_sender(&self) -> Option<mpsc::Sender<SwarmCommand>> {
        self.swarm_runtime
            .read()
            .as_ref()
            .and_then(|s| s.command_sender())
    }

    /// Run the node
    pub async fn run(&mut self) -> anyhow::Result<()> {
        tracing::info!("Starting Datachain Rope node...");

        // Set state to starting
        *self.state.write() = NodeState::Starting;

        // Initialize components
        self.init_storage().await?;
        let (identity_seed, node_id) = self.init_crypto().await?;
        self.identity_seed = Some(identity_seed);
        self.node_id = Some(node_id.clone());

        // Initialize and start libp2p network
        self.init_network(identity_seed).await?;

        // Initialize genesis if needed
        let genesis_string_id = self.init_genesis().await?;

        // Initialize EVM backend (optional EVM execution-layer verifier — Reth in prod)
        let _evm_backend_handle = self.init_evm_backend().await;

        // Initialize consensus orchestrator — this always runs natively.
        // The EVM backend is attached as an optional verifier when available.
        let orch_config = OrchestratorConfig {
            min_testimonies: self.config.consensus.min_testimonies,
            min_anchor_confirmations: 3,
            verification_interval_secs: 60,
            ai_agents_enabled: self.config.consensus.ai_agents_enabled,
            ai_min_confidence: 0.7,
            max_pending_txs: 500,
        };
        // Quipu Canon v2.0 Phase 2 — load (or generate) the persistent
        // hybrid consensus signing key and build the committee registry.
        // This binds the validator identity for the life of the data dir
        // so testimony signatures verify across restarts, and turns on
        // real signature verification against the committee roster
        // (`validator_set.json` in the data dir, when present).
        // See `validator_keystore.rs`.
        let validator_identity = crate::validator_keystore::load_or_create(&self.data_dir)?;
        let validator_registry =
            crate::validator_keystore::build_registry(&self.data_dir, &validator_identity)?;
        let orchestrator = Arc::new(ConsensusOrchestrator::new_with_validator(
            orch_config,
            node_id,
            self.evm_backend.clone(),
            self.current_round.clone(),
            validator_identity.signer.clone(),
            validator_identity.node_id,
            validator_registry,
        ));
        self.orchestrator = Some(orchestrator);

        // Initialize personal ledger subsystem.
        //
        // Quipu Canon v2.0 Phase 1.6: the LedgerStore is RocksDB-backed
        // by default so personal ledgers (agent wallets, testimonies,
        // GDPR tombstones) survive process restarts. Opt out with
        // `ROPE_LEDGER_PERSISTENCE=0` (tests / ephemeral sandboxes);
        // override the DB location with `ROPE_LEDGER_DB_PATH`.
        let lattice = Arc::new(StringLattice::new());
        // 2026-07-27 P1.2 — finality BFS must not run inside add_string.
        lattice.start_finality_actor();
        let persistence_enabled = std::env::var("ROPE_LEDGER_PERSISTENCE")
            .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(true);
        let ledger_db_path = std::env::var("ROPE_LEDGER_DB_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| self.data_dir.join("ledger_db"));
        // Phase 1.6.1 (2026-08-11 P1) — lazy rehydration.
        // The 2026-08-11 outage was caused by the eager open loading all
        // ~532K knot payloads into RAM at boot (RSS 200 MB -> 4.5 GB in
        // ~5 min), crash-looping under the systemd cgroup ceiling before
        // the RPC listener could bind. Enable lazy mode in production
        // via `ROPE_LAZY_REHYDRATE=1` so:
        //   * open pays only the RocksDB WAL replay + descriptor/chain
        //     recovery cost (~seconds, not minutes),
        //   * tombstones + ledger descriptors are still restored at boot
        //     (small in aggregate, needed by hot-path readers),
        //   * knot payloads are loaded on demand via
        //     `LedgerManager::ensure_string_loaded` when a query touches
        //     them (typical working set is a small fraction of history),
        //   * optionally a bounded background pass fills the rest via
        //     `ROPE_REHYDRATE_ASYNC=1` after RPC has already bound.
        // The old eager path (default when `ROPE_LAZY_REHYDRATE` is
        // absent) is preserved bit-for-bit for rollback safety.
        let lazy_rehydrate = std::env::var("ROPE_LAZY_REHYDRATE")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        let (ledger_store, recovered_blobs, recovered_tombstones) = if persistence_enabled {
            let open_result = if lazy_rehydrate {
                LedgerStore::open_with_recovery_lazy(&ledger_db_path)
            } else {
                LedgerStore::open_with_recovery(&ledger_db_path)
            };
            match open_result {
                Ok((store, blobs, tombstones)) => {
                    if lazy_rehydrate {
                        tracing::info!(
                            "Ledger persistence active (LAZY) at {:?} — {} tombstones on disk; \
                             knot payloads will be loaded on demand \
                             (Phase 1.6.1 P1, 2026-08-11)",
                            ledger_db_path,
                            tombstones.len()
                        );
                        debug_assert!(
                            blobs.is_empty(),
                            "lazy open must return an empty blob vec"
                        );
                    } else {
                        tracing::info!(
                            "Ledger persistence active at {:?} — {} knot blobs, {} tombstones on disk",
                            ledger_db_path,
                            blobs.len(),
                            tombstones.len()
                        );
                    }
                    (Arc::new(store), blobs, tombstones)
                }
                Err(e) => {
                    // A node that silently loses every ledger on restart is
                    // worse than one that refuses to start with a clear
                    // error — fail fast so operators notice.
                    return Err(anyhow::anyhow!(
                        "Failed to open persistent ledger store at {:?}: {e}. \
                         Fix the DB (or set ROPE_LEDGER_PERSISTENCE=0 to \
                         explicitly accept in-memory operation).",
                        ledger_db_path
                    ));
                }
            }
        } else {
            tracing::warn!(
                "ROPE_LEDGER_PERSISTENCE=0 — personal ledgers are IN-MEMORY and \
                 will NOT survive a restart"
            );
            (Arc::new(LedgerStore::new()), Vec::new(), Vec::new())
        };
        let oes_seed: [u8; 32] = {
            let mut s = [0u8; 32];
            s.copy_from_slice(&identity_seed[..32]);
            s
        };
        let oes_manager = Arc::new(OESManager::genesis(&oes_seed));
        let clock_manager = Arc::new(ClockManager::new(node_id));
        let creator_key = PublicKey::new(identity_seed, Vec::new());
        let ledger = Arc::new(LedgerManager::new(
            lattice,
            ledger_store,
            oes_manager,
            node_id,
            creator_key,
            clock_manager,
        ));
        // Phase 1.6 — replay recovered knots, tombstones, and ledger
        // descriptors into the fresh lattice + registry.
        //
        // Phase 1.6.1 (2026-08-11 P1) — when `ROPE_LAZY_REHYDRATE=1` the
        // eager `open_with_recovery` returned an empty knot-blob vec (see
        // above), so we take the metadata-only path here. Knot payloads
        // then load on demand via `LedgerManager::ensure_string_loaded`.
        if lazy_rehydrate {
            ledger.rehydrate_metadata_only(recovered_tombstones);
        } else {
            ledger.rehydrate_from_disk(recovered_blobs, recovered_tombstones);
        }
        self.ledger_manager = Some(ledger.clone());
        tracing::info!("Personal ledger subsystem initialized (rope_* RPC methods active)");

        // Phase 1.6.1 (2026-08-11 P1) — optional background rehydration
        // pass. Off by default in lazy mode (the working set is small);
        // opt in with `ROPE_REHYDRATE_ASYNC=1` when you'd rather pay
        // steady I/O to warm the whole lattice. Batch size and
        // between-batch sleep are tunable so operators can trade
        // completion latency against RSS-growth smoothness.
        //
        // Safety: this only runs when the lazy path is active — the
        // eager path already loaded every knot synchronously so an
        // async pass would be a no-op (all `restore_string` calls
        // would hit the shard's already-present fast return).
        let rehydrate_async = std::env::var("ROPE_REHYDRATE_ASYNC")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        if lazy_rehydrate && rehydrate_async {
            let batch_size: usize = std::env::var("ROPE_REHYDRATE_BATCH")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .filter(|n: &usize| *n > 0)
                .unwrap_or(10_000);
            let sleep_ms: u64 = std::env::var("ROPE_REHYDRATE_SLEEP_MS")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(250);
            let sleep_between = std::time::Duration::from_millis(sleep_ms);
            let ledger_bg = ledger.clone();
            tracing::info!(
                "Spawning background ledger rehydration (batch_size={}, \
                 sleep_between_batches={}ms) — RPC listener will bind BEFORE \
                 rehydration completes",
                batch_size,
                sleep_ms
            );
            // spawn_blocking because RocksDB iteration is synchronous
            // and would otherwise block the tokio runtime.
            tokio::task::spawn_blocking(move || {
                match ledger_bg.rehydrate_strings_in_background(batch_size, sleep_between) {
                    Ok(n) => tracing::info!(
                        "Background ledger rehydration finished ({} knots restored)",
                        n
                    ),
                    Err(e) => tracing::error!(
                        "Background ledger rehydration failed: {} — node will still \
                         serve queries via on-demand loading",
                        e
                    ),
                }
            });
        }

        // Auto-anchor the signed deployer attestation onto the deployer's
        // personal ledger (Quipu Canon: this lives on the global lattice ==
        // main Rope ledger). Idempotent across restarts via a marker file.
        // See `master-node-governance.mdc` for the authority model.
        if let Err(e) = self.try_anchor_deployer_attestation(&ledger).await {
            tracing::warn!("Deployer attestation auto-anchor skipped: {}", e);
        }

        // Spawn ecosystem entity-manifest refresh tasks. Pulls
        // https://tanastok.io/api/v1/tanastok-entity-manifest every 5
        // minutes and merges into the live label registry consumed by
        // the rope_listStrings / rope_resolveLabel RPC paths and DCScan.
        // Honours `ROPE_DISABLE_ENTITY_MANIFEST=1` for offline runs.
        // See `SPEC_TANASTOK_ENTITY_INTEGRATION_v1.md` Phase 5.
        crate::entity_manifest::spawn_refresh_task(vec![
            crate::entity_manifest::ManifestSource::tanastok_default(),
        ]);
        tracing::info!(
            "entity-manifest refresh tasks spawned (Tanastok 5-min cadence)",
        );

        // Initialize IoT Gateway — bridges MQTT/CoAP/HTTP to personal Strings
        if self.config.iot_gateway.enabled {
            let iot_config = IoTGatewayConfig {
                enabled: true,
                mqtt_port: self.config.iot_gateway.mqtt_port,
                coap_port: self.config.iot_gateway.coap_port,
                max_devices: self.config.iot_gateway.max_devices,
                ..Default::default()
            };
            let mut gateway = IoTGateway::new(iot_config);

            let ledger_for_iot = ledger.clone();
            let sink: rope_iot_gateway::gateway::IoTSink =
                Arc::new(move |wallet, itype, desc, meta| {
                    use rope_core::personal_ledger::{InteractionRecord, InteractionType};
                    let record = InteractionRecord {
                        interaction_type: InteractionType::Custom(itype),
                        counterparty: None,
                        data: desc.as_bytes().to_vec(),
                        timestamp: chrono::Utc::now().timestamp(),
                        metadata: meta,
                    };
                    ledger_for_iot
                        .append_to_ledger(&wallet, record)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
            gateway.set_sink(sink);

            let gateway = Arc::new(gateway);
            let gw_clone = gateway.clone();
            tokio::spawn(async move {
                if let Err(e) = gw_clone.start().await {
                    tracing::error!("IoT Gateway start error: {}", e);
                }
            });
            self.iot_gateway = Some(gateway);
            tracing::info!(
                "IoT Gateway initialized (MQTT:{} CoAP:{} max:{})",
                self.config.iot_gateway.mqtt_port,
                self.config.iot_gateway.coap_port,
                self.config.iot_gateway.max_devices
            );
        }

        // Initialize AI Agent Framework — pluggable domain-specific agents
        if self.config.ai_framework.enabled {
            let fw_config = AgentFrameworkConfig {
                enabled: true,
                builtin_maintenance_agent: self.config.ai_framework.builtin_maintenance_agent,
                builtin_anomaly_agent: self.config.ai_framework.builtin_anomaly_agent,
                max_agents: self.config.ai_framework.max_agents,
                scheduler_interval_secs: self.config.ai_framework.scheduler_interval_secs,
                ..Default::default()
            };
            let mut framework = AgentFramework::new(fw_config);

            let ledger_for_ai = ledger.clone();
            let ai_sink: rope_ai_framework::framework::DiagnosisSink =
                Arc::new(move |wallet, itype, desc, meta| {
                    use rope_core::personal_ledger::{InteractionRecord, InteractionType};
                    let record = InteractionRecord {
                        interaction_type: InteractionType::Custom(itype),
                        counterparty: None,
                        data: desc.as_bytes().to_vec(),
                        timestamp: chrono::Utc::now().timestamp(),
                        metadata: meta,
                    };
                    ledger_for_ai
                        .append_to_ledger(&wallet, record)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
            framework.set_sink(ai_sink);

            let framework = Arc::new(framework);
            if let Err(e) = framework.register_builtins().await {
                tracing::warn!("Failed to register built-in AI agents: {}", e);
            }

            let fw_clone = framework.clone();
            tokio::spawn(async move {
                fw_clone.start_scheduler().await;
            });

            self.ai_framework = Some(framework);
            tracing::info!(
                "AI Agent Framework initialized (max:{} scheduler:{}s)",
                self.config.ai_framework.max_agents,
                self.config.ai_framework.scheduler_interval_secs
            );
        }

        // Start string producer if validator
        let producer_handle = if self.config.consensus.enabled
            && matches!(self.config.node.mode, NodeMode::Validator)
        {
            Some(
                self.start_string_producer(node_id, genesis_string_id)
                    .await?,
            )
        } else {
            tracing::info!("String production disabled (non-validator mode)");
            None
        };

        // Start RPC server with EVM backend, consensus orchestrator, ledger
        // manager, AND master-node governance + deployer identity (added 2026-05-03,
        // see master-node-governance.mdc).
        let rpc_handle = if self.config.rpc.enabled {
            let current_round = self.current_round.clone();
            let chain_id = self.config.node.chain_id;
            let evm_backend = self.evm_backend.clone();
            let orchestrator = self.orchestrator.clone();
            let ledger = self.ledger_manager.clone();
            let iot = self.iot_gateway.clone();
            let ai = self.ai_framework.clone();

            // Load master-nodes.toml registry (best effort; node still boots
            // if the registry file is missing — but governance RPC methods
            // will refuse all actions in that case).
            let governance = match crate::governance::GovernanceManager::from_file(
                &self.config.governance.master_nodes_file,
                &self.config.governance.log_path,
                self.config.governance.enforce,
            ) {
                Ok(g) => {
                    let r = g.registry_snapshot();
                    tracing::info!(
                        "Governance loaded: {} master node(s), {} member node(s), {} founder key(s), enforce={}",
                        r.master_nodes.len(),
                        r.member_nodes.len(),
                        r.founder.founder_keys.len(),
                        self.config.governance.enforce
                    );
                    Some(g)
                }
                Err(e) => {
                    tracing::warn!(
                        "Governance disabled (could not load {}): {e}",
                        self.config.governance.master_nodes_file
                    );
                    None
                }
            };
            let deployer = Some(self.config.deployer.clone());
            let self_node_id = self
                .node_id
                .as_ref()
                .map(|n| n.to_hex())
                .unwrap_or_default();

            let rpc_server = RpcServer::new_full_v2(
                &self.config.rpc,
                chain_id,
                current_round,
                evm_backend,
                orchestrator,
                ledger,
                iot,
                ai,
                governance,
                deployer,
                self_node_id,
            )
            .await?;
            Some(tokio::spawn(async move {
                if let Err(e) = rpc_server.run().await {
                    tracing::error!("RPC server error: {}", e);
                }
            }))
        } else {
            None
        };

        // P1 §17.5 #3 — internal loopback RPC watchdog.
        //
        // This is independent from the systemd/HA layer: it probes
        // 127.0.0.1:<rpc_http_port> for `eth_blockNumber` on its own
        // interval, writes a JSON snapshot to `<data_dir>/self-watchdog.json`
        // on every tick, and — when `ROPE_SELF_WATCHDOG_SUICIDE=1` is set —
        // calls `std::process::exit(1)` if no probe has succeeded in
        // `ROPE_SELF_WATCHDOG_STALL_SECS` (default 60s), forcing systemd
        // to restart even under a wedge that leaves the RPC accept-loop
        // parked in a lock. Runs alongside the RPC server task so an
        // absent RPC (`node.rpc.enabled = false`) skips the watchdog
        // entirely — nothing to probe.
        let watchdog_handle = if self.config.rpc.enabled
            && crate::self_watchdog::watchdog_enabled_from_env()
        {
            let wd_cfg = crate::self_watchdog::WatchdogConfig::from_env(
                &self.data_dir,
                &self.config.rpc.http_addr,
            );
            tracing::info!(
                "Self-watchdog enabled: probe={} interval={:?} timeout={:?} \
                 stall_threshold={:?} startup_grace={:?} suicide={} state_file={:?}",
                wd_cfg.probe_url,
                wd_cfg.interval,
                wd_cfg.timeout,
                wd_cfg.stall_threshold,
                wd_cfg.startup_grace,
                wd_cfg.suicide_enabled,
                wd_cfg.state_file,
            );
            let (_state, handle) = crate::self_watchdog::spawn(wd_cfg);
            Some(handle)
        } else {
            if !self.config.rpc.enabled {
                tracing::debug!("Self-watchdog skipped (rpc.enabled=false)");
            } else {
                tracing::info!("Self-watchdog disabled via ROPE_SELF_WATCHDOG_ENABLED=0");
            }
            None
        };

        // Start metrics server
        let metrics_handle = if self.config.metrics.enabled {
            let metrics_server = MetricsServer::new(&self.config.metrics)?;
            Some(tokio::spawn(async move {
                if let Err(e) = metrics_server.run().await {
                    tracing::error!("Metrics server error: {}", e);
                }
            }))
        } else {
            None
        };

        // Start network event processing
        let network_handle = self.start_network_event_processor();

        // Set state to running
        *self.state.write() = NodeState::Running;

        self.print_startup_banner();

        // Wait for shutdown signal
        self.wait_for_shutdown().await;

        // Graceful shutdown
        *self.state.write() = NodeState::Stopping;
        tracing::info!("Shutting down...");

        // Stop string producer
        if let Some(tx) = self.producer_shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        // Stop swarm
        self.stop_network().await?;

        // Stop other components
        if let Some(handle) = rpc_handle {
            handle.abort();
        }
        if let Some(handle) = watchdog_handle {
            handle.abort();
        }
        if let Some(handle) = metrics_handle {
            handle.abort();
        }
        if let Some(handle) = network_handle {
            handle.abort();
        }
        if let Some(handle) = producer_handle {
            handle.abort();
        }

        *self.state.write() = NodeState::Stopped;
        tracing::info!("Node stopped");

        Ok(())
    }

    /// Anchor the signed `[deployer]` attestation onto the deployer's
    /// personal ledger (which lives on the global Datachain Rope lattice
    /// — i.e. the main Rope ledger).
    ///
    /// Skipped when:
    ///   - `[deployer].self_signature` is empty (claim only, not signed)
    ///   - `[deployer].wallet_address` is empty or invalid
    ///   - The deployer already has a populated personal ledger on this
    ///     node (`entry_count > 0`). This makes the call idempotent across
    ///     restarts once the ledger storage backend is persistent. With
    ///     today's in-memory `LedgerStore`, every restart re-anchors —
    ///     that's intentional, it keeps the rebuilt in-memory chain
    ///     populated. Once the RocksDB column family lands the check
    ///     will short-circuit on a healthy node and no extra knot is
    ///     created.
    ///
    /// Use the RPC method `rope_anchorDeployerAttestation` to anchor on
    /// demand (e.g. after re-signing with a fresh founder key).
    async fn try_anchor_deployer_attestation(
        &self,
        ledger: &std::sync::Arc<crate::ledger_manager::LedgerManager>,
    ) -> Result<(), String> {
        let dep = &self.config.deployer;
        if dep.self_signature.trim().is_empty() {
            return Err("self_signature is empty (run `rope identity sign-deployer`)".into());
        }
        if dep.wallet_address.trim().is_empty() {
            return Err("[deployer].wallet_address is empty".into());
        }

        // Ledger-driven idempotency. We can't use a marker file because
        // today's LedgerStore is in-memory — a marker would survive across
        // restarts even though the chain it claims to record is gone.
        if let Ok(status) = ledger.get_ledger_status(&dep.wallet_address) {
            if status.entry_count > 0 {
                tracing::debug!(
                    "Deployer attestation already on this node \
                     (wallet={} entries={}); skipping.",
                    dep.wallet_address,
                    status.entry_count
                );
                return Ok(());
            }
        }

        let canonical = deployer_canonical_json_bytes(dep)?;
        let node_id_hex = self
            .node_id
            .as_ref()
            .map(|n| n.to_hex())
            .unwrap_or_default();
        let chain_id = self.config.node.chain_id;

        let _resp = ledger
            .anchor_deployer_attestation(
                &dep.wallet_address,
                &canonical,
                &dep.self_signature,
                &node_id_hex,
                chain_id,
            )
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Initialize the EVM backend (optional EVM execution-layer verifier).
    ///
    /// When the EVM backend is available, rope-node can delegate EVM state
    /// queries to it and cross-verify execution. When it is absent,
    /// rope-node runs natively — consensus, strings, testimony, finality,
    /// and AI agents all function without it.
    ///
    /// In production the EVM backend is **Reth v1.11.2** (per
    /// `reth-blue-green-ipfs-architecture.mdc`). The URL is resolved in this
    /// order:
    ///   1. `EVM_RPC_URL` env var (canonical, preferred)
    ///   2. `ANVIL_URL`  env var (deprecated alias, kept for legacy
    ///                   deployments — emits a one-shot warning)
    ///   3. `[evm_backend].url` from the TOML config (also accepts the
    ///                   legacy `[anvil].url` section thanks to the
    ///                   `serde(alias)` on `NodeConfig::evm_backend`)
    async fn init_evm_backend(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        // Endpoint resolution order:
        //   1. `EVM_RPC_URL` env var (canonical) — may itself be a
        //      comma-separated list of endpoints (primary,fallback1,fallback2).
        //   2. `ANVIL_URL` env var (deprecated alias, one-shot warning).
        //   3. `[evm_backend].url` + `[evm_backend].fallback_urls` from TOML.
        // Env-provided lists take precedence but are always appended with the
        // configured fallbacks so an operator override never silently drops
        // the resilient edge endpoints.
        let mut evm_urls: Vec<String> = match std::env::var("EVM_RPC_URL") {
            Ok(u) => u
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            Err(_) => match std::env::var("ANVIL_URL") {
                Ok(u) => {
                    tracing::warn!(
                        "Using deprecated env var ANVIL_URL={}. \
                         Please rename to EVM_RPC_URL — Anvil was archived \
                         2026-03-31, see reth-blue-green-ipfs-architecture.mdc.",
                        u
                    );
                    vec![u]
                }
                Err(_) => self.config.evm_backend.endpoint_list(),
            },
        };

        // Always fold in the configured fallbacks (de-duplicated) so that an
        // env-provided primary still benefits from the resilient edge list.
        for u in self.config.evm_backend.endpoint_list() {
            if !evm_urls.contains(&u) {
                evm_urls.push(u);
            }
        }
        if evm_urls.is_empty() {
            evm_urls.push("http://127.0.0.1:8595".to_string());
        }

        let evm_url = evm_urls[0].clone();

        // Resolve the upstream Reth WebSocket URL for the subscription
        // bridge (`ws_subscription_bridge.rs`). Precedence lives in
        // `EvmBackendSettings::resolved_ws_url`: env `ROPE_RETH_WS_URL`
        // → `[evm_backend].ws_url` TOML → `ws://127.0.0.1:8547` default.
        // `None` here means the operator explicitly disabled the bridge;
        // in that case `eth_subscribe` returns a canonical JSON-RPC error
        // rather than a dead subscription id.
        let reth_ws_url = self.config.evm_backend.resolved_ws_url();

        let evm_config = EvmBackendConfig {
            urls: evm_urls.clone(),
            expected_chain_id: self.config.node.chain_id,
            reth_ws_url: reth_ws_url.clone(),
            ..Default::default()
        };
        tracing::info!(
            "EVM backend endpoints ({}): {}",
            evm_urls.len(),
            evm_urls.join(", ")
        );
        match reth_ws_url.as_deref() {
            Some(ws) => tracing::info!(
                "EVM WebSocket bridge target: {ws} (eth_subscribe over wss://ws.datachain.network enabled)"
            ),
            None => tracing::warn!(
                "EVM WebSocket bridge disabled (ROPE_RETH_WS_URL or [evm_backend].ws_url set to empty); \
                 eth_subscribe on wss://ws.datachain.network will return -32601 method-unavailable"
            ),
        }

        // Construct and install the EVM backend UNCONDITIONALLY whenever the
        // client can be built. The previous flow dropped the backend on any
        // initial-reachability failure and never tried again — see the
        // 2026-05-20 BLUE outage postmortem
        // (.cursor/rules/handover-blue-outage-2026-05-20-postmortem.mdc §2):
        // rope-node beat reth to the 8595 socket on the post-reboot start,
        // `initialize()` failed with Connection refused, the backend was set
        // to None, and rope_knotIndex / eth_blockNumber returned 0x0 for
        // every subsequent request until the rope-node process was manually
        // restarted. With this change the backend is created, the health
        // checker is spawned unconditionally, and the backend self-heals as
        // soon as reth's RPC port becomes available.
        match EvmBackend::new(evm_config) {
            Ok(backend) => {
                let backend = Arc::new(backend);
                match backend.initialize().await {
                    Ok(()) => {
                        tracing::info!("EVM execution-layer backend connected at {}", evm_url);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "EVM backend unreachable at startup at {} ({}). \
                             Health checker will retry; eth_*/rope_knotIndex will use \
                             native fallback values until the backend recovers.",
                            evm_url,
                            e
                        );
                    }
                }
                let health_handle = backend.spawn_health_checker();
                self.evm_backend = Some(backend);
                Some(health_handle)
            }
            Err(e) => {
                tracing::warn!(
                    "EVM backend client could not be created ({}). \
                     eth_* RPC methods will use native fallback values.",
                    e
                );
                None
            }
        }
    }

    /// Initialize genesis
    async fn init_genesis(&self) -> anyhow::Result<StringId> {
        let genesis_path = self.data_dir.join("genesis.json");

        let genesis = if genesis_path.exists() {
            let content = std::fs::read_to_string(&genesis_path)?;
            serde_json::from_str(&content)?
        } else {
            // Generate genesis based on chain ID
            let gen = if self.config.node.chain_id == 271829 {
                genesis::generate_testnet_genesis()?
            } else {
                genesis::generate_genesis(1, self.config.node.chain_id)?
            };

            // Save genesis
            let content = serde_json::to_string_pretty(&gen)?;
            std::fs::write(&genesis_path, &content)?;
            tracing::info!("Genesis saved to {:?}", genesis_path);

            gen
        };

        tracing::info!("Genesis hash: {}", hex::encode(&genesis.genesis_hash[..8]));
        tracing::info!(
            "Genesis string: {}",
            hex::encode(&genesis.genesis_string_id[..8])
        );

        Ok(StringId::new(genesis.genesis_string_id))
    }

    /// Start the string producer
    async fn start_string_producer(
        &mut self,
        node_id: NodeId,
        genesis_string_id: StringId,
    ) -> anyhow::Result<tokio::task::JoinHandle<()>> {
        let config = StringProducerConfig {
            string_interval_ms: self.config.consensus.block_time_ms,
            min_testimonies: self.config.consensus.min_testimonies,
            max_pending_strings: 1000,
            enabled: true,
            is_validator: matches!(self.config.node.mode, NodeMode::Validator),
        };

        let mut producer = StringProducer::new(config, node_id);
        producer.set_genesis(genesis_string_id);

        let mut event_rx = producer.subscribe();
        let current_round = self.current_round.clone();
        let swarm = self.swarm_runtime.clone();
        let orchestrator = self.orchestrator.clone();

        tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    ProductionEvent::AnchorFinalized {
                        anchor_id,
                        round,
                        strings_included: _,
                    } => {
                        *current_round.write() = round;

                        // Feed anchor into consensus orchestrator for finality
                        // processing, and produce this node's signed testimony
                        // for the anchor (Quipu Canon v2.0 Phase 2). The wire
                        // bytes are gossiped to the committee below so peers
                        // can verify the hybrid (Ed25519 + Dilithium3)
                        // signature against the roster and tally finality.
                        let testimony_wire = if let Some(ref orch) = orchestrator {
                            orch.on_anchor_finalized(round, anchor_id);
                            Some(orch.attest_and_serialize(
                                anchor_id,
                                rope_core::types::AttestationType::Existence,
                            ))
                        } else {
                            None
                        };

                        // Broadcast anchor + signed testimony to P2P network
                        let publish_result = {
                            let swarm_guard = swarm.read();
                            if let Some(sw) = swarm_guard.as_ref() {
                                let msg = format!(
                                    "anchor:{}:{}",
                                    round,
                                    hex::encode(&anchor_id.as_bytes()[..16])
                                );
                                Some((sw.command_sender(), msg))
                            } else {
                                None
                            }
                        };

                        if let Some((Some(cmd_tx), msg)) = publish_result {
                            let _ = cmd_tx
                                .send(rope_network::SwarmCommand::Publish {
                                    topic: "/rope/anchors/1.0.0".to_string(),
                                    data: msg.into_bytes(),
                                })
                                .await;
                            if let Some(wire) = testimony_wire {
                                let _ = cmd_tx
                                    .send(rope_network::SwarmCommand::Publish {
                                        topic: "/rope/testimonies/1.0.0".to_string(),
                                        data: wire,
                                    })
                                    .await;
                            }
                        }
                    }
                    ProductionEvent::ProductionError { round, error } => {
                        tracing::warn!("Production error at round {}: {}", round, error);
                    }
                    _ => {}
                }
            }
        });

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        self.producer_shutdown_tx = Some(shutdown_tx);

        // Start producer
        let handle = tokio::spawn(async move {
            producer.run(shutdown_rx).await;
        });

        tracing::info!(
            "String producer started (interval: {}ms)",
            self.config.consensus.block_time_ms
        );

        Ok(handle)
    }

    /// Print startup banner with node information
    fn print_startup_banner(&self) {
        tracing::info!("╔══════════════════════════════════════════════════════════════╗");
        tracing::info!("║              DATACHAIN ROPE NODE IS RUNNING                  ║");
        tracing::info!("╚══════════════════════════════════════════════════════════════╝");
        tracing::info!("");
        tracing::info!("Chain ID: {}", self.config.node.chain_id);
        tracing::info!("Mode: {:?}", self.config.node.mode);

        // Print peer ID if swarm is running
        if let Some(swarm) = self.swarm_runtime.read().as_ref() {
            if let Some(peer_id) = swarm.local_peer_id() {
                tracing::info!("Peer ID: {}", peer_id);
            }
        }

        tracing::info!("P2P Listen: {}", self.config.network.listen_addr);
        tracing::info!(
            "Bootstrap nodes: {}",
            self.config.network.bootstrap_nodes.len()
        );

        if self.config.rpc.enabled {
            tracing::info!("HTTP RPC: http://{}", self.config.rpc.http_addr);
            tracing::info!("gRPC: {}", self.config.rpc.grpc_addr);
            tracing::info!("WebSocket: ws://{}", self.config.rpc.ws_addr);
        }

        if self.config.metrics.enabled {
            tracing::info!(
                "Metrics: http://{}/metrics",
                self.config.metrics.prometheus_addr
            );
        }

        if self.config.consensus.enabled && matches!(self.config.node.mode, NodeMode::Validator) {
            tracing::info!(
                "String Production: ENABLED ({}ms interval)",
                self.config.consensus.block_time_ms
            );
        }

        if self.config.iot_gateway.enabled {
            tracing::info!(
                "IoT Gateway: MQTT:{} CoAP:{} (max {} devices)",
                self.config.iot_gateway.mqtt_port,
                self.config.iot_gateway.coap_port,
                self.config.iot_gateway.max_devices,
            );
        }

        if self.config.ai_framework.enabled {
            let agent_count = self
                .ai_framework
                .as_ref()
                .map(|f| f.agent_count())
                .unwrap_or(0);
            tracing::info!(
                "AI Agent Framework: {} agents registered (scheduler: {}s)",
                agent_count,
                self.config.ai_framework.scheduler_interval_secs,
            );
        }

        tracing::info!("");
        tracing::info!("Press Ctrl+C to stop the node");
    }

    /// Initialize storage
    async fn init_storage(&self) -> anyhow::Result<()> {
        tracing::info!("Initializing storage...");

        let db_path = self.data_dir.join("db");
        std::fs::create_dir_all(&db_path)?;

        tracing::info!("Storage initialized at {:?}", db_path);
        Ok(())
    }

    /// Initialize cryptography and return identity seed and node ID
    async fn init_crypto(&self) -> anyhow::Result<([u8; 32], NodeId)> {
        tracing::info!("Initializing cryptography (OES with post-quantum support)...");

        let keys_path = self.data_dir.join("keys");
        std::fs::create_dir_all(&keys_path)?;

        // Load or generate keys
        let node_key_path = keys_path.join("node.key");
        let identity_seed: [u8; 32];
        let node_id: NodeId;

        if !node_key_path.exists() {
            tracing::info!("Generating node keys with hybrid post-quantum cryptography...");
            let keypair = rope_crypto::keys::KeyPair::generate_hybrid()?;

            // Save keys
            let private_key_bytes = keypair.private_key_bytes();
            std::fs::write(&node_key_path, &private_key_bytes)?;
            std::fs::write(keys_path.join("node.pub"), keypair.public_key_bytes())?;
            std::fs::write(keys_path.join("node.id"), hex::encode(keypair.node_id()))?;

            // Use first 32 bytes of private key as identity seed for libp2p
            identity_seed = private_key_bytes[..32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to extract identity seed from keypair"))?;

            node_id = NodeId::new(keypair.node_id());

            tracing::info!("Node ID: {}", hex::encode(keypair.node_id()));
            tracing::info!("Keys saved to {:?}", keys_path);
        } else {
            let private_key_bytes = std::fs::read(&node_key_path)?;
            let id_bytes = std::fs::read(keys_path.join("node.id"))?;
            let id_hex = String::from_utf8_lossy(&id_bytes);

            // Extract identity seed from saved private key
            identity_seed = private_key_bytes[..32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid private key format"))?;

            // Parse node ID from hex
            let id_bytes = hex::decode(id_hex.trim())?;
            let mut id_arr = [0u8; 32];
            id_arr.copy_from_slice(&id_bytes[..32]);
            node_id = NodeId::new(id_arr);

            tracing::info!("Node ID: {}", id_hex.trim());
        }

        tracing::info!("Cryptography initialized (Ed25519 + Dilithium3 + Kyber768)");
        Ok((identity_seed, node_id))
    }

    /// Initialize networking with libp2p swarm
    async fn init_network(&mut self, identity_seed: [u8; 32]) -> anyhow::Result<()> {
        tracing::info!("Initializing P2P network with libp2p swarm...");

        // Parse listen address from config
        let listen_addr: SocketAddr = self
            .config
            .network
            .listen_addr
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:9000".parse().unwrap());

        // Build swarm configuration from node config
        let swarm_config = SwarmConfig {
            transport: TransportConfig {
                listen_addr,
                enable_quic: self.config.network.enable_quic,
                enable_tcp: true,
                enable_websocket: false,
                connection_timeout: Duration::from_secs(30),
                idle_timeout: Duration::from_secs(300),
                max_connections: self.config.network.max_peers,
                enable_pq_crypto: true,
                bootstrap_peers: self.config.network.bootstrap_nodes.clone(),
                enable_relay: self.config.network.enable_nat,
                gossip_heartbeat: Duration::from_secs(1),
                kad_replication: 20,
            },
            gossipsub: GossipSubConfig {
                heartbeat_interval: Duration::from_secs(1),
                max_transmit_size: 1024 * 1024, // 1MB max message
                mesh_n: 6,
                mesh_n_low: 4,
                mesh_n_high: 12,
                gossip_lazy: 6,
                history_length: 5,
                history_gossip: 3,
                fanout_ttl: Duration::from_secs(60),
                duplicate_cache_time: Duration::from_secs(60),
                flood_publish: false,
            },
            kademlia: KademliaConfig {
                protocol_name: "/rope/kad/1.0.0".to_string(),
                replication_factor: 20,
                parallelism: 3,
                record_ttl: Duration::from_secs(3600 * 24),
                provider_ttl: Duration::from_secs(3600 * 12),
                server_mode: matches!(self.config.node.mode, NodeMode::Validator),
            },
            request_response: RequestResponseConfig {
                protocol_name: "/rope/req/1.0.0".to_string(),
                request_timeout: Duration::from_secs(30),
                max_concurrent_requests: 100,
            },
            identity_seed: Some(identity_seed),
        };

        // Create and start swarm runtime
        let mut swarm_runtime = RopeSwarmRuntime::new(swarm_config);

        swarm_runtime
            .start()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start swarm: {}", e))?;

        // Get event receiver before moving swarm_runtime
        let event_rx = swarm_runtime.event_receiver();

        // Log peer ID
        if let Some(peer_id) = swarm_runtime.local_peer_id() {
            tracing::info!("Local Peer ID: {}", peer_id);
        }

        // Subscribe to core topics
        swarm_runtime
            .subscribe("/rope/strings/1.0.0")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to strings topic: {}", e))?;
        swarm_runtime
            .subscribe("/rope/gossip/1.0.0")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to gossip topic: {}", e))?;
        swarm_runtime
            .subscribe("/rope/testimonies/1.0.0")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to testimonies topic: {}", e))?;
        swarm_runtime
            .subscribe("/rope/anchors/1.0.0")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to anchors topic: {}", e))?;

        // Connect to bootstrap nodes
        for bootstrap in &self.config.network.bootstrap_nodes {
            match swarm_runtime.dial(bootstrap).await {
                Ok(_) => tracing::info!("Dialing bootstrap node: {}", bootstrap),
                Err(e) => tracing::warn!("Failed to dial bootstrap {}: {}", bootstrap, e),
            }
        }

        // Store references
        *self.swarm_runtime.write() = Some(swarm_runtime);
        *self.network_event_rx.write() = Some(event_rx);

        tracing::info!("P2P network initialized with QUIC + TCP transport");
        tracing::info!("Subscribed to core protocol topics");
        tracing::info!(
            "Bootstrap nodes: {}",
            self.config.network.bootstrap_nodes.len()
        );

        Ok(())
    }

    /// Start network event processor
    fn start_network_event_processor(&self) -> Option<tokio::task::JoinHandle<()>> {
        let event_rx = self.network_event_rx.write().take()?;
        let state = self.state.clone();
        let current_round = self.current_round.clone();
        let orchestrator = self.orchestrator.clone();

        Some(tokio::spawn(async move {
            Self::process_network_events(event_rx, state, current_round, orchestrator).await;
        }))
    }

    /// Process network events from the swarm
    async fn process_network_events(
        mut event_rx: broadcast::Receiver<SwarmNetworkEvent>,
        state: Arc<RwLock<NodeState>>,
        current_round: Arc<RwLock<u64>>,
        orchestrator: Option<Arc<ConsensusOrchestrator>>,
    ) {
        loop {
            // Check if we should stop
            if *state.read() == NodeState::Stopping || *state.read() == NodeState::Stopped {
                break;
            }

            match event_rx.recv().await {
                Ok(event) => {
                    match event {
                        SwarmNetworkEvent::PeerConnected { peer_id } => {
                            tracing::info!("Peer connected: {}", peer_id);
                        }
                        SwarmNetworkEvent::PeerDisconnected { peer_id } => {
                            tracing::info!("Peer disconnected: {}", peer_id);
                        }
                        SwarmNetworkEvent::GossipMessage {
                            topic,
                            data,
                            source,
                        } => {
                            tracing::debug!(
                                "Gossip message on '{}' from {}: {} bytes",
                                topic,
                                source,
                                data.len()
                            );
                            // Process message based on topic
                            Self::handle_gossip_message(
                                &topic,
                                &data,
                                &source,
                                &current_round,
                                orchestrator.as_ref(),
                            )
                            .await;
                        }
                        SwarmNetworkEvent::DhtRecordFound { key, value } => {
                            tracing::debug!(
                                "DHT record found: {} = {} bytes",
                                hex::encode(&key),
                                value.len()
                            );
                        }
                        SwarmNetworkEvent::DhtProvidersFound { key, providers } => {
                            tracing::debug!(
                                "DHT providers for {}: {} providers",
                                hex::encode(&key),
                                providers.len()
                            );
                        }
                        _ => {}
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Network event processor lagged by {} events", n);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::info!("Network event channel closed");
                    break;
                }
            }
        }
    }

    /// Handle incoming gossip messages
    async fn handle_gossip_message(
        topic: &str,
        data: &[u8],
        source: &libp2p::PeerId,
        current_round: &Arc<RwLock<u64>>,
        orchestrator: Option<&Arc<ConsensusOrchestrator>>,
    ) {
        match topic {
            "/rope/strings/1.0.0" => {
                tracing::trace!("Received string announcement from {}", source);
            }
            "/rope/gossip/1.0.0" => {
                tracing::trace!("Received gossip event from {}", source);
            }
            "/rope/testimonies/1.0.0" => {
                // Quipu Canon v2.0 Phase 2: verify the peer testimony's
                // hybrid signature against the committee registry and fold
                // it into the finality tally for its target string.
                match orchestrator {
                    Some(orch) => match orch.submit_peer_testimony(data) {
                        Ok(true) => {
                            tracing::debug!(
                                "Peer testimony from {} accepted — target string finalized",
                                source
                            );
                        }
                        Ok(false) => {
                            tracing::trace!(
                                "Peer testimony from {} accepted (not yet final)",
                                source
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Peer testimony from {} REJECTED: {} ({} bytes)",
                                source,
                                e,
                                data.len()
                            );
                        }
                    },
                    None => {
                        tracing::trace!(
                            "Received testimony from {} but orchestrator not ready",
                            source
                        );
                    }
                }
            }
            "/rope/anchors/1.0.0" => {
                // Parse anchor message
                if let Ok(msg) = String::from_utf8(data.to_vec()) {
                    if msg.starts_with("anchor:") {
                        let parts: Vec<&str> = msg.split(':').collect();
                        if parts.len() >= 2 {
                            if let Ok(round) = parts[1].parse::<u64>() {
                                let local_round = *current_round.read();
                                if round > local_round {
                                    tracing::info!(
                                        "Received anchor #{} from {} (local: #{})",
                                        round,
                                        source,
                                        local_round
                                    );
                                    // In a full implementation, we'd sync here
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                tracing::trace!("Received message on unknown topic: {}", topic);
            }
        }
    }

    /// Stop the network
    async fn stop_network(&mut self) -> anyhow::Result<()> {
        if let Some(mut swarm) = self.swarm_runtime.write().take() {
            swarm
                .stop()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop swarm: {}", e))?;
            tracing::info!("P2P network stopped");
        }
        Ok(())
    }

    /// Wait for shutdown signal
    async fn wait_for_shutdown(&self) {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("Failed to install signal handler")
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

    /// Publish a message to the network
    pub async fn publish(&self, topic: &str, data: Vec<u8>) -> anyhow::Result<()> {
        if let Some(swarm) = self.swarm_runtime.read().as_ref() {
            swarm
                .publish(topic, data)
                .await
                .map_err(|e| anyhow::anyhow!("Publish failed: {}", e))?;
        }
        Ok(())
    }

    /// Get network statistics
    pub fn network_stats(&self) -> Option<rope_network::SwarmStats> {
        self.swarm_runtime.read().as_ref().map(|s| s.stats())
    }
}

impl Drop for RopeNode {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.blocking_send(());
        }
    }
}

/// Canonical JSON encoding of a `[deployer]` attestation, sorted keys at
/// every level, with `self_signature` excluded — must produce the same
/// bytes as `rope identity sign-deployer` so the on-chain record can be
/// re-verified after the fact.
pub(crate) fn deployer_canonical_json_bytes(
    dep: &crate::config::DeployerSettings,
) -> Result<Vec<u8>, String> {
    let mut clone = dep.clone();
    clone.self_signature = String::new();
    let v = serde_json::to_value(&clone).map_err(|e| e.to_string())?;
    Ok(canonical_json_bytes(&v))
}

fn canonical_json_bytes(v: &serde_json::Value) -> Vec<u8> {
    use serde_json::Value;
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(&Value::String((*k).clone()), out);
                    out.push(':');
                    write(&o[*k], out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NodeConfig;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_node_creation() {
        let config = NodeConfig::testnet();
        let temp_dir = TempDir::new().unwrap();

        let node = RopeNode::new(config, temp_dir.path().to_path_buf()).await;
        assert!(node.is_ok());

        let node = node.unwrap();
        assert_eq!(node.state(), NodeState::Starting);
    }
}
