//! # Cerber Security AI Agent
//!
//! Automated security scanning, threat detection, and vulnerability assessment
//! for Datachain Rope network and smart contracts.
//!
//! ## Features
//!
//! - **Static Analysis**: Code pattern detection for vulnerabilities
//! - **Dynamic Analysis**: Runtime behavior monitoring
//! - **Anomaly Detection**: ML-based threat identification
//! - **Smart Contract Audit**: Solidity/EVM vulnerability scanning
//! - **Dependency Audit**: Known vulnerability checking
//! - **Reputation Scoring**: Entity trust assessment
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    CERBER SECURITY AGENT                     │
//! ├─────────────────────────────────────────────────────────────┤
//! │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
//! │  │   Static    │  │   Dynamic   │  │     Anomaly         │ │
//! │  │   Analyzer  │  │   Monitor   │  │     Detector        │ │
//! │  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘ │
//! │         │                │                     │            │
//! │         └────────────────┼─────────────────────┘            │
//! │                          ▼                                  │
//! │              ┌───────────────────────┐                      │
//! │              │   Threat Aggregator   │                      │
//! │              └───────────┬───────────┘                      │
//! │                          ▼                                  │
//! │              ┌───────────────────────┐                      │
//! │              │   Risk Assessment     │                      │
//! │              │   & Recommendations   │                      │
//! │              └───────────────────────┘                      │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use async_trait::async_trait;
use parking_lot::RwLock;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;

pub mod analyzer;
pub mod monitor;
pub mod reputation;
pub mod scanner;

// Re-exports
pub use analyzer::*;
pub use monitor::*;
pub use reputation::*;
pub use scanner::*;

/// Security severity levels
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Info => write!(f, "INFO"),
            Severity::Low => write!(f, "LOW"),
            Severity::Medium => write!(f, "MEDIUM"),
            Severity::High => write!(f, "HIGH"),
            Severity::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// Security finding from any scanner
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityFinding {
    /// Unique identifier
    pub id: String,
    /// Finding title
    pub title: String,
    /// Detailed description
    pub description: String,
    /// Severity level
    pub severity: Severity,
    /// Category (reentrancy, overflow, access-control, etc.)
    pub category: String,
    /// Location in code (file:line or contract:function)
    pub location: Option<String>,
    /// Remediation recommendation
    pub remediation: String,
    /// CWE ID if applicable
    pub cwe_id: Option<u32>,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,
    /// Timestamp
    pub timestamp: i64,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Security audit report
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityReport {
    /// Report ID
    pub id: String,
    /// Target (contract address, crate name, etc.)
    pub target: String,
    /// Report timestamp
    pub timestamp: i64,
    /// Duration of scan (ms)
    pub scan_duration_ms: u64,
    /// All findings
    pub findings: Vec<SecurityFinding>,
    /// Summary statistics
    pub summary: ReportSummary,
    /// Overall risk score (0-100)
    pub risk_score: u32,
    /// Passed/failed status
    pub passed: bool,
}

/// Report summary statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReportSummary {
    pub total_findings: usize,
    pub critical_count: usize,
    pub high_count: usize,
    pub medium_count: usize,
    pub low_count: usize,
    pub info_count: usize,
}

impl SecurityReport {
    /// Create new report
    pub fn new(target: &str) -> Self {
        Self {
            id: format!("report-{}", chrono::Utc::now().timestamp()),
            target: target.to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            scan_duration_ms: 0,
            findings: Vec::new(),
            summary: ReportSummary::default(),
            risk_score: 0,
            passed: true,
        }
    }

    /// Add a finding
    pub fn add_finding(&mut self, finding: SecurityFinding) {
        match finding.severity {
            Severity::Critical => self.summary.critical_count += 1,
            Severity::High => self.summary.high_count += 1,
            Severity::Medium => self.summary.medium_count += 1,
            Severity::Low => self.summary.low_count += 1,
            Severity::Info => self.summary.info_count += 1,
        }
        self.summary.total_findings += 1;
        self.findings.push(finding);
    }

    /// Calculate risk score
    pub fn calculate_risk_score(&mut self) {
        // Weighted risk calculation
        let score = self.summary.critical_count as u32 * 40
            + self.summary.high_count as u32 * 20
            + self.summary.medium_count as u32 * 10
            + self.summary.low_count as u32 * 5
            + self.summary.info_count as u32 * 1;

        self.risk_score = score.min(100);
        self.passed = self.summary.critical_count == 0 && self.summary.high_count == 0;
    }

    /// Generate JSON report
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Security scanner trait
#[async_trait]
pub trait SecurityScanner: Send + Sync {
    /// Scanner name
    fn name(&self) -> &str;

    /// Scan and return findings
    async fn scan(&self, target: &ScanTarget) -> Result<Vec<SecurityFinding>, SecurityError>;

    /// Check if scanner supports this target type
    fn supports(&self, target: &ScanTarget) -> bool;
}

/// Scan target types
#[derive(Clone, Debug)]
pub enum ScanTarget {
    /// Smart contract bytecode
    ContractBytecode(Vec<u8>),
    /// Solidity source code
    SoliditySource(String),
    /// Rust source code
    RustSource(String),
    /// Transaction data
    Transaction {
        from: [u8; 20],
        to: Option<[u8; 20]>,
        data: Vec<u8>,
        value: u128,
    },
    /// Entity address for reputation
    Entity([u8; 32]),
    /// Network traffic pattern
    NetworkTraffic(Vec<NetworkEvent>),
}

/// Network event for traffic analysis
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkEvent {
    pub timestamp: i64,
    pub source_ip: String,
    pub destination_ip: String,
    pub port: u16,
    pub bytes_sent: u64,
    pub request_type: String,
}

/// Security errors
#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("Scan failed: {0}")]
    ScanFailed(String),

    #[error("Invalid target: {0}")]
    InvalidTarget(String),

    #[error("Pattern compilation error: {0}")]
    PatternError(String),

    #[error("Analysis timeout")]
    Timeout,

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Cerber Security Agent - Main orchestrator
pub struct CerberAgent {
    /// Registered scanners
    scanners: Vec<Arc<dyn SecurityScanner>>,
    /// Scan history
    history: RwLock<Vec<SecurityReport>>,
    /// Configuration
    config: CerberConfig,
    /// Known vulnerability database
    vuln_db: Arc<VulnerabilityDatabase>,
}

/// Cerber configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CerberConfig {
    /// Maximum scan duration (ms)
    pub max_scan_duration_ms: u64,
    /// Parallel scanner limit
    pub parallel_scanners: usize,
    /// Auto-block critical findings
    pub auto_block_critical: bool,
    /// Report retention count
    pub max_reports: usize,
    /// Minimum confidence threshold
    pub min_confidence: f64,
}

impl Default for CerberConfig {
    fn default() -> Self {
        Self {
            max_scan_duration_ms: 300_000, // 5 minutes
            parallel_scanners: 4,
            auto_block_critical: true,
            max_reports: 1000,
            min_confidence: 0.7,
        }
    }
}

impl CerberAgent {
    /// Create new Cerber agent with default scanners
    pub fn new(config: CerberConfig) -> Self {
        let vuln_db = Arc::new(VulnerabilityDatabase::new());

        let mut agent = Self {
            scanners: Vec::new(),
            history: RwLock::new(Vec::new()),
            config,
            vuln_db: vuln_db.clone(),
        };

        // Register default scanners
        agent.register_scanner(Arc::new(StaticAnalyzer::new()));
        agent.register_scanner(Arc::new(SmartContractScanner::new(vuln_db.clone())));
        agent.register_scanner(Arc::new(DependencyScanner::new()));
        agent.register_scanner(Arc::new(AnomalyDetector::new()));

        agent
    }

    /// Register a scanner
    pub fn register_scanner(&mut self, scanner: Arc<dyn SecurityScanner>) {
        self.scanners.push(scanner);
    }

    /// Run full security scan
    pub async fn scan(&self, target: &ScanTarget) -> Result<SecurityReport, SecurityError> {
        let start = std::time::Instant::now();
        let target_name = match target {
            ScanTarget::ContractBytecode(_) => "contract-bytecode",
            ScanTarget::SoliditySource(_) => "solidity-source",
            ScanTarget::RustSource(_) => "rust-source",
            ScanTarget::Transaction { .. } => "transaction",
            ScanTarget::Entity(_) => "entity",
            ScanTarget::NetworkTraffic(_) => "network-traffic",
        };

        let mut report = SecurityReport::new(target_name);

        // Run all applicable scanners
        for scanner in &self.scanners {
            if scanner.supports(target) {
                match scanner.scan(target).await {
                    Ok(findings) => {
                        for finding in findings {
                            if finding.confidence >= self.config.min_confidence {
                                report.add_finding(finding);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Scanner {} failed: {}", scanner.name(), e);
                    }
                }
            }
        }

        report.scan_duration_ms = start.elapsed().as_millis() as u64;
        report.calculate_risk_score();

        // Store in history
        let mut history = self.history.write();
        history.push(report.clone());
        if history.len() > self.config.max_reports {
            history.remove(0);
        }

        tracing::info!(
            "Security scan complete: {} findings, risk score: {}, passed: {}",
            report.summary.total_findings,
            report.risk_score,
            report.passed
        );

        Ok(report)
    }

    /// Get scan history
    pub fn history(&self) -> Vec<SecurityReport> {
        self.history.read().clone()
    }

    /// Quick check - returns true if target is safe
    pub async fn quick_check(&self, target: &ScanTarget) -> bool {
        match self.scan(target).await {
            Ok(report) => report.passed,
            Err(_) => false,
        }
    }
}

impl Default for CerberAgent {
    fn default() -> Self {
        Self::new(CerberConfig::default())
    }
}

/// Known vulnerability database
pub struct VulnerabilityDatabase {
    /// Vulnerability patterns
    patterns: RwLock<Vec<VulnPattern>>,
    /// Known malicious signatures
    signatures: RwLock<HashSet<String>>,
}

/// Vulnerability pattern
#[derive(Clone, Debug)]
pub struct VulnPattern {
    pub id: String,
    pub name: String,
    pub pattern: Regex,
    pub severity: Severity,
    pub category: String,
    pub description: String,
    pub cwe_id: Option<u32>,
}

impl VulnerabilityDatabase {
    /// Create new database with built-in patterns
    pub fn new() -> Self {
        let db = Self {
            patterns: RwLock::new(Vec::new()),
            signatures: RwLock::new(HashSet::new()),
        };
        db.load_builtin_patterns();
        db
    }

    /// Load built-in vulnerability patterns
    fn load_builtin_patterns(&self) {
        let mut patterns = self.patterns.write();

        // Reentrancy pattern
        if let Ok(re) = Regex::new(r"call\{value:.*\}\(.*\).*\n.*=.*") {
            patterns.push(VulnPattern {
                id: "REENT-001".to_string(),
                name: "Reentrancy Vulnerability".to_string(),
                pattern: re,
                severity: Severity::Critical,
                category: "reentrancy".to_string(),
                description: "External call before state update may allow reentrancy".to_string(),
                cwe_id: Some(841),
            });
        }

        // Integer overflow (pre-0.8.0)
        if let Ok(re) = Regex::new(r"pragma solidity\s*[\^<>=]*\s*0\.[0-7]") {
            patterns.push(VulnPattern {
                id: "OVER-001".to_string(),
                name: "Potential Integer Overflow".to_string(),
                pattern: re,
                severity: Severity::High,
                category: "integer-overflow".to_string(),
                description: "Solidity version < 0.8.0 lacks built-in overflow checks".to_string(),
                cwe_id: Some(190),
            });
        }

        // Unchecked return value
        if let Ok(re) = Regex::new(r"\.call\{.*\}\([^)]*\)\s*;") {
            patterns.push(VulnPattern {
                id: "UNCHECKED-001".to_string(),
                name: "Unchecked Return Value".to_string(),
                pattern: re,
                severity: Severity::Medium,
                category: "unchecked-return".to_string(),
                description: "Low-level call return value not checked".to_string(),
                cwe_id: Some(252),
            });
        }

        // tx.origin usage
        if let Ok(re) = Regex::new(r"tx\.origin") {
            patterns.push(VulnPattern {
                id: "TXORIGIN-001".to_string(),
                name: "tx.origin Authentication".to_string(),
                pattern: re,
                severity: Severity::High,
                category: "access-control".to_string(),
                description: "tx.origin used for authorization is vulnerable to phishing"
                    .to_string(),
                cwe_id: Some(284),
            });
        }

        // Hardcoded private key pattern
        if let Ok(re) = Regex::new(r#"["']0x[a-fA-F0-9]{64}["']"#) {
            patterns.push(VulnPattern {
                id: "SECRET-001".to_string(),
                name: "Hardcoded Private Key".to_string(),
                pattern: re,
                severity: Severity::Critical,
                category: "secrets".to_string(),
                description: "Potential hardcoded private key detected".to_string(),
                cwe_id: Some(798),
            });
        }

        // Selfdestruct vulnerability
        if let Ok(re) = Regex::new(r"selfdestruct\s*\(") {
            patterns.push(VulnPattern {
                id: "DESTRUCT-001".to_string(),
                name: "Selfdestruct Available".to_string(),
                pattern: re,
                severity: Severity::Medium,
                category: "denial-of-service".to_string(),
                description: "Contract can be destroyed, potentially locking funds".to_string(),
                cwe_id: Some(400),
            });
        }

        // Delegatecall vulnerability
        if let Ok(re) = Regex::new(r"delegatecall\s*\(") {
            patterns.push(VulnPattern {
                id: "DELEGATECALL-001".to_string(),
                name: "Delegatecall Usage".to_string(),
                pattern: re,
                severity: Severity::High,
                category: "access-control".to_string(),
                description: "Delegatecall may allow arbitrary code execution".to_string(),
                cwe_id: Some(829),
            });
        }

        // Rust unsafe block
        if let Ok(re) = Regex::new(r"unsafe\s*\{") {
            patterns.push(VulnPattern {
                id: "RUST-UNSAFE-001".to_string(),
                name: "Unsafe Rust Code".to_string(),
                pattern: re,
                severity: Severity::Medium,
                category: "memory-safety".to_string(),
                description: "Unsafe block may bypass Rust's safety guarantees".to_string(),
                cwe_id: Some(119),
            });
        }

        // SQL injection pattern
        if let Ok(re) = Regex::new(r#"format!\s*\([^)]*SELECT.*\{\}"#) {
            patterns.push(VulnPattern {
                id: "SQLI-001".to_string(),
                name: "Potential SQL Injection".to_string(),
                pattern: re,
                severity: Severity::High,
                category: "injection".to_string(),
                description: "String interpolation in SQL query may allow injection".to_string(),
                cwe_id: Some(89),
            });
        }
    }

    /// Get patterns
    pub fn patterns(&self) -> Vec<VulnPattern> {
        self.patterns.read().clone()
    }

    /// Check if signature is known malicious
    pub fn is_malicious(&self, signature: &str) -> bool {
        self.signatures.read().contains(signature)
    }

    /// Add malicious signature
    pub fn add_malicious_signature(&self, signature: String) {
        self.signatures.write().insert(signature);
    }
}

impl Default for VulnerabilityDatabase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Critical > Severity::High);
        assert!(Severity::High > Severity::Medium);
        assert!(Severity::Medium > Severity::Low);
        assert!(Severity::Low > Severity::Info);
    }

    #[test]
    fn test_report_creation() {
        let mut report = SecurityReport::new("test-contract");
        assert_eq!(report.summary.total_findings, 0);
        assert!(report.passed);

        report.add_finding(SecurityFinding {
            id: "TEST-001".to_string(),
            title: "Test Finding".to_string(),
            description: "Test".to_string(),
            severity: Severity::High,
            category: "test".to_string(),
            location: None,
            remediation: "Fix it".to_string(),
            cwe_id: None,
            confidence: 0.9,
            timestamp: chrono::Utc::now().timestamp(),
            metadata: HashMap::new(),
        });

        assert_eq!(report.summary.total_findings, 1);
        assert_eq!(report.summary.high_count, 1);
    }

    #[test]
    fn test_risk_score_calculation() {
        let mut report = SecurityReport::new("test");

        // Add critical finding
        report.add_finding(SecurityFinding {
            id: "CRIT-001".to_string(),
            title: "Critical".to_string(),
            description: "Critical issue".to_string(),
            severity: Severity::Critical,
            category: "test".to_string(),
            location: None,
            remediation: "Fix".to_string(),
            cwe_id: None,
            confidence: 1.0,
            timestamp: 0,
            metadata: HashMap::new(),
        });

        report.calculate_risk_score();
        assert_eq!(report.risk_score, 40);
        assert!(!report.passed);
    }

    #[test]
    fn test_vuln_database() {
        let db = VulnerabilityDatabase::new();
        let patterns = db.patterns();
        assert!(!patterns.is_empty());

        // Check reentrancy pattern exists
        assert!(patterns.iter().any(|p| p.id == "REENT-001"));
    }

    #[tokio::test]
    async fn test_cerber_agent() {
        let agent = CerberAgent::default();

        let source = r#"
            pragma solidity ^0.7.0;
            contract Test {
                function withdraw() public {
                    msg.sender.call{value: balance}("");
                    balance = 0;
                }
            }
        "#;

        let report = agent
            .scan(&ScanTarget::SoliditySource(source.to_string()))
            .await
            .unwrap();

        // Should find at least the old solidity version issue
        assert!(report.summary.total_findings > 0);
    }
}
