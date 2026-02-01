//! Security Analyzers
//!
//! Deep analysis modules for security assessment

use super::*;

/// Transaction analyzer for detecting malicious patterns
pub struct TransactionAnalyzer {
    /// Known malicious addresses
    blacklist: RwLock<HashSet<[u8; 20]>>,
    /// Suspicious patterns
    patterns: Vec<TxPattern>,
}

/// Transaction pattern for detection
#[derive(Clone, Debug)]
pub struct TxPattern {
    pub name: String,
    pub data_pattern: Option<Vec<u8>>,
    pub value_threshold: Option<u128>,
    pub severity: Severity,
}

impl TransactionAnalyzer {
    pub fn new() -> Self {
        let mut patterns = Vec::new();

        // Flash loan pattern (large value, immediate return)
        patterns.push(TxPattern {
            name: "Potential Flash Loan".to_string(),
            data_pattern: None,
            value_threshold: Some(1_000_000_000_000_000_000_000), // 1000 ETH
            severity: Severity::Info,
        });

        Self {
            blacklist: RwLock::new(HashSet::new()),
            patterns,
        }
    }

    /// Add address to blacklist
    pub fn blacklist_address(&self, address: [u8; 20]) {
        self.blacklist.write().insert(address);
    }

    /// Check if address is blacklisted
    pub fn is_blacklisted(&self, address: &[u8; 20]) -> bool {
        self.blacklist.read().contains(address)
    }

    /// Analyze transaction
    pub fn analyze(
        &self,
        from: &[u8; 20],
        to: Option<&[u8; 20]>,
        data: &[u8],
        value: u128,
    ) -> Vec<SecurityFinding> {
        let mut findings = Vec::new();

        // Check blacklist
        if self.is_blacklisted(from) {
            findings.push(SecurityFinding {
                id: "TX-BLACKLIST-FROM".to_string(),
                title: "Transaction from Blacklisted Address".to_string(),
                description: format!("Sender {} is blacklisted", hex::encode(from)),
                severity: Severity::Critical,
                category: "blacklist".to_string(),
                location: None,
                remediation: "Block this transaction".to_string(),
                cwe_id: None,
                confidence: 1.0,
                timestamp: chrono::Utc::now().timestamp(),
                metadata: HashMap::new(),
            });
        }

        if let Some(to_addr) = to {
            if self.is_blacklisted(to_addr) {
                findings.push(SecurityFinding {
                    id: "TX-BLACKLIST-TO".to_string(),
                    title: "Transaction to Blacklisted Address".to_string(),
                    description: format!("Recipient {} is blacklisted", hex::encode(to_addr)),
                    severity: Severity::Critical,
                    category: "blacklist".to_string(),
                    location: None,
                    remediation: "Block this transaction".to_string(),
                    cwe_id: None,
                    confidence: 1.0,
                    timestamp: chrono::Utc::now().timestamp(),
                    metadata: HashMap::new(),
                });
            }
        }

        // Check patterns
        for pattern in &self.patterns {
            let mut matched = false;

            if let Some(threshold) = pattern.value_threshold {
                if value >= threshold {
                    matched = true;
                }
            }

            if let Some(ref data_pattern) = pattern.data_pattern {
                if data.windows(data_pattern.len()).any(|w| w == data_pattern) {
                    matched = true;
                }
            }

            if matched {
                findings.push(SecurityFinding {
                    id: format!(
                        "TX-PATTERN-{}",
                        pattern.name.to_uppercase().replace(' ', "-")
                    ),
                    title: pattern.name.clone(),
                    description: format!("Transaction matches pattern: {}", pattern.name),
                    severity: pattern.severity,
                    category: "pattern-match".to_string(),
                    location: None,
                    remediation: "Review transaction details".to_string(),
                    cwe_id: None,
                    confidence: 0.8,
                    timestamp: chrono::Utc::now().timestamp(),
                    metadata: HashMap::new(),
                });
            }
        }

        // Check for suspicious data patterns
        if !data.is_empty() {
            // Check for potential selector collision
            if data.len() >= 4 {
                let selector = &data[0..4];
                // Known dangerous selectors
                let dangerous_selectors: Vec<[u8; 4]> = vec![
                    [0xa9, 0x05, 0x9c, 0xbb], // transfer
                    [0x09, 0x5e, 0xa7, 0xb3], // approve
                ];

                for dangerous in dangerous_selectors {
                    if selector == dangerous {
                        findings.push(SecurityFinding {
                            id: "TX-DANGEROUS-SELECTOR".to_string(),
                            title: "Token Operation Detected".to_string(),
                            description:
                                "Transaction contains token transfer or approval operation"
                                    .to_string(),
                            severity: Severity::Info,
                            category: "token-operation".to_string(),
                            location: None,
                            remediation: "Verify the operation is intended".to_string(),
                            cwe_id: None,
                            confidence: 1.0,
                            timestamp: chrono::Utc::now().timestamp(),
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
        }

        findings
    }
}

impl Default for TransactionAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Gas usage analyzer
pub struct GasAnalyzer {
    /// Historical gas usage
    history: RwLock<Vec<u64>>,
    /// Threshold multiplier for anomaly
    anomaly_threshold: f64,
}

impl GasAnalyzer {
    pub fn new() -> Self {
        Self {
            history: RwLock::new(Vec::new()),
            anomaly_threshold: 3.0, // 3 standard deviations
        }
    }

    /// Record gas usage
    pub fn record(&self, gas: u64) {
        let mut history = self.history.write();
        history.push(gas);
        // Keep last 1000 entries
        if history.len() > 1000 {
            history.remove(0);
        }
    }

    /// Check if gas usage is anomalous
    pub fn is_anomalous(&self, gas: u64) -> bool {
        let history = self.history.read();
        if history.len() < 10 {
            return false;
        }

        let mean: f64 = history.iter().map(|&x| x as f64).sum::<f64>() / history.len() as f64;
        let variance: f64 = history
            .iter()
            .map(|&x| (x as f64 - mean).powi(2))
            .sum::<f64>()
            / history.len() as f64;
        let std_dev = variance.sqrt();

        (gas as f64 - mean).abs() > self.anomaly_threshold * std_dev
    }

    /// Get average gas
    pub fn average(&self) -> u64 {
        let history = self.history.read();
        if history.is_empty() {
            return 0;
        }
        (history.iter().sum::<u64>() as f64 / history.len() as f64) as u64
    }
}

impl Default for GasAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Code complexity analyzer
pub struct ComplexityAnalyzer;

impl ComplexityAnalyzer {
    /// Calculate cyclomatic complexity (simplified)
    pub fn cyclomatic_complexity(source: &str) -> u32 {
        let mut complexity: u32 = 1;

        // Count decision points
        let decision_keywords = [
            "if", "else", "while", "for", "case", "catch", "&&", "||", "?",
        ];

        for keyword in decision_keywords {
            complexity += source.matches(keyword).count() as u32;
        }

        complexity
    }

    /// Analyze and return findings
    pub fn analyze(source: &str) -> Vec<SecurityFinding> {
        let mut findings = Vec::new();
        let complexity = Self::cyclomatic_complexity(source);

        if complexity > 50 {
            findings.push(SecurityFinding {
                id: "COMPLEXITY-HIGH".to_string(),
                title: "High Cyclomatic Complexity".to_string(),
                description: format!(
                    "Code complexity is {} which may indicate maintainability issues",
                    complexity
                ),
                severity: Severity::Medium,
                category: "maintainability".to_string(),
                location: None,
                remediation: "Consider refactoring complex functions into smaller units"
                    .to_string(),
                cwe_id: Some(1121),
                confidence: 0.9,
                timestamp: chrono::Utc::now().timestamp(),
                metadata: HashMap::from([("complexity".to_string(), complexity.to_string())]),
            });
        } else if complexity > 20 {
            findings.push(SecurityFinding {
                id: "COMPLEXITY-MODERATE".to_string(),
                title: "Moderate Code Complexity".to_string(),
                description: format!("Code complexity is {}", complexity),
                severity: Severity::Low,
                category: "maintainability".to_string(),
                location: None,
                remediation: "Consider simplifying complex logic".to_string(),
                cwe_id: None,
                confidence: 0.8,
                timestamp: chrono::Utc::now().timestamp(),
                metadata: HashMap::from([("complexity".to_string(), complexity.to_string())]),
            });
        }

        // Check for deep nesting
        let max_nesting = Self::max_nesting_depth(source);
        if max_nesting > 5 {
            findings.push(SecurityFinding {
                id: "NESTING-DEEP".to_string(),
                title: "Deep Nesting Detected".to_string(),
                description: format!("Maximum nesting depth is {}", max_nesting),
                severity: Severity::Low,
                category: "maintainability".to_string(),
                location: None,
                remediation: "Reduce nesting with early returns or extraction".to_string(),
                cwe_id: None,
                confidence: 0.9,
                timestamp: chrono::Utc::now().timestamp(),
                metadata: HashMap::new(),
            });
        }

        findings
    }

    /// Calculate maximum nesting depth
    fn max_nesting_depth(source: &str) -> u32 {
        let mut max_depth: u32 = 0;
        let mut current_depth: u32 = 0;

        for ch in source.chars() {
            match ch {
                '{' => {
                    current_depth += 1;
                    max_depth = max_depth.max(current_depth);
                }
                '}' => {
                    current_depth = current_depth.saturating_sub(1);
                }
                _ => {}
            }
        }

        max_depth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_analyzer_blacklist() {
        let analyzer = TransactionAnalyzer::new();
        let addr = [1u8; 20];

        assert!(!analyzer.is_blacklisted(&addr));
        analyzer.blacklist_address(addr);
        assert!(analyzer.is_blacklisted(&addr));
    }

    #[test]
    fn test_gas_analyzer_anomaly() {
        let analyzer = GasAnalyzer::new();

        // Record normal values
        for i in 0..100 {
            analyzer.record(21000 + (i * 100));
        }

        // Normal value should not be anomalous
        assert!(!analyzer.is_anomalous(25000));

        // Very high value should be anomalous
        assert!(analyzer.is_anomalous(1_000_000));
    }

    #[test]
    fn test_complexity_analyzer() {
        let simple = "function foo() { return 1; }";
        assert!(ComplexityAnalyzer::cyclomatic_complexity(simple) < 5);

        let complex = r#"
            function bar() {
                if (a) {
                    if (b) {
                        for (i = 0; i < 10; i++) {
                            if (c || d && e) {
                                while (f) {
                                    // ...
                                }
                            }
                        }
                    }
                }
            }
        "#;
        assert!(ComplexityAnalyzer::cyclomatic_complexity(complex) > 5);
    }

    #[test]
    fn test_nesting_depth() {
        let code = "{ { { } } }";
        assert_eq!(ComplexityAnalyzer::max_nesting_depth(code), 3);
    }
}
