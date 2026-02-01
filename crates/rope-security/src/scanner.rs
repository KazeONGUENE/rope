//! Security Scanners
//!
//! Various scanners for different target types

use super::*;

/// Static code analyzer
pub struct StaticAnalyzer {
    /// Custom patterns
    custom_patterns: Vec<VulnPattern>,
}

impl StaticAnalyzer {
    pub fn new() -> Self {
        Self {
            custom_patterns: Vec::new(),
        }
    }

    /// Add custom pattern
    pub fn add_pattern(&mut self, pattern: VulnPattern) {
        self.custom_patterns.push(pattern);
    }
}

impl Default for StaticAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecurityScanner for StaticAnalyzer {
    fn name(&self) -> &str {
        "StaticAnalyzer"
    }

    fn supports(&self, target: &ScanTarget) -> bool {
        matches!(
            target,
            ScanTarget::SoliditySource(_) | ScanTarget::RustSource(_)
        )
    }

    async fn scan(&self, target: &ScanTarget) -> Result<Vec<SecurityFinding>, SecurityError> {
        let source = match target {
            ScanTarget::SoliditySource(s) => s,
            ScanTarget::RustSource(s) => s,
            _ => return Ok(Vec::new()),
        };

        let mut findings = Vec::new();
        let db = VulnerabilityDatabase::new();

        for pattern in db.patterns().iter().chain(self.custom_patterns.iter()) {
            for mat in pattern.pattern.find_iter(source) {
                // Calculate line number
                let line_num = source[..mat.start()].lines().count() + 1;

                findings.push(SecurityFinding {
                    id: format!("{}-{}", pattern.id, findings.len()),
                    title: pattern.name.clone(),
                    description: pattern.description.clone(),
                    severity: pattern.severity,
                    category: pattern.category.clone(),
                    location: Some(format!("line {}", line_num)),
                    remediation: get_remediation(&pattern.category),
                    cwe_id: pattern.cwe_id,
                    confidence: 0.85,
                    timestamp: chrono::Utc::now().timestamp(),
                    metadata: HashMap::new(),
                });
            }
        }

        Ok(findings)
    }
}

/// Smart contract security scanner
pub struct SmartContractScanner {
    vuln_db: Arc<VulnerabilityDatabase>,
}

impl SmartContractScanner {
    pub fn new(vuln_db: Arc<VulnerabilityDatabase>) -> Self {
        Self { vuln_db }
    }

    /// Analyze bytecode for known patterns
    fn analyze_bytecode(&self, bytecode: &[u8]) -> Vec<SecurityFinding> {
        let mut findings = Vec::new();
        let hex_code = hex::encode(bytecode);

        // Check for DELEGATECALL opcode (0xf4)
        if bytecode.contains(&0xf4) {
            findings.push(SecurityFinding {
                id: "BYTECODE-DELEGATECALL".to_string(),
                title: "Delegatecall Detected in Bytecode".to_string(),
                description: "Contract uses delegatecall which may allow code injection"
                    .to_string(),
                severity: Severity::High,
                category: "access-control".to_string(),
                location: None,
                remediation: "Verify delegatecall targets are trusted".to_string(),
                cwe_id: Some(829),
                confidence: 0.9,
                timestamp: chrono::Utc::now().timestamp(),
                metadata: HashMap::new(),
            });
        }

        // Check for SELFDESTRUCT opcode (0xff)
        if bytecode.contains(&0xff) {
            findings.push(SecurityFinding {
                id: "BYTECODE-SELFDESTRUCT".to_string(),
                title: "Selfdestruct Detected in Bytecode".to_string(),
                description: "Contract can be destroyed".to_string(),
                severity: Severity::Medium,
                category: "denial-of-service".to_string(),
                location: None,
                remediation: "Ensure selfdestruct is properly protected".to_string(),
                cwe_id: Some(400),
                confidence: 0.95,
                timestamp: chrono::Utc::now().timestamp(),
                metadata: HashMap::new(),
            });
        }

        // Check for known malicious signatures
        if self
            .vuln_db
            .is_malicious(&hex_code[..64.min(hex_code.len())])
        {
            findings.push(SecurityFinding {
                id: "BYTECODE-MALICIOUS".to_string(),
                title: "Known Malicious Contract".to_string(),
                description: "Contract signature matches known malicious code".to_string(),
                severity: Severity::Critical,
                category: "malware".to_string(),
                location: None,
                remediation: "Do not interact with this contract".to_string(),
                cwe_id: None,
                confidence: 1.0,
                timestamp: chrono::Utc::now().timestamp(),
                metadata: HashMap::new(),
            });
        }

        findings
    }
}

#[async_trait]
impl SecurityScanner for SmartContractScanner {
    fn name(&self) -> &str {
        "SmartContractScanner"
    }

    fn supports(&self, target: &ScanTarget) -> bool {
        matches!(
            target,
            ScanTarget::ContractBytecode(_) | ScanTarget::SoliditySource(_)
        )
    }

    async fn scan(&self, target: &ScanTarget) -> Result<Vec<SecurityFinding>, SecurityError> {
        match target {
            ScanTarget::ContractBytecode(bytecode) => Ok(self.analyze_bytecode(bytecode)),
            ScanTarget::SoliditySource(source) => {
                let mut findings = Vec::new();

                // Check for common vulnerabilities in source
                for pattern in self.vuln_db.patterns() {
                    for mat in pattern.pattern.find_iter(source) {
                        let line_num = source[..mat.start()].lines().count() + 1;

                        findings.push(SecurityFinding {
                            id: format!("SC-{}", pattern.id),
                            title: pattern.name.clone(),
                            description: pattern.description.clone(),
                            severity: pattern.severity,
                            category: pattern.category.clone(),
                            location: Some(format!("line {}", line_num)),
                            remediation: get_remediation(&pattern.category),
                            cwe_id: pattern.cwe_id,
                            confidence: 0.85,
                            timestamp: chrono::Utc::now().timestamp(),
                            metadata: HashMap::new(),
                        });
                    }
                }

                // Additional Solidity-specific checks
                if source.contains("block.timestamp") {
                    findings.push(SecurityFinding {
                        id: "SC-TIMESTAMP".to_string(),
                        title: "Block Timestamp Dependence".to_string(),
                        description: "Contract relies on block.timestamp which can be manipulated"
                            .to_string(),
                        severity: Severity::Low,
                        category: "time-manipulation".to_string(),
                        location: None,
                        remediation: "Avoid using block.timestamp for critical logic".to_string(),
                        cwe_id: Some(829),
                        confidence: 0.7,
                        timestamp: chrono::Utc::now().timestamp(),
                        metadata: HashMap::new(),
                    });
                }

                if source.contains("block.number") && source.contains("random") {
                    findings.push(SecurityFinding {
                        id: "SC-RANDOMNESS".to_string(),
                        title: "Weak Randomness Source".to_string(),
                        description: "Using block.number for randomness is predictable".to_string(),
                        severity: Severity::High,
                        category: "randomness".to_string(),
                        location: None,
                        remediation: "Use Chainlink VRF or similar secure randomness source"
                            .to_string(),
                        cwe_id: Some(330),
                        confidence: 0.8,
                        timestamp: chrono::Utc::now().timestamp(),
                        metadata: HashMap::new(),
                    });
                }

                Ok(findings)
            }
            _ => Ok(Vec::new()),
        }
    }
}

/// Dependency vulnerability scanner
pub struct DependencyScanner {
    /// Known vulnerable crate versions
    vulnerable_crates: HashMap<String, Vec<(String, Severity, String)>>,
}

impl DependencyScanner {
    pub fn new() -> Self {
        let mut vulnerable_crates = HashMap::new();

        // Known vulnerable crate versions (example data)
        vulnerable_crates.insert(
            "hyper".to_string(),
            vec![(
                "<0.14.10".to_string(),
                Severity::High,
                "HTTP request smuggling".to_string(),
            )],
        );

        vulnerable_crates.insert(
            "tokio".to_string(),
            vec![(
                "<1.8.4".to_string(),
                Severity::Medium,
                "Data race in JoinHandle".to_string(),
            )],
        );

        Self { vulnerable_crates }
    }
}

impl Default for DependencyScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecurityScanner for DependencyScanner {
    fn name(&self) -> &str {
        "DependencyScanner"
    }

    fn supports(&self, target: &ScanTarget) -> bool {
        matches!(target, ScanTarget::RustSource(_))
    }

    async fn scan(&self, _target: &ScanTarget) -> Result<Vec<SecurityFinding>, SecurityError> {
        // In a real implementation, this would parse Cargo.lock and check versions
        // For now, return empty as this requires file system access
        Ok(Vec::new())
    }
}

/// Get remediation advice for a category
fn get_remediation(category: &str) -> String {
    match category {
        "reentrancy" => "Use checks-effects-interactions pattern or reentrancy guard".to_string(),
        "integer-overflow" => {
            "Use Solidity 0.8+ or SafeMath library for arithmetic operations".to_string()
        }
        "access-control" => {
            "Implement proper access control with role-based permissions".to_string()
        }
        "unchecked-return" => "Always check return values of low-level calls".to_string(),
        "secrets" => {
            "Remove hardcoded secrets and use environment variables or secure vaults".to_string()
        }
        "denial-of-service" => "Add access controls and consider removing selfdestruct".to_string(),
        "memory-safety" => "Minimize unsafe blocks and document safety invariants".to_string(),
        "injection" => "Use parameterized queries and input validation".to_string(),
        _ => "Review and remediate according to security best practices".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_static_analyzer() {
        let analyzer = StaticAnalyzer::new();

        let source = r#"
            pragma solidity ^0.6.0;
            contract Vulnerable {
                function bad() public {
                    tx.origin;
                }
            }
        "#;

        let findings = analyzer
            .scan(&ScanTarget::SoliditySource(source.to_string()))
            .await
            .unwrap();

        // Should find tx.origin and old solidity version
        assert!(findings.len() >= 2);
    }

    #[tokio::test]
    async fn test_contract_scanner_bytecode() {
        let db = Arc::new(VulnerabilityDatabase::new());
        let scanner = SmartContractScanner::new(db);

        // Bytecode with SELFDESTRUCT opcode
        let bytecode = vec![0x60, 0x00, 0xff, 0x00];

        let findings = scanner
            .scan(&ScanTarget::ContractBytecode(bytecode))
            .await
            .unwrap();

        assert!(findings.iter().any(|f| f.id == "BYTECODE-SELFDESTRUCT"));
    }
}
