//! Security Monitoring
//!
//! Real-time monitoring and anomaly detection

use super::*;

/// Anomaly detector using statistical analysis
pub struct AnomalyDetector {
    /// Transaction rate baseline
    tx_rate_baseline: RwLock<f64>,
    /// Error rate baseline
    error_rate_baseline: RwLock<f64>,
    /// Detection sensitivity (lower = more sensitive)
    sensitivity: f64,
    /// Alert history
    alerts: RwLock<Vec<SecurityAlert>>,
}

/// Security alert
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityAlert {
    pub id: String,
    pub alert_type: AlertType,
    pub severity: Severity,
    pub message: String,
    pub timestamp: i64,
    pub resolved: bool,
    pub metadata: HashMap<String, String>,
}

/// Alert types
#[derive(Clone, Debug, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub enum AlertType {
    AnomalousTraffic,
    HighErrorRate,
    SuspiciousPattern,
    RateLimitExceeded,
    UnauthorizedAccess,
    MaliciousPayload,
    ReputationDrop,
    ConsensusAnomaly,
}

impl AnomalyDetector {
    pub fn new() -> Self {
        Self {
            tx_rate_baseline: RwLock::new(100.0),   // 100 tx/s default
            error_rate_baseline: RwLock::new(0.01), // 1% default
            sensitivity: 2.0,
            alerts: RwLock::new(Vec::new()),
        }
    }

    /// Update baseline from observations
    pub fn update_baseline(&self, tx_rate: f64, error_rate: f64) {
        // Exponential moving average
        let alpha = 0.1;
        {
            let mut baseline = self.tx_rate_baseline.write();
            *baseline = *baseline * (1.0 - alpha) + tx_rate * alpha;
        }
        {
            let mut baseline = self.error_rate_baseline.write();
            *baseline = *baseline * (1.0 - alpha) + error_rate * alpha;
        }
    }

    /// Detect anomalies in current metrics
    pub fn detect(&self, tx_rate: f64, error_rate: f64) -> Vec<SecurityAlert> {
        let mut alerts = Vec::new();

        let tx_baseline = *self.tx_rate_baseline.read();
        let error_baseline = *self.error_rate_baseline.read();

        // Check for traffic spike
        if tx_rate > tx_baseline * (1.0 + self.sensitivity) {
            alerts.push(SecurityAlert {
                id: format!("ALERT-{}", chrono::Utc::now().timestamp_millis()),
                alert_type: AlertType::AnomalousTraffic,
                severity: Severity::High,
                message: format!(
                    "Traffic spike detected: {} tx/s (baseline: {} tx/s)",
                    tx_rate, tx_baseline
                ),
                timestamp: chrono::Utc::now().timestamp(),
                resolved: false,
                metadata: HashMap::from([
                    ("current_rate".to_string(), tx_rate.to_string()),
                    ("baseline".to_string(), tx_baseline.to_string()),
                ]),
            });
        }

        // Check for traffic drop (potential DoS or network issue)
        if tx_rate < tx_baseline * 0.1 && tx_baseline > 10.0 {
            alerts.push(SecurityAlert {
                id: format!("ALERT-{}", chrono::Utc::now().timestamp_millis()),
                alert_type: AlertType::AnomalousTraffic,
                severity: Severity::Medium,
                message: format!(
                    "Traffic drop detected: {} tx/s (baseline: {} tx/s)",
                    tx_rate, tx_baseline
                ),
                timestamp: chrono::Utc::now().timestamp(),
                resolved: false,
                metadata: HashMap::new(),
            });
        }

        // Check for high error rate
        if error_rate > error_baseline * (1.0 + self.sensitivity) && error_rate > 0.05 {
            alerts.push(SecurityAlert {
                id: format!("ALERT-{}", chrono::Utc::now().timestamp_millis()),
                alert_type: AlertType::HighErrorRate,
                severity: Severity::High,
                message: format!(
                    "High error rate: {:.2}% (baseline: {:.2}%)",
                    error_rate * 100.0,
                    error_baseline * 100.0
                ),
                timestamp: chrono::Utc::now().timestamp(),
                resolved: false,
                metadata: HashMap::new(),
            });
        }

        // Store alerts
        {
            let mut stored = self.alerts.write();
            stored.extend(alerts.clone());
            // Keep last 1000 alerts
            let current_len = stored.len();
            if current_len > 1000 {
                let drain_count = current_len - 1000;
                stored.drain(0..drain_count);
            }
        }

        alerts
    }

    /// Get active (unresolved) alerts
    pub fn active_alerts(&self) -> Vec<SecurityAlert> {
        self.alerts
            .read()
            .iter()
            .filter(|a| !a.resolved)
            .cloned()
            .collect()
    }

    /// Resolve an alert
    pub fn resolve_alert(&self, alert_id: &str) -> bool {
        let mut alerts = self.alerts.write();
        if let Some(alert) = alerts.iter_mut().find(|a| a.id == alert_id) {
            alert.resolved = true;
            return true;
        }
        false
    }

    /// Get alert count by type
    pub fn alert_counts(&self) -> HashMap<AlertType, usize> {
        let alerts = self.alerts.read();
        let mut counts = HashMap::new();

        for alert in alerts.iter() {
            *counts.entry(alert.alert_type.clone()).or_insert(0) += 1;
        }

        counts
    }
}

impl Default for AnomalyDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecurityScanner for AnomalyDetector {
    fn name(&self) -> &str {
        "AnomalyDetector"
    }

    fn supports(&self, target: &ScanTarget) -> bool {
        matches!(target, ScanTarget::NetworkTraffic(_))
    }

    async fn scan(&self, target: &ScanTarget) -> Result<Vec<SecurityFinding>, SecurityError> {
        let events = match target {
            ScanTarget::NetworkTraffic(events) => events,
            _ => return Ok(Vec::new()),
        };

        let mut findings = Vec::new();

        // Analyze traffic patterns
        if events.len() > 10 {
            // Calculate request rate
            let duration = events.last().map(|e| e.timestamp).unwrap_or(0)
                - events.first().map(|e| e.timestamp).unwrap_or(0);

            if duration > 0 {
                let rate = events.len() as f64 / (duration as f64);

                // Check for suspicious IPs (many requests from single source)
                let mut ip_counts: HashMap<&str, usize> = HashMap::new();
                for event in events {
                    *ip_counts.entry(&event.source_ip).or_insert(0) += 1;
                }

                for (ip, count) in ip_counts {
                    let ip_rate = count as f64 / events.len() as f64;
                    if ip_rate > 0.5 && count > 100 {
                        findings.push(SecurityFinding {
                            id: format!("NET-IP-FLOOD-{}", ip.replace('.', "_")),
                            title: "Potential IP Flood".to_string(),
                            description: format!(
                                "IP {} accounts for {:.1}% of traffic ({} requests)",
                                ip,
                                ip_rate * 100.0,
                                count
                            ),
                            severity: Severity::High,
                            category: "denial-of-service".to_string(),
                            location: Some(ip.to_string()),
                            remediation: "Consider rate limiting this IP".to_string(),
                            cwe_id: Some(400),
                            confidence: 0.85,
                            timestamp: chrono::Utc::now().timestamp(),
                            metadata: HashMap::new(),
                        });
                    }
                }

                // Check for unusual ports
                let mut port_counts: HashMap<u16, usize> = HashMap::new();
                for event in events {
                    *port_counts.entry(event.port).or_insert(0) += 1;
                }

                for (port, count) in port_counts {
                    // Check for uncommon ports
                    let common_ports = [80, 443, 8080, 8443, 9000, 9001, 3001];
                    if !common_ports.contains(&port) && count > 10 {
                        findings.push(SecurityFinding {
                            id: format!("NET-UNUSUAL-PORT-{}", port),
                            title: "Unusual Port Activity".to_string(),
                            description: format!(
                                "Traffic on unusual port {} ({} requests)",
                                port, count
                            ),
                            severity: Severity::Low,
                            category: "reconnaissance".to_string(),
                            location: Some(format!("port {}", port)),
                            remediation: "Verify this port should be exposed".to_string(),
                            cwe_id: None,
                            confidence: 0.6,
                            timestamp: chrono::Utc::now().timestamp(),
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
        }

        Ok(findings)
    }
}

/// Rate limit monitor
pub struct RateLimitMonitor {
    /// Requests per IP in current window
    requests: RwLock<HashMap<String, RequestWindow>>,
    /// Window size (seconds)
    window_seconds: u64,
    /// Max requests per window
    max_requests: u64,
}

/// Request window tracking
#[derive(Clone, Debug)]
struct RequestWindow {
    count: u64,
    window_start: i64,
}

impl RateLimitMonitor {
    pub fn new(window_seconds: u64, max_requests: u64) -> Self {
        Self {
            requests: RwLock::new(HashMap::new()),
            window_seconds,
            max_requests,
        }
    }

    /// Check and record a request
    pub fn check_request(&self, ip: &str) -> RateLimitResult {
        let now = chrono::Utc::now().timestamp();
        let mut requests = self.requests.write();

        let window = requests.entry(ip.to_string()).or_insert(RequestWindow {
            count: 0,
            window_start: now,
        });

        // Reset window if expired
        if now - window.window_start >= self.window_seconds as i64 {
            window.count = 0;
            window.window_start = now;
        }

        window.count += 1;

        if window.count > self.max_requests {
            RateLimitResult::Limited {
                current: window.count,
                limit: self.max_requests,
                retry_after: (window.window_start + self.window_seconds as i64 - now) as u64,
            }
        } else {
            RateLimitResult::Allowed {
                remaining: self.max_requests - window.count,
            }
        }
    }

    /// Get current usage for an IP
    pub fn get_usage(&self, ip: &str) -> Option<(u64, u64)> {
        self.requests
            .read()
            .get(ip)
            .map(|w| (w.count, self.max_requests))
    }

    /// Clear expired windows
    pub fn cleanup(&self) {
        let now = chrono::Utc::now().timestamp();
        let mut requests = self.requests.write();
        requests.retain(|_, w| now - w.window_start < self.window_seconds as i64 * 2);
    }
}

/// Rate limit check result
#[derive(Debug, Clone)]
pub enum RateLimitResult {
    Allowed {
        remaining: u64,
    },
    Limited {
        current: u64,
        limit: u64,
        retry_after: u64,
    },
}

impl Default for RateLimitMonitor {
    fn default() -> Self {
        Self::new(60, 100) // 100 requests per minute
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anomaly_detector() {
        let detector = AnomalyDetector::new();

        // Normal conditions
        let alerts = detector.detect(100.0, 0.01);
        assert!(alerts.is_empty());

        // Anomalous conditions
        let alerts = detector.detect(500.0, 0.01);
        assert!(!alerts.is_empty());
        assert!(alerts
            .iter()
            .any(|a| a.alert_type == AlertType::AnomalousTraffic));
    }

    #[test]
    fn test_rate_limit_monitor() {
        let monitor = RateLimitMonitor::new(60, 5);

        // First 5 requests allowed
        for _ in 0..5 {
            let result = monitor.check_request("1.2.3.4");
            assert!(matches!(result, RateLimitResult::Allowed { .. }));
        }

        // 6th request limited
        let result = monitor.check_request("1.2.3.4");
        assert!(matches!(result, RateLimitResult::Limited { .. }));

        // Different IP still allowed
        let result = monitor.check_request("5.6.7.8");
        assert!(matches!(result, RateLimitResult::Allowed { .. }));
    }

    #[tokio::test]
    async fn test_network_traffic_scan() {
        let detector = AnomalyDetector::new();

        let events: Vec<NetworkEvent> = (0..200)
            .map(|i| NetworkEvent {
                timestamp: 1000 + i,
                source_ip: if i < 150 { "1.1.1.1" } else { "2.2.2.2" }.to_string(),
                destination_ip: "3.3.3.3".to_string(),
                port: 443,
                bytes_sent: 1000,
                request_type: "HTTP".to_string(),
            })
            .collect();

        let findings = detector
            .scan(&ScanTarget::NetworkTraffic(events))
            .await
            .unwrap();

        // Should detect the IP flood from 1.1.1.1
        assert!(findings.iter().any(|f| f.category == "denial-of-service"));
    }
}
