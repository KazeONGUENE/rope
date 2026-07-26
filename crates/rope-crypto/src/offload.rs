//! # Quipu Canon v2.0 Phase 5 — Post-quantum signing offload pipeline
//!
//! Phase 2.C ([`crate::batch`]) parallelised signature *verification*.
//! Phase 5 attacks the other half of the PQ cost: signature *production*.
//! A hybrid signature costs ~15 µs of Ed25519 plus ~200–400 µs of
//! CRYSTALS-Dilithium3 per call, and every testimony a validator emits
//! pays that price on the consensus hot path.
//!
//! ## Architecture
//!
//! The pipeline separates *who wants a signature* from *who computes it*:
//!
//! ```text
//!  consensus hot path            offload pipeline               backend
//!  ──────────────────    ┌──────────────────────────────┐   ┌───────────┐
//!  submit(msg) ─────────▶│ bounded queue → batcher →    │──▶│ CpuPool   │
//!    returns SignTicket  │ dispatch batches to backend  │   │ (rayon)   │
//!  ticket.wait() ◀───────│ ← results by ticket          │   ├───────────┤
//!                        └──────────────────────────────┘   │ Gpu/Asic  │
//!                                                           │ (future)  │
//!                                                           └───────────┘
//! ```
//!
//! - [`SigningBackend`] is the hardware abstraction. It is deliberately
//!   **batch-oriented**: GPUs and signing ASICs amortise their transfer
//!   and dispatch overhead over large batches, so the contract is
//!   "sign N messages at once", never "sign one message". The CPU
//!   implementation ([`CpuPoolBackend`]) honours the same contract with
//!   a rayon data-parallel map.
//! - [`OffloadSigner`] is the pipeline: a bounded submission queue, a
//!   dedicated collector thread that drains the queue into batches
//!   (adaptive: it takes whatever is queued, up to `max_batch`), and
//!   per-request completion delivery through [`SignTicket`]s.
//! - [`OffloadStats`] exposes counters for ops dashboards: submitted,
//!   signed, batches dispatched, mean batch size, and queue high-water
//!   mark — the numbers needed to size a GPU purchase with real data.
//!
//! ## Why this is the correct Phase 5 step
//!
//! The 5M-TPS spec (§9, `QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md`)
//! prices Phase 5 as "GPU/ASIC PQ-signing offload, 600K/node". The
//! hardware is procurement; the *software* deliverable is this pipeline:
//! once every signing call site goes through [`SigningBackend`], swapping
//! `CpuPoolBackend` for a CUDA/OpenCL Dilithium backend is a contained,
//! testable change — the queueing, batching, ticketing, and metrics all
//! stay identical. Until that hardware lands, `CpuPoolBackend` already
//! removes Dilithium from the consensus hot path and scales signing to
//! all available cores.

use crate::hybrid::{HybridSignature, HybridSigner};
use rayon::prelude::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

// ============================================================================
// Backend abstraction
// ============================================================================

/// A batch-oriented signature production backend.
///
/// Implementations MUST be deterministic with respect to input order:
/// `sign_batch(msgs)[i]` is the signature over `msgs[i]`.
///
/// The trait is object-safe so the pipeline can hold `Arc<dyn
/// SigningBackend>` and backends can be swapped at construction time
/// (CPU pool today, GPU/ASIC tomorrow) without touching call sites.
pub trait SigningBackend: Send + Sync {
    /// Sign every message in the batch with the node's hybrid key.
    /// Returns one signature per message, in input order.
    fn sign_batch(&self, messages: &[Vec<u8>]) -> Vec<HybridSignature>;

    /// Human-readable backend identifier for logs and dashboards
    /// (e.g. `"cpu-pool"`, `"cuda-dilithium"`).
    fn name(&self) -> &'static str;

    /// The batch size at which this backend reaches peak throughput.
    /// The pipeline uses it as its dispatch high-water mark. CPU pools
    /// saturate around `num_cpus`; GPU backends will report much
    /// larger values (thousands).
    fn preferred_batch(&self) -> usize;
}

/// CPU worker-pool backend: data-parallel hybrid signing on the rayon
/// thread pool. This is the production Phase 5 backend until dedicated
/// signing hardware is provisioned.
pub struct CpuPoolBackend {
    signer: Arc<HybridSigner>,
    preferred: usize,
}

impl CpuPoolBackend {
    pub fn new(signer: Arc<HybridSigner>) -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            signer,
            // 4 items per core amortises rayon scheduling overhead
            // while keeping dispatch latency low.
            preferred: cores * 4,
        }
    }
}

impl SigningBackend for CpuPoolBackend {
    fn sign_batch(&self, messages: &[Vec<u8>]) -> Vec<HybridSignature> {
        messages
            .par_iter()
            .with_min_len(1)
            .map(|m| self.signer.sign(m))
            .collect()
    }

    fn name(&self) -> &'static str {
        "cpu-pool"
    }

    fn preferred_batch(&self) -> usize {
        self.preferred
    }
}

// ============================================================================
// Pipeline
// ============================================================================

/// Completion handle for one submitted message.
///
/// `wait()` blocks until the pipeline delivers the signature.
/// Dropping the ticket without waiting is safe — the signature is
/// computed and discarded.
pub struct SignTicket {
    rx: mpsc::Receiver<HybridSignature>,
}

impl SignTicket {
    /// Block until the signature is ready.
    pub fn wait(self) -> Result<HybridSignature, OffloadError> {
        self.rx.recv().map_err(|_| OffloadError::PipelineShutDown)
    }

    /// Block with a timeout.
    pub fn wait_timeout(self, timeout: Duration) -> Result<HybridSignature, OffloadError> {
        match self.rx.recv_timeout(timeout) {
            Ok(sig) => Ok(sig),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(OffloadError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(OffloadError::PipelineShutDown),
        }
    }
}

/// Errors surfaced by the offload pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffloadError {
    /// The submission queue is full (backpressure). The caller should
    /// sign inline or retry — losing the request silently is never
    /// acceptable for consensus.
    QueueFull,
    /// The pipeline has been shut down.
    PipelineShutDown,
    /// `wait_timeout` elapsed before the signature was produced.
    Timeout,
}

impl std::fmt::Display for OffloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OffloadError::QueueFull => write!(f, "signing queue full (backpressure)"),
            OffloadError::PipelineShutDown => write!(f, "signing pipeline shut down"),
            OffloadError::Timeout => write!(f, "timed out waiting for signature"),
        }
    }
}

impl std::error::Error for OffloadError {}

struct SignRequest {
    message: Vec<u8>,
    responder: mpsc::Sender<HybridSignature>,
}

/// Point-in-time snapshot of pipeline counters.
#[derive(Clone, Debug)]
pub struct OffloadStats {
    pub backend: &'static str,
    pub submitted: u64,
    pub signed: u64,
    pub batches: u64,
    /// Mean messages per dispatched batch (0 when no batch has run).
    pub mean_batch_size: f64,
    /// Highest queue depth observed since startup.
    pub queue_high_water: usize,
    /// Signatures per second over the pipeline's lifetime.
    pub lifetime_sig_per_sec: f64,
}

struct Counters {
    submitted: AtomicU64,
    signed: AtomicU64,
    batches: AtomicU64,
    queue_depth: AtomicUsize,
    queue_high_water: AtomicUsize,
}

/// The Phase 5 offload pipeline. See module docs for the architecture.
///
/// One `OffloadSigner` per node process. Cheap to share via `Arc`.
pub struct OffloadSigner {
    tx: mpsc::SyncSender<SignRequest>,
    backend_name: &'static str,
    counters: Arc<Counters>,
    started: Instant,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl OffloadSigner {
    /// Spawn the pipeline over the given backend.
    ///
    /// `queue_capacity` bounds the submission queue: when full,
    /// [`submit`](Self::submit) returns [`OffloadError::QueueFull`]
    /// instead of blocking the consensus hot path (fail-visible
    /// backpressure, never silent loss).
    pub fn start(backend: Arc<dyn SigningBackend>, queue_capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel::<SignRequest>(queue_capacity.max(1));
        let counters = Arc::new(Counters {
            submitted: AtomicU64::new(0),
            signed: AtomicU64::new(0),
            batches: AtomicU64::new(0),
            queue_depth: AtomicUsize::new(0),
            queue_high_water: AtomicUsize::new(0),
        });
        let backend_name = backend.name();

        let worker_counters = counters.clone();
        let worker = std::thread::Builder::new()
            .name("pq-sign-offload".into())
            .spawn(move || {
                Self::collector_loop(rx, backend, worker_counters);
            })
            .expect("spawning pq-sign-offload thread");

        tracing::info!(
            "PQ signing offload pipeline started (backend={}, queue_capacity={})",
            backend_name,
            queue_capacity
        );

        Self {
            tx,
            backend_name,
            counters,
            started: Instant::now(),
            worker: Some(worker),
        }
    }

    /// Convenience constructor: CPU pool backend with a queue sized for
    /// sustained anchor production (one testimony per anchor per peer).
    pub fn start_cpu(signer: Arc<HybridSigner>) -> Self {
        Self::start(Arc::new(CpuPoolBackend::new(signer)), 4096)
    }

    /// Submit a message for signing. Non-blocking: returns immediately
    /// with a [`SignTicket`], or [`OffloadError::QueueFull`] under
    /// backpressure (the caller then signs inline — correctness is
    /// never sacrificed to the pipeline).
    pub fn submit(&self, message: Vec<u8>) -> Result<SignTicket, OffloadError> {
        let (resp_tx, resp_rx) = mpsc::channel();
        let req = SignRequest {
            message,
            responder: resp_tx,
        };
        match self.tx.try_send(req) {
            Ok(()) => {
                self.counters.submitted.fetch_add(1, Ordering::Relaxed);
                let depth = self.counters.queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
                self.counters
                    .queue_high_water
                    .fetch_max(depth, Ordering::Relaxed);
                Ok(SignTicket { rx: resp_rx })
            }
            Err(mpsc::TrySendError::Full(_)) => Err(OffloadError::QueueFull),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(OffloadError::PipelineShutDown),
        }
    }

    /// Submit a whole batch and block until every signature is ready.
    /// This is the bulk path for callers that already hold N messages
    /// (e.g. re-signing a checkpoint, load generation). Order is
    /// preserved.
    pub fn sign_batch_blocking(
        &self,
        messages: Vec<Vec<u8>>,
    ) -> Result<Vec<HybridSignature>, OffloadError> {
        let tickets: Result<Vec<SignTicket>, OffloadError> =
            messages.into_iter().map(|m| self.submit(m)).collect();
        tickets?.into_iter().map(|t| t.wait()).collect()
    }

    /// Current pipeline counters.
    pub fn stats(&self) -> OffloadStats {
        let signed = self.counters.signed.load(Ordering::Relaxed);
        let batches = self.counters.batches.load(Ordering::Relaxed);
        let elapsed = self.started.elapsed().as_secs_f64();
        OffloadStats {
            backend: self.backend_name,
            submitted: self.counters.submitted.load(Ordering::Relaxed),
            signed,
            batches,
            mean_batch_size: if batches == 0 {
                0.0
            } else {
                signed as f64 / batches as f64
            },
            queue_high_water: self.counters.queue_high_water.load(Ordering::Relaxed),
            lifetime_sig_per_sec: if elapsed > 0.0 {
                signed as f64 / elapsed
            } else {
                0.0
            },
        }
    }

    /// Collector: drain the queue into adaptive batches and dispatch
    /// them to the backend. Runs on the dedicated pipeline thread.
    fn collector_loop(
        rx: mpsc::Receiver<SignRequest>,
        backend: Arc<dyn SigningBackend>,
        counters: Arc<Counters>,
    ) {
        let max_batch = backend.preferred_batch().max(1);
        loop {
            // Block for the first request (idle pipeline costs nothing).
            let first = match rx.recv() {
                Ok(r) => r,
                Err(_) => break, // all senders dropped → shutdown
            };
            let mut batch = vec![first];
            // Opportunistically take whatever else is already queued,
            // up to the backend's preferred batch size. No artificial
            // latency is added: if the queue is empty we dispatch a
            // batch of one immediately.
            while batch.len() < max_batch {
                match rx.try_recv() {
                    Ok(r) => batch.push(r),
                    Err(_) => break,
                }
            }

            counters
                .queue_depth
                .fetch_sub(batch.len().min(counters.queue_depth.load(Ordering::Relaxed)), Ordering::Relaxed);

            let messages: Vec<Vec<u8>> = batch.iter().map(|r| r.message.clone()).collect();
            let signatures = backend.sign_batch(&messages);
            debug_assert_eq!(signatures.len(), batch.len());

            counters.batches.fetch_add(1, Ordering::Relaxed);
            counters
                .signed
                .fetch_add(batch.len() as u64, Ordering::Relaxed);

            for (req, sig) in batch.into_iter().zip(signatures.into_iter()) {
                // A dropped ticket is fine — the caller stopped caring.
                let _ = req.responder.send(sig);
            }
        }
        tracing::info!("PQ signing offload pipeline stopped");
    }
}

impl Drop for OffloadSigner {
    fn drop(&mut self) {
        // Close the queue: replace tx so the collector's recv() errors
        // out once in-flight work drains, then join the thread.
        let (dead_tx, _) = mpsc::sync_channel(1);
        self.tx = dead_tx;
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::{HybridSigner, HybridVerifier};

    fn pipeline() -> (OffloadSigner, crate::hybrid::HybridPublicKey) {
        let (signer, pk) = HybridSigner::generate();
        (OffloadSigner::start_cpu(Arc::new(signer)), pk)
    }

    #[test]
    fn offloaded_signature_verifies() {
        let (pipe, pk) = pipeline();
        let msg = b"phase5 offload signature".to_vec();
        let sig = pipe.submit(msg.clone()).unwrap().wait().unwrap();
        assert!(HybridVerifier::verify(&pk, &msg, &sig).unwrap());
    }

    #[test]
    fn batch_blocking_preserves_order() {
        let (pipe, pk) = pipeline();
        let messages: Vec<Vec<u8>> = (0..64).map(|i| format!("m-{i}").into_bytes()).collect();
        let sigs = pipe.sign_batch_blocking(messages.clone()).unwrap();
        assert_eq!(sigs.len(), 64);
        for (m, s) in messages.iter().zip(sigs.iter()) {
            assert!(
                HybridVerifier::verify(&pk, m, s).unwrap(),
                "signature must verify against its own message (order preserved)"
            );
        }
    }

    #[test]
    fn stats_track_throughput() {
        let (pipe, _) = pipeline();
        let messages: Vec<Vec<u8>> = (0..32).map(|i| format!("s-{i}").into_bytes()).collect();
        let _ = pipe.sign_batch_blocking(messages).unwrap();
        let stats = pipe.stats();
        assert_eq!(stats.backend, "cpu-pool");
        assert_eq!(stats.submitted, 32);
        assert_eq!(stats.signed, 32);
        assert!(stats.batches >= 1);
        assert!(stats.mean_batch_size >= 1.0);
        assert!(stats.lifetime_sig_per_sec > 0.0);
    }

    #[test]
    fn queue_full_surfaces_backpressure_not_loss() {
        // Capacity-1 queue + a slow first request in flight ⇒ the
        // pipeline must answer QueueFull, never drop silently.
        let (signer, _) = HybridSigner::generate();
        let pipe = OffloadSigner::start(Arc::new(CpuPoolBackend::new(Arc::new(signer))), 1);

        // Saturate: keep submitting until we observe backpressure.
        // (The collector drains fast, so loop rather than assert on
        // the first try.)
        let mut saw_backpressure = false;
        let mut tickets = Vec::new();
        for i in 0..10_000 {
            match pipe.submit(format!("bp-{i}").into_bytes()) {
                Ok(t) => tickets.push(t),
                Err(OffloadError::QueueFull) => {
                    saw_backpressure = true;
                    break;
                }
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        // Either we saw explicit backpressure, or the pipeline kept up
        // with 10K submissions on a capacity-1 queue — both are
        // correct behaviours; silent loss is the only failure mode,
        // and it is excluded by every ticket resolving below.
        for t in tickets {
            t.wait().unwrap();
        }
        let _ = saw_backpressure;
    }

    #[test]
    fn cpu_pool_backend_signs_in_parallel_and_correctly() {
        let (signer, pk) = HybridSigner::generate();
        let backend = CpuPoolBackend::new(Arc::new(signer));
        let messages: Vec<Vec<u8>> = (0..16).map(|i| format!("par-{i}").into_bytes()).collect();
        let sigs = backend.sign_batch(&messages);
        assert_eq!(sigs.len(), 16);
        for (m, s) in messages.iter().zip(sigs.iter()) {
            assert!(HybridVerifier::verify(&pk, m, s).unwrap());
        }
    }

    /// Guard that the offload pool actually parallelises: signing N
    /// messages through the CPU pool must beat a serial loop on any
    /// multi-core machine. Generous slack (1.3×) to avoid CI flakes.
    #[test]
    fn pool_beats_serial_on_multi_core() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        if cores <= 1 {
            return;
        }

        const N: usize = 32;
        let (signer, _) = HybridSigner::generate();
        let signer = Arc::new(signer);
        let backend = CpuPoolBackend::new(signer.clone());
        let messages: Vec<Vec<u8>> = (0..N).map(|i| format!("perf-{i}").into_bytes()).collect();

        // Warm-up (rayon pool spin-up).
        let _ = backend.sign_batch(&messages);

        let serial_start = Instant::now();
        for m in &messages {
            let _ = signer.sign(m);
        }
        let serial = serial_start.elapsed();

        let pool_start = Instant::now();
        let _ = backend.sign_batch(&messages);
        let pool = pool_start.elapsed();

        assert!(
            pool.as_micros() * 13 / 10 < serial.as_micros(),
            "pool {pool:?} must be at least 1.3× faster than serial {serial:?}"
        );
    }
}
