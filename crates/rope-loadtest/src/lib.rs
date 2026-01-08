//! # Datachain Rope Load Testing Infrastructure
//!
//! This module provides comprehensive load testing capabilities for production deployment.
//!
//! ## Features
//!
//! - **API Load Testing**: HTTP/REST endpoint stress testing
//! - **Network Load Testing**: P2P message throughput testing
//! - **Transaction Load Testing**: End-to-end transaction simulation
//! - **Concurrent User Simulation**: Multi-user scenario testing
//! - **Metrics Collection**: Prometheus-compatible metrics
//! - **HDR Histograms**: High-precision latency distribution
//!
//! ## Usage
//!
//! ```bash
//! # Run basic load test
//! cargo run --package rope-loadtest -- --target https://api.dcscan.io --duration 60
//!
//! # Run with custom concurrency
//! cargo run --package rope-loadtest -- --target https://api.dcscan.io --concurrency 100 --rps 1000
//!
//! # Run specific scenarios
//! cargo run --package rope-loadtest -- --scenario strings --target https://api.dcscan.io
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use hdrhistogram::Histogram;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

// ============================================================================
// CONFIGURATION
// ============================================================================

/// Load test configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadTestConfig {
    /// Target base URL
    pub target_url: String,

    /// Test duration in seconds
    pub duration_secs: u64,

    /// Target requests per second
    pub target_rps: u64,

    /// Maximum concurrent requests
    pub max_concurrency: usize,

    /// Request timeout in seconds
    pub request_timeout_secs: u64,

    /// Warmup duration in seconds
    pub warmup_secs: u64,

    /// Ramp-up duration in seconds
    pub ramp_up_secs: u64,

    /// Scenarios to run
    pub scenarios: Vec<String>,

    /// Enable detailed logging
    pub verbose: bool,

    /// Metrics export port
    pub metrics_port: Option<u16>,
}

impl Default for LoadTestConfig {
    fn default() -> Self {
        Self {
            target_url: "http://localhost:3001".to_string(),
            duration_secs: 60,
            target_rps: 100,
            max_concurrency: 50,
            request_timeout_secs: 30,
            warmup_secs: 5,
            ramp_up_secs: 10,
            scenarios: vec!["all".to_string()],
            verbose: false,
            metrics_port: Some(9090),
        }
    }
}

// ============================================================================
// METRICS
// ============================================================================

/// Load test metrics
#[derive(Debug, Default)]
pub struct LoadTestMetrics {
    /// Total requests sent
    pub total_requests: AtomicU64,

    /// Successful requests
    pub successful_requests: AtomicU64,

    /// Failed requests
    pub failed_requests: AtomicU64,

    /// Total bytes sent
    pub bytes_sent: AtomicU64,

    /// Total bytes received
    pub bytes_received: AtomicU64,

    /// Latency histogram (microseconds)
    pub latency_histogram: RwLock<Histogram<u64>>,

    /// Error counts by type
    pub error_counts: RwLock<HashMap<String, u64>>,

    /// Requests per second (rolling)
    pub current_rps: AtomicU64,

    /// Start time
    pub start_time: RwLock<Option<Instant>>,
}

impl LoadTestMetrics {
    pub fn new() -> Self {
        Self {
            latency_histogram: RwLock::new(
                Histogram::new_with_bounds(1, 60_000_000, 3).unwrap(), // 1µs to 60s
            ),
            ..Default::default()
        }
    }

    /// Record a successful request
    pub fn record_success(&self, latency_us: u64, bytes_sent: u64, bytes_received: u64) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.successful_requests.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes_sent, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(bytes_received, Ordering::Relaxed);

        if let Err(e) = self.latency_histogram.write().record(latency_us) {
            warn!("Failed to record latency: {}", e);
        }
    }

    /// Record a failed request
    pub fn record_failure(&self, error_type: &str, latency_us: u64) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.failed_requests.fetch_add(1, Ordering::Relaxed);

        let mut errors = self.error_counts.write();
        *errors.entry(error_type.to_string()).or_insert(0) += 1;

        if let Err(e) = self.latency_histogram.write().record(latency_us) {
            warn!("Failed to record latency: {}", e);
        }
    }

    /// Get summary statistics
    pub fn summary(&self) -> MetricsSummary {
        let hist = self.latency_histogram.read();
        let duration = self
            .start_time
            .read()
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(1.0);

        let total = self.total_requests.load(Ordering::Relaxed);
        let successful = self.successful_requests.load(Ordering::Relaxed);
        let failed = self.failed_requests.load(Ordering::Relaxed);

        MetricsSummary {
            total_requests: total,
            successful_requests: successful,
            failed_requests: failed,
            success_rate: if total > 0 {
                (successful as f64 / total as f64) * 100.0
            } else {
                0.0
            },
            avg_rps: total as f64 / duration,
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            latency_p50_us: hist.value_at_quantile(0.50),
            latency_p90_us: hist.value_at_quantile(0.90),
            latency_p99_us: hist.value_at_quantile(0.99),
            latency_p999_us: hist.value_at_quantile(0.999),
            latency_max_us: hist.max(),
            latency_min_us: hist.min(),
            latency_mean_us: hist.mean() as u64,
            duration_secs: duration,
            error_counts: self.error_counts.read().clone(),
        }
    }
}

/// Metrics summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSummary {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub success_rate: f64,
    pub avg_rps: f64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub latency_p50_us: u64,
    pub latency_p90_us: u64,
    pub latency_p99_us: u64,
    pub latency_p999_us: u64,
    pub latency_max_us: u64,
    pub latency_min_us: u64,
    pub latency_mean_us: u64,
    pub duration_secs: f64,
    pub error_counts: HashMap<String, u64>,
}

impl MetricsSummary {
    /// Print formatted report
    pub fn print_report(&self) {
        println!("\n╔══════════════════════════════════════════════════════════════╗");
        println!("║              DATACHAIN ROPE LOAD TEST RESULTS                 ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!(
            "║ Duration:          {:>10.2} seconds                        ║",
            self.duration_secs
        );
        println!(
            "║ Total Requests:    {:>10}                                 ║",
            self.total_requests
        );
        println!(
            "║ Successful:        {:>10}                                 ║",
            self.successful_requests
        );
        println!(
            "║ Failed:            {:>10}                                 ║",
            self.failed_requests
        );
        println!(
            "║ Success Rate:      {:>10.2}%                               ║",
            self.success_rate
        );
        println!(
            "║ Avg RPS:           {:>10.2}                                ║",
            self.avg_rps
        );
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║ LATENCY (microseconds)                                       ║");
        println!(
            "║   p50:             {:>10}                                 ║",
            self.latency_p50_us
        );
        println!(
            "║   p90:             {:>10}                                 ║",
            self.latency_p90_us
        );
        println!(
            "║   p99:             {:>10}                                 ║",
            self.latency_p99_us
        );
        println!(
            "║   p99.9:           {:>10}                                 ║",
            self.latency_p999_us
        );
        println!(
            "║   max:             {:>10}                                 ║",
            self.latency_max_us
        );
        println!(
            "║   mean:            {:>10}                                 ║",
            self.latency_mean_us
        );
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!(
            "║ Bytes Sent:        {:>10}                                 ║",
            self.bytes_sent
        );
        println!(
            "║ Bytes Received:    {:>10}                                 ║",
            self.bytes_received
        );

        if !self.error_counts.is_empty() {
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║ ERRORS                                                       ║");
            for (error_type, count) in &self.error_counts {
                println!("║   {:20}: {:>10}                         ║", error_type, count);
            }
        }

        println!("╚══════════════════════════════════════════════════════════════╝\n");
    }

    /// Check if results meet specification requirements
    pub fn check_spec_requirements(&self) -> SpecCheckResult {
        let mut result = SpecCheckResult {
            passes: true,
            checks: Vec::new(),
        };

        // Check latency p99 < 100ms (100,000 µs)
        let latency_check = self.latency_p99_us < 100_000;
        result.checks.push(SpecCheck {
            name: "Latency p99 < 100ms".to_string(),
            passed: latency_check,
            actual: format!("{}µs", self.latency_p99_us),
            expected: "<100,000µs".to_string(),
        });
        if !latency_check {
            result.passes = false;
        }

        // Check success rate > 99%
        let success_check = self.success_rate > 99.0;
        result.checks.push(SpecCheck {
            name: "Success rate > 99%".to_string(),
            passed: success_check,
            actual: format!("{:.2}%", self.success_rate),
            expected: ">99%".to_string(),
        });
        if !success_check {
            result.passes = false;
        }

        // Check throughput > 100 RPS (for basic test)
        let throughput_check = self.avg_rps > 100.0;
        result.checks.push(SpecCheck {
            name: "Throughput > 100 RPS".to_string(),
            passed: throughput_check,
            actual: format!("{:.2} RPS", self.avg_rps),
            expected: ">100 RPS".to_string(),
        });
        if !throughput_check {
            result.passes = false;
        }

        result
    }
}

/// Specification check result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecCheckResult {
    pub passes: bool,
    pub checks: Vec<SpecCheck>,
}

/// Individual specification check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecCheck {
    pub name: String,
    pub passed: bool,
    pub actual: String,
    pub expected: String,
}

impl SpecCheckResult {
    pub fn print_report(&self) {
        println!("\n═══════════════════════════════════════════════════════════════");
        println!("              SPECIFICATION COMPLIANCE CHECK");
        println!("═══════════════════════════════════════════════════════════════");

        for check in &self.checks {
            let status = if check.passed { "✅ PASS" } else { "❌ FAIL" };
            println!("\n  {} - {}", check.name, status);
            println!("    Actual:   {}", check.actual);
            println!("    Expected: {}", check.expected);
        }

        println!("\n═══════════════════════════════════════════════════════════════");
        if self.passes {
            println!("  OVERALL: ✅ ALL CHECKS PASS");
        } else {
            println!("  OVERALL: ❌ SOME CHECKS FAILED");
        }
        println!("═══════════════════════════════════════════════════════════════\n");
    }
}

// ============================================================================
// LOAD TEST SCENARIOS
// ============================================================================

/// Trait for load test scenarios
#[async_trait]
pub trait LoadTestScenario: Send + Sync {
    /// Scenario name
    fn name(&self) -> &str;

    /// Execute a single request
    async fn execute(&self, client: &reqwest::Client, base_url: &str) -> ScenarioResult;
}

/// Result of a scenario execution
#[derive(Debug)]
pub struct ScenarioResult {
    pub success: bool,
    pub latency_us: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub error: Option<String>,
}

// ============================================================================
// BUILT-IN SCENARIOS
// ============================================================================

/// Health check scenario
pub struct HealthCheckScenario;

#[async_trait]
impl LoadTestScenario for HealthCheckScenario {
    fn name(&self) -> &str {
        "health_check"
    }

    async fn execute(&self, client: &reqwest::Client, base_url: &str) -> ScenarioResult {
        let start = Instant::now();
        let url = format!("{}/api/v1/health", base_url);

        match client.get(&url).send().await {
            Ok(response) => {
                let bytes = response.bytes().await.unwrap_or_default();
                ScenarioResult {
                    success: true,
                    latency_us: start.elapsed().as_micros() as u64,
                    bytes_sent: url.len() as u64,
                    bytes_received: bytes.len() as u64,
                    error: None,
                }
            }
            Err(e) => ScenarioResult {
                success: false,
                latency_us: start.elapsed().as_micros() as u64,
                bytes_sent: url.len() as u64,
                bytes_received: 0,
                error: Some(e.to_string()),
            },
        }
    }
}

/// Strings listing scenario
pub struct ListStringsScenario {
    pub limit: u32,
}

#[async_trait]
impl LoadTestScenario for ListStringsScenario {
    fn name(&self) -> &str {
        "list_strings"
    }

    async fn execute(&self, client: &reqwest::Client, base_url: &str) -> ScenarioResult {
        let start = Instant::now();
        let url = format!("{}/api/v1/strings?limit={}", base_url, self.limit);

        match client.get(&url).send().await {
            Ok(response) => {
                let bytes = response.bytes().await.unwrap_or_default();
                ScenarioResult {
                    success: true,
                    latency_us: start.elapsed().as_micros() as u64,
                    bytes_sent: url.len() as u64,
                    bytes_received: bytes.len() as u64,
                    error: None,
                }
            }
            Err(e) => ScenarioResult {
                success: false,
                latency_us: start.elapsed().as_micros() as u64,
                bytes_sent: url.len() as u64,
                bytes_received: 0,
                error: Some(e.to_string()),
            },
        }
    }
}

/// Stats endpoint scenario
pub struct StatsScenario;

#[async_trait]
impl LoadTestScenario for StatsScenario {
    fn name(&self) -> &str {
        "stats"
    }

    async fn execute(&self, client: &reqwest::Client, base_url: &str) -> ScenarioResult {
        let start = Instant::now();
        let url = format!("{}/api/v1/stats", base_url);

        match client.get(&url).send().await {
            Ok(response) => {
                let bytes = response.bytes().await.unwrap_or_default();
                ScenarioResult {
                    success: true,
                    latency_us: start.elapsed().as_micros() as u64,
                    bytes_sent: url.len() as u64,
                    bytes_received: bytes.len() as u64,
                    error: None,
                }
            }
            Err(e) => ScenarioResult {
                success: false,
                latency_us: start.elapsed().as_micros() as u64,
                bytes_sent: url.len() as u64,
                bytes_received: 0,
                error: Some(e.to_string()),
            },
        }
    }
}

/// Projects listing scenario
pub struct ListProjectsScenario;

#[async_trait]
impl LoadTestScenario for ListProjectsScenario {
    fn name(&self) -> &str {
        "list_projects"
    }

    async fn execute(&self, client: &reqwest::Client, base_url: &str) -> ScenarioResult {
        let start = Instant::now();
        let url = format!("{}/api/v1/projects", base_url);

        match client.get(&url).send().await {
            Ok(response) => {
                let bytes = response.bytes().await.unwrap_or_default();
                ScenarioResult {
                    success: true,
                    latency_us: start.elapsed().as_micros() as u64,
                    bytes_sent: url.len() as u64,
                    bytes_received: bytes.len() as u64,
                    error: None,
                }
            }
            Err(e) => ScenarioResult {
                success: false,
                latency_us: start.elapsed().as_micros() as u64,
                bytes_sent: url.len() as u64,
                bytes_received: 0,
                error: Some(e.to_string()),
            },
        }
    }
}

/// Mixed workload scenario (simulates realistic traffic)
pub struct MixedWorkloadScenario;

#[async_trait]
impl LoadTestScenario for MixedWorkloadScenario {
    fn name(&self) -> &str {
        "mixed_workload"
    }

    async fn execute(&self, client: &reqwest::Client, base_url: &str) -> ScenarioResult {
        // Randomly select an endpoint based on realistic distribution
        let random: f64 = rand::random();
        let endpoint = if random < 0.4 {
            "/api/v1/strings?limit=10"
        } else if random < 0.7 {
            "/api/v1/stats"
        } else if random < 0.85 {
            "/api/v1/projects"
        } else if random < 0.95 {
            "/api/v1/health"
        } else {
            "/api/v1/federations"
        };

        let start = Instant::now();
        let url = format!("{}{}", base_url, endpoint);

        match client.get(&url).send().await {
            Ok(response) => {
                let bytes = response.bytes().await.unwrap_or_default();
                ScenarioResult {
                    success: true,
                    latency_us: start.elapsed().as_micros() as u64,
                    bytes_sent: url.len() as u64,
                    bytes_received: bytes.len() as u64,
                    error: None,
                }
            }
            Err(e) => ScenarioResult {
                success: false,
                latency_us: start.elapsed().as_micros() as u64,
                bytes_sent: url.len() as u64,
                bytes_received: 0,
                error: Some(e.to_string()),
            },
        }
    }
}

// ============================================================================
// LOAD TEST RUNNER
// ============================================================================

/// Main load test runner
pub struct LoadTestRunner {
    config: LoadTestConfig,
    metrics: Arc<LoadTestMetrics>,
    scenarios: Vec<Arc<dyn LoadTestScenario>>,
}

impl LoadTestRunner {
    /// Create a new load test runner
    pub fn new(config: LoadTestConfig) -> Self {
        Self {
            config,
            metrics: Arc::new(LoadTestMetrics::new()),
            scenarios: Vec::new(),
        }
    }

    /// Add a scenario
    pub fn add_scenario<S: LoadTestScenario + 'static>(&mut self, scenario: S) {
        self.scenarios.push(Arc::new(scenario));
    }

    /// Add default scenarios
    pub fn add_default_scenarios(&mut self) {
        self.add_scenario(HealthCheckScenario);
        self.add_scenario(ListStringsScenario { limit: 10 });
        self.add_scenario(StatsScenario);
        self.add_scenario(ListProjectsScenario);
        self.add_scenario(MixedWorkloadScenario);
    }

    /// Run the load test
    pub async fn run(&self) -> MetricsSummary {
        info!(
            "Starting load test against {} for {}s at {} RPS",
            self.config.target_url, self.config.duration_secs, self.config.target_rps
        );

        *self.metrics.start_time.write() = Some(Instant::now());

        // Create HTTP client
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.config.request_timeout_secs))
            .pool_max_idle_per_host(self.config.max_concurrency)
            .build()
            .expect("Failed to create HTTP client");

        // Create semaphore for concurrency control
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrency));

        // Warmup phase
        if self.config.warmup_secs > 0 {
            info!("Warmup phase: {}s", self.config.warmup_secs);
            self.run_phase(&client, &semaphore, self.config.warmup_secs, 10)
                .await;
            info!("Warmup complete");
        }

        // Ramp-up phase
        if self.config.ramp_up_secs > 0 {
            info!("Ramp-up phase: {}s", self.config.ramp_up_secs);
            let steps = 5;
            let step_duration = self.config.ramp_up_secs / steps;
            let rps_step = self.config.target_rps / steps;

            for i in 1..=steps {
                let current_rps = rps_step * i;
                info!("Ramp-up step {}/{}: {} RPS", i, steps, current_rps);
                self.run_phase(&client, &semaphore, step_duration, current_rps)
                    .await;
            }
            info!("Ramp-up complete");
        }

        // Main test phase
        info!(
            "Main test phase: {}s at {} RPS",
            self.config.duration_secs, self.config.target_rps
        );
        self.run_phase(
            &client,
            &semaphore,
            self.config.duration_secs,
            self.config.target_rps,
        )
        .await;

        info!("Load test complete");
        self.metrics.summary()
    }

    /// Run a single phase of the test
    async fn run_phase(
        &self,
        client: &reqwest::Client,
        semaphore: &Arc<Semaphore>,
        duration_secs: u64,
        target_rps: u64,
    ) {
        let start = Instant::now();
        let target_duration = Duration::from_secs(duration_secs);
        let interval = Duration::from_secs_f64(1.0 / target_rps as f64);

        let mut tasks = FuturesUnordered::new();
        let mut next_request = Instant::now();

        while start.elapsed() < target_duration {
            // Wait until next request time
            if Instant::now() < next_request {
                sleep(next_request - Instant::now()).await;
            }
            next_request = Instant::now() + interval;

            // Select a random scenario
            let scenario_idx = rand::random::<usize>() % self.scenarios.len().max(1);
            if self.scenarios.is_empty() {
                continue;
            }
            let scenario = self.scenarios[scenario_idx].clone();

            let client = client.clone();
            let semaphore = semaphore.clone();
            let metrics = self.metrics.clone();
            let base_url = self.config.target_url.clone();

            let task = async move {
                let _permit = semaphore.acquire().await.expect("Semaphore closed");

                let result = scenario.execute(&client, &base_url).await;

                if result.success {
                    metrics.record_success(
                        result.latency_us,
                        result.bytes_sent,
                        result.bytes_received,
                    );
                } else {
                    let error_type = result
                        .error
                        .as_ref()
                        .map(|e| e.split(':').next().unwrap_or("unknown"))
                        .unwrap_or("unknown");
                    metrics.record_failure(error_type, result.latency_us);
                }
            };

            tasks.push(task);

            // Process completed tasks
            while let Some(_) = tasks.next().now_or_never() {}
        }

        // Wait for remaining tasks
        while tasks.next().await.is_some() {}
    }

    /// Get current metrics
    pub fn current_metrics(&self) -> MetricsSummary {
        self.metrics.summary()
    }
}

// ============================================================================
// STRESS TEST
// ============================================================================

/// Run a stress test to find breaking point
pub async fn run_stress_test(
    base_url: &str,
    max_rps: u64,
    step_duration_secs: u64,
) -> Vec<(u64, MetricsSummary)> {
    let mut results = Vec::new();
    let step_size = max_rps / 10;

    info!(
        "Starting stress test: ramping from {} to {} RPS",
        step_size, max_rps
    );

    for rps in (step_size..=max_rps).step_by(step_size as usize) {
        info!("Testing at {} RPS...", rps);

        let config = LoadTestConfig {
            target_url: base_url.to_string(),
            duration_secs: step_duration_secs,
            target_rps: rps,
            warmup_secs: 0,
            ramp_up_secs: 0,
            ..Default::default()
        };

        let mut runner = LoadTestRunner::new(config);
        runner.add_default_scenarios();
        let summary = runner.run().await;

        results.push((rps, summary.clone()));

        // Stop if error rate exceeds 5%
        if summary.success_rate < 95.0 {
            warn!(
                "Error rate exceeded 5% at {} RPS. Stopping stress test.",
                rps
            );
            break;
        }

        // Stop if p99 latency exceeds 1 second
        if summary.latency_p99_us > 1_000_000 {
            warn!("P99 latency exceeded 1s at {} RPS. Stopping stress test.", rps);
            break;
        }
    }

    // Print stress test results
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("                    STRESS TEST RESULTS");
    println!("═══════════════════════════════════════════════════════════════");
    println!("{:>10} {:>10} {:>12} {:>12}", "RPS", "Success%", "p99 (µs)", "Avg RPS");
    println!("───────────────────────────────────────────────────────────────");

    for (rps, summary) in &results {
        println!(
            "{:>10} {:>10.2} {:>12} {:>12.2}",
            rps, summary.success_rate, summary.latency_p99_us, summary.avg_rps
        );
    }

    println!("═══════════════════════════════════════════════════════════════\n");

    results
}

// ============================================================================
// SOAK TEST
// ============================================================================

/// Run a soak test for extended duration
pub async fn run_soak_test(
    base_url: &str,
    rps: u64,
    duration_hours: u64,
    checkpoint_interval_mins: u64,
) -> Vec<MetricsSummary> {
    let mut checkpoints = Vec::new();
    let total_mins = duration_hours * 60;
    let num_checkpoints = total_mins / checkpoint_interval_mins;

    info!(
        "Starting soak test: {} hours at {} RPS",
        duration_hours, rps
    );

    for i in 0..num_checkpoints {
        info!(
            "Checkpoint {}/{} ({}min elapsed)",
            i + 1,
            num_checkpoints,
            (i + 1) * checkpoint_interval_mins
        );

        let config = LoadTestConfig {
            target_url: base_url.to_string(),
            duration_secs: checkpoint_interval_mins * 60,
            target_rps: rps,
            warmup_secs: 0,
            ramp_up_secs: 0,
            ..Default::default()
        };

        let mut runner = LoadTestRunner::new(config);
        runner.add_default_scenarios();
        let summary = runner.run().await;

        checkpoints.push(summary.clone());

        // Log checkpoint results
        info!(
            "Checkpoint {}: {:.2}% success, {:.2} RPS, p99: {}µs",
            i + 1,
            summary.success_rate,
            summary.avg_rps,
            summary.latency_p99_us
        );

        // Early termination on degradation
        if summary.success_rate < 90.0 {
            error!("Success rate dropped below 90%. Terminating soak test.");
            break;
        }
    }

    checkpoints
}

