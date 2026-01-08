//! # Datachain Rope Load Test CLI
//!
//! Command-line interface for running load tests against Datachain Rope.
//!
//! ## Usage
//!
//! ```bash
//! # Basic load test
//! rope-loadtest --target https://dcscan.io --duration 60 --rps 100
//!
//! # Stress test to find breaking point
//! rope-loadtest stress --target https://dcscan.io --max-rps 1000
//!
//! # Soak test for extended duration
//! rope-loadtest soak --target https://dcscan.io --duration-hours 1 --rps 50
//! ```

use clap::{Parser, Subcommand};
use rope_loadtest::*;
use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Parser)]
#[command(name = "rope-loadtest")]
#[command(author = "Datachain Rope Team")]
#[command(version = "1.0.0")]
#[command(about = "Load testing tool for Datachain Rope", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Target base URL
    #[arg(short, long, default_value = "https://dcscan.io")]
    target: String,

    /// Test duration in seconds
    #[arg(short, long, default_value = "60")]
    duration: u64,

    /// Target requests per second
    #[arg(short, long, default_value = "100")]
    rps: u64,

    /// Maximum concurrent requests
    #[arg(short, long, default_value = "50")]
    concurrency: usize,

    /// Warmup duration in seconds
    #[arg(long, default_value = "5")]
    warmup: u64,

    /// Ramp-up duration in seconds
    #[arg(long, default_value = "10")]
    ramp_up: u64,

    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Output results to JSON file
    #[arg(short, long)]
    output: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a basic load test
    Basic {
        /// Target base URL
        #[arg(short, long, default_value = "https://dcscan.io")]
        target: String,

        /// Test duration in seconds
        #[arg(short, long, default_value = "60")]
        duration: u64,

        /// Target requests per second
        #[arg(short, long, default_value = "100")]
        rps: u64,
    },

    /// Run a stress test to find breaking point
    Stress {
        /// Target base URL
        #[arg(short, long, default_value = "https://dcscan.io")]
        target: String,

        /// Maximum RPS to test
        #[arg(short, long, default_value = "1000")]
        max_rps: u64,

        /// Duration per step in seconds
        #[arg(short, long, default_value = "30")]
        step_duration: u64,
    },

    /// Run a soak test for extended duration
    Soak {
        /// Target base URL
        #[arg(short, long, default_value = "https://dcscan.io")]
        target: String,

        /// Test duration in hours
        #[arg(long, default_value = "1")]
        duration_hours: u64,

        /// Target requests per second
        #[arg(short, long, default_value = "50")]
        rps: u64,

        /// Checkpoint interval in minutes
        #[arg(long, default_value = "10")]
        checkpoint_interval: u64,
    },

    /// Run specification compliance test
    SpecCheck {
        /// Target base URL
        #[arg(short, long, default_value = "https://dcscan.io")]
        target: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Setup tracing
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    info!("Datachain Rope Load Test Tool v1.0.0");

    match cli.command {
        Some(Commands::Basic { target, duration, rps }) => {
            run_basic_test(&target, duration, rps).await;
        }
        Some(Commands::Stress { target, max_rps, step_duration }) => {
            run_stress_test(&target, max_rps, step_duration).await;
        }
        Some(Commands::Soak { target, duration_hours, rps, checkpoint_interval }) => {
            run_soak_test(&target, rps, duration_hours, checkpoint_interval).await;
        }
        Some(Commands::SpecCheck { target }) => {
            run_spec_check(&target).await;
        }
        None => {
            // Run default load test with CLI args
            let config = LoadTestConfig {
                target_url: cli.target,
                duration_secs: cli.duration,
                target_rps: cli.rps,
                max_concurrency: cli.concurrency,
                warmup_secs: cli.warmup,
                ramp_up_secs: cli.ramp_up,
                verbose: cli.verbose,
                ..Default::default()
            };

            let mut runner = LoadTestRunner::new(config);
            runner.add_default_scenarios();

            let summary = runner.run().await;
            summary.print_report();

            // Check spec compliance
            let spec_result = summary.check_spec_requirements();
            spec_result.print_report();

            // Output JSON if requested
            if let Some(output_path) = cli.output {
                let json = serde_json::to_string_pretty(&summary).expect("Failed to serialize");
                std::fs::write(&output_path, json).expect("Failed to write output file");
                info!("Results saved to {}", output_path);
            }

            // Exit with appropriate code
            if spec_result.passes {
                std::process::exit(0);
            } else {
                std::process::exit(1);
            }
        }
    }
}

async fn run_basic_test(target: &str, duration: u64, rps: u64) {
    let config = LoadTestConfig {
        target_url: target.to_string(),
        duration_secs: duration,
        target_rps: rps,
        ..Default::default()
    };

    let mut runner = LoadTestRunner::new(config);
    runner.add_default_scenarios();

    let summary = runner.run().await;
    summary.print_report();

    let spec_result = summary.check_spec_requirements();
    spec_result.print_report();
}

async fn run_spec_check(target: &str) {
    info!("Running specification compliance check against {}", target);

    // Run a moderate load test
    let config = LoadTestConfig {
        target_url: target.to_string(),
        duration_secs: 30,
        target_rps: 100,
        warmup_secs: 5,
        ramp_up_secs: 5,
        ..Default::default()
    };

    let mut runner = LoadTestRunner::new(config);
    runner.add_default_scenarios();

    let summary = runner.run().await;
    summary.print_report();

    let spec_result = summary.check_spec_requirements();
    spec_result.print_report();

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║              SPECIFICATION REQUIREMENTS (§8.2)                ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ String Creation Time p99 < 100ms                             ║");
    println!("║ Testimony Finality < 3s                                      ║");
    println!("║ Network Throughput > 10,000 TPS (full system)                ║");
    println!("║ Memory per String < 1KB overhead                             ║");
    println!("║ OES Key Generation < 50ms                                    ║");
    println!("║ Dilithium3 Signing < 5ms                                     ║");
    println!("║ Kyber768 Encapsulation < 2ms                                 ║");
    println!("║ Reed-Solomon Encode < 10ms/MB                                ║");
    println!("║ Virtual Voting < 50ms per round                              ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    if spec_result.passes {
        println!("✅ API load test PASSES specification requirements");
        std::process::exit(0);
    } else {
        println!("❌ API load test FAILS some specification requirements");
        std::process::exit(1);
    }
}

