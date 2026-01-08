//! # Regeneration Protocol
//! 
//! DNA-inspired repair mechanisms for the String Lattice.
//! Implements multiple repair strategies analogous to DNA repair:
//! 
//! | DNA Repair | Rope Analog | Description |
//! |------------|-------------|-------------|
//! | BER (Base Excision Repair) | SingleNucleotide | Repair 1-32 byte errors |
//! | NER (Nucleotide Excision Repair) | SegmentRepair | Repair segment corruption |
//! | MMR (Mismatch Repair) | MismatchRepair | Fix hash verification errors |
//! | DSB (Double-Strand Break Repair) | SevereRepair | Handle major data loss |
//! | Recombination | FullRegeneration | Complete string reconstruction |
//! 
//! ## Repair Flow
//! 
//! ```text
//! Damage Detected → Classify → Select Strategy → Request Data → Repair → Verify
//!       ↓
//! [SingleNucleotide] → Use Reed-Solomon parity from complement
//! [SegmentCorruption] → Request segment from multiple peers
//! [MismatchError] → Request full content, compare, merge
//! [SevereCorruption] → Multi-source reconstruction
//! [TotalLoss] → Full regeneration from complement + peers
//! ```
//!
//! ## Damage Detection
//! 
//! The protocol includes active damage detection:
//! - **Checksum Verification**: Per-segment checksums for localized detection
//! - **Complement Comparison**: Compare primary and complement strands
//! - **Periodic Scanning**: Background integrity scans
//! - **Access-Time Detection**: Detect corruption on read operations

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use parking_lot::RwLock;

/// Damage type detected in a string
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DamageType {
    /// Single nucleotide error (1-32 bytes)
    SingleNucleotide { offset: usize, expected_hash: [u8; 32] },
    
    /// Segment corruption (multiple nucleotides)
    SegmentCorruption { start: usize, end: usize, severity_percent: u8 },
    
    /// Hash mismatch error
    MismatchError { computed: [u8; 32], expected: [u8; 32] },
    
    /// Severe corruption (>50% data lost)
    SevereCorruption { recovery_chance_percent: u8 },
    
    /// Complete loss (need full regeneration)
    TotalLoss,
    
    /// Complement mismatch (primary-complement desync)
    ComplementDesync,
}

impl DamageType {
    /// Get severity score (0-100)
    pub fn severity(&self) -> u8 {
        match self {
            DamageType::SingleNucleotide { .. } => 10,
            DamageType::SegmentCorruption { severity_percent, .. } => *severity_percent,
            DamageType::MismatchError { .. } => 50,
            DamageType::SevereCorruption { recovery_chance_percent } => 100 - recovery_chance_percent,
            DamageType::TotalLoss => 100,
            DamageType::ComplementDesync => 30,
        }
    }
    
    /// Get recommended repair strategy
    pub fn recommended_strategy(&self) -> RepairStrategy {
        match self {
            DamageType::SingleNucleotide { .. } => RepairStrategy::ParityReconstruction,
            DamageType::SegmentCorruption { .. } => RepairStrategy::SegmentRequest,
            DamageType::MismatchError { .. } => RepairStrategy::MultiSourceVerify,
            DamageType::SevereCorruption { .. } => RepairStrategy::MultiSourceReconstruct,
            DamageType::TotalLoss => RepairStrategy::FullRegeneration,
            DamageType::ComplementDesync => RepairStrategy::ComplementSync,
        }
    }
}

/// Repair strategy
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepairStrategy {
    /// Use Reed-Solomon parity from complement
    ParityReconstruction,
    
    /// Request specific segment from peers
    SegmentRequest,
    
    /// Get data from multiple sources and verify
    MultiSourceVerify,
    
    /// Reconstruct from multiple partial sources
    MultiSourceReconstruct,
    
    /// Full regeneration from complement + peers
    FullRegeneration,
    
    /// Sync primary and complement
    ComplementSync,
}

/// Repair request
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepairRequest {
    /// Request ID
    pub id: [u8; 32],
    
    /// String to repair
    pub string_id: [u8; 32],
    
    /// Detected damage type
    pub damage_type: DamageType,
    
    /// Selected strategy
    pub strategy: RepairStrategy,
    
    /// Requester node ID
    pub requester_id: [u8; 32],
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Priority (0-100)
    pub priority: u8,
    
    /// Retry count
    pub retry_count: u32,
}

impl RepairRequest {
    /// Create new repair request
    pub fn new(string_id: [u8; 32], damage_type: DamageType, requester_id: [u8; 32]) -> Self {
        let strategy = damage_type.recommended_strategy();
        let priority = damage_type.severity();
        let timestamp = chrono::Utc::now().timestamp();
        
        let mut id_data = string_id.to_vec();
        id_data.extend_from_slice(&requester_id);
        id_data.extend_from_slice(&timestamp.to_le_bytes());
        let id = *blake3::hash(&id_data).as_bytes();
        
        Self {
            id,
            string_id,
            damage_type,
            strategy,
            requester_id,
            timestamp,
            priority,
            retry_count: 0,
        }
    }
}

/// Repair response from a peer
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepairResponse {
    /// Request ID this responds to
    pub request_id: [u8; 32],
    
    /// String ID
    pub string_id: [u8; 32],
    
    /// Repair data (content or segment)
    pub repair_data: Vec<u8>,
    
    /// Provider node ID
    pub provider_id: [u8; 32],
    
    /// Content hash for verification
    pub content_hash: [u8; 32],
    
    /// Provider's signature
    pub signature: Vec<u8>,
    
    /// Timestamp
    pub timestamp: i64,
}

/// Repair result
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RepairResult {
    /// Successfully repaired
    Success {
        string_id: [u8; 32],
        repaired_bytes: usize,
        sources_used: usize,
    },
    
    /// Partial repair (some data still missing)
    Partial {
        string_id: [u8; 32],
        completion_percentage: f64,
        missing_ranges: Vec<(usize, usize)>,
    },
    
    /// Repair failed
    Failed {
        string_id: [u8; 32],
        reason: String,
    },
    
    /// String is unrecoverable
    Unrecoverable {
        string_id: [u8; 32],
        reason: String,
    },
}

/// Regeneration coordinator
pub struct RegenerationCoordinator {
    /// Pending repair requests
    pending_repairs: RwLock<HashMap<[u8; 32], RepairRequest>>,
    
    /// Responses received
    responses: RwLock<HashMap<[u8; 32], Vec<RepairResponse>>>,
    
    /// Completed repairs
    completed: RwLock<Vec<RepairResult>>,
    
    /// Node ID (for requests)
    node_id: [u8; 32],
    
    /// Known providers for strings
    providers: RwLock<HashMap<[u8; 32], HashSet<[u8; 32]>>>,
    
    /// Statistics
    stats: RwLock<RegenerationStats>,
}

/// Regeneration statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RegenerationStats {
    pub total_repairs: u64,
    pub successful_repairs: u64,
    pub failed_repairs: u64,
    pub partial_repairs: u64,
    pub bytes_recovered: u64,
    pub avg_sources_used: f64,
    pub avg_repair_time_ms: f64,
}

impl RegenerationCoordinator {
    /// Create new coordinator
    pub fn new(node_id: [u8; 32]) -> Self {
        Self {
            pending_repairs: RwLock::new(HashMap::new()),
            responses: RwLock::new(HashMap::new()),
            completed: RwLock::new(Vec::new()),
            node_id,
            providers: RwLock::new(HashMap::new()),
            stats: RwLock::new(RegenerationStats::default()),
        }
    }
    
    /// Detect damage in a string
    pub fn detect_damage(&self, content: &[u8], expected_hash: &[u8; 32]) -> Option<DamageType> {
        // Check hash
        let computed = *blake3::hash(content).as_bytes();
        if computed != *expected_hash {
            // Determine severity
            if content.is_empty() {
                return Some(DamageType::TotalLoss);
            }
            
            // Check for severe corruption
            let zero_bytes = content.iter().filter(|&&b| b == 0).count();
            let corruption_percent = (zero_bytes * 100 / content.len()) as u8;
            
            if corruption_percent > 50 {
                return Some(DamageType::SevereCorruption {
                    recovery_chance_percent: 100 - corruption_percent,
                });
            }
            
            return Some(DamageType::MismatchError {
                computed,
                expected: *expected_hash,
            });
        }
        
        None
    }
    
    /// Request repair for a string
    pub fn request_repair(&self, request: RepairRequest) -> [u8; 32] {
        let id = request.id;
        self.pending_repairs.write().insert(id, request);
        self.responses.write().insert(id, Vec::new());
        id
    }
    
    /// Add a response to a repair request
    pub fn add_response(&self, response: RepairResponse) -> bool {
        let mut responses = self.responses.write();
        if let Some(list) = responses.get_mut(&response.request_id) {
            list.push(response);
            return true;
        }
        false
    }
    
    /// Attempt to complete a repair
    pub fn try_complete_repair(&self, request_id: &[u8; 32]) -> Option<RepairResult> {
        let request = self.pending_repairs.read().get(request_id)?.clone();
        let responses = self.responses.read().get(request_id)?.clone();
        
        if responses.is_empty() {
            return None;
        }
        
        // Verify responses
        let valid_responses: Vec<_> = responses.iter()
            .filter(|r| {
                let hash = *blake3::hash(&r.repair_data).as_bytes();
                hash == r.content_hash
            })
            .collect();
        
        if valid_responses.is_empty() {
            return Some(RepairResult::Failed {
                string_id: request.string_id,
                reason: "No valid responses".to_string(),
            });
        }
        
        // For multi-source verification, check majority agreement
        let mut hash_counts: HashMap<[u8; 32], usize> = HashMap::new();
        for r in &valid_responses {
            *hash_counts.entry(r.content_hash).or_insert(0) += 1;
        }
        
        let (consensus_hash, count) = hash_counts.iter()
            .max_by_key(|(_, count)| *count)
            .unwrap();
        
        // Require majority for multi-source strategies
        if matches!(request.strategy, RepairStrategy::MultiSourceVerify | RepairStrategy::MultiSourceReconstruct) {
            if *count < (valid_responses.len() + 1) / 2 {
                return Some(RepairResult::Partial {
                    string_id: request.string_id,
                    completion_percentage: (*count as f64 / valid_responses.len() as f64) * 100.0,
                    missing_ranges: vec![],
                });
            }
        }
        
        // Get the repair data with consensus hash
        let repair_data = valid_responses.iter()
            .find(|r| &r.content_hash == consensus_hash)
            .map(|r| &r.repair_data)?;
        
        let result = RepairResult::Success {
            string_id: request.string_id,
            repaired_bytes: repair_data.len(),
            sources_used: valid_responses.len(),
        };
        
        // Update stats
        {
            let mut stats = self.stats.write();
            stats.total_repairs += 1;
            stats.successful_repairs += 1;
            stats.bytes_recovered += repair_data.len() as u64;
        }
        
        // Move to completed
        self.pending_repairs.write().remove(request_id);
        self.responses.write().remove(request_id);
        self.completed.write().push(result.clone());
        
        Some(result)
    }
    
    /// Mark repair as failed
    pub fn mark_failed(&self, request_id: &[u8; 32], reason: String) {
        if let Some(request) = self.pending_repairs.write().remove(request_id) {
            self.responses.write().remove(request_id);
            
            let result = RepairResult::Failed {
                string_id: request.string_id,
                reason,
            };
            
            self.stats.write().failed_repairs += 1;
            self.completed.write().push(result);
        }
    }
    
    /// Register a provider for a string
    pub fn register_provider(&self, string_id: [u8; 32], provider_id: [u8; 32]) {
        self.providers.write()
            .entry(string_id)
            .or_insert_with(HashSet::new)
            .insert(provider_id);
    }
    
    /// Get providers for a string
    pub fn get_providers(&self, string_id: &[u8; 32]) -> Vec<[u8; 32]> {
        self.providers.read()
            .get(string_id)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }
    
    /// Get statistics
    pub fn stats(&self) -> RegenerationStats {
        self.stats.read().clone()
    }
    
    /// Get pending repairs count
    pub fn pending_count(&self) -> usize {
        self.pending_repairs.read().len()
    }
    
    /// Get completed repairs
    pub fn completed_repairs(&self) -> Vec<RepairResult> {
        self.completed.read().clone()
    }
}

impl Default for RegenerationCoordinator {
    fn default() -> Self {
        Self::new([0u8; 32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_damage_detection() {
        let coord = RegenerationCoordinator::new([1u8; 32]);
        let content = b"Hello, World!";
        let expected_hash = *blake3::hash(content).as_bytes();
        
        // No damage
        assert!(coord.detect_damage(content, &expected_hash).is_none());
        
        // Mismatch
        let wrong_hash = [0u8; 32];
        let damage = coord.detect_damage(content, &wrong_hash);
        assert!(matches!(damage, Some(DamageType::MismatchError { .. })));
        
        // Total loss
        let damage = coord.detect_damage(&[], &expected_hash);
        assert!(matches!(damage, Some(DamageType::TotalLoss)));
    }
    
    #[test]
    fn test_repair_request() {
        let coord = RegenerationCoordinator::new([1u8; 32]);
        
        let damage = DamageType::MismatchError {
            computed: [0u8; 32],
            expected: [1u8; 32],
        };
        
        let request = RepairRequest::new([2u8; 32], damage, [1u8; 32]);
        let request_id = coord.request_repair(request);
        
        assert_eq!(coord.pending_count(), 1);
        
        // Add response
        let response = RepairResponse {
            request_id,
            string_id: [2u8; 32],
            repair_data: b"repaired content".to_vec(),
            provider_id: [3u8; 32],
            content_hash: *blake3::hash(b"repaired content").as_bytes(),
            signature: vec![],
            timestamp: 0,
        };
        
        assert!(coord.add_response(response));
        
        // Complete repair
        let result = coord.try_complete_repair(&request_id);
        assert!(matches!(result, Some(RepairResult::Success { .. })));
        assert_eq!(coord.pending_count(), 0);
    }
    
    #[test]
    fn test_damage_severity() {
        assert!(DamageType::SingleNucleotide { offset: 0, expected_hash: [0; 32] }.severity() < 20);
        assert!(DamageType::TotalLoss.severity() > 90);
    }
}

// ============================================================================
// Damage Detection System
// ============================================================================

/// Segment checksum for localized damage detection
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SegmentChecksum {
    /// Segment index
    pub index: usize,
    
    /// Start offset in the string content
    pub start: usize,
    
    /// End offset (exclusive)
    pub end: usize,
    
    /// BLAKE3 hash of this segment
    pub checksum: [u8; 32],
    
    /// CRC32 for quick verification
    pub crc32: u32,
}

impl SegmentChecksum {
    /// Create checksum for a segment
    pub fn new(index: usize, start: usize, data: &[u8]) -> Self {
        let checksum = *blake3::hash(data).as_bytes();
        let crc32 = crc32fast::hash(data);
        
        Self {
            index,
            start,
            end: start + data.len(),
            checksum,
            crc32,
        }
    }
    
    /// Quick verify using CRC32
    pub fn quick_verify(&self, data: &[u8]) -> bool {
        crc32fast::hash(data) == self.crc32
    }
    
    /// Full verify using BLAKE3
    pub fn full_verify(&self, data: &[u8]) -> bool {
        *blake3::hash(data).as_bytes() == self.checksum
    }
}

/// String integrity metadata with segment checksums
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StringIntegrity {
    /// String ID
    pub string_id: [u8; 32],
    
    /// Full content hash
    pub content_hash: [u8; 32],
    
    /// Segment size (default 4KB)
    pub segment_size: usize,
    
    /// Per-segment checksums
    pub segments: Vec<SegmentChecksum>,
    
    /// Last verified timestamp
    pub last_verified: i64,
    
    /// Complement string ID (for cross-verification)
    pub complement_id: Option<[u8; 32]>,
}

impl StringIntegrity {
    /// Create integrity metadata for content
    pub fn new(string_id: [u8; 32], content: &[u8], segment_size: usize) -> Self {
        let content_hash = *blake3::hash(content).as_bytes();
        
        let segments: Vec<_> = content
            .chunks(segment_size)
            .enumerate()
            .map(|(i, chunk)| SegmentChecksum::new(i, i * segment_size, chunk))
            .collect();
        
        Self {
            string_id,
            content_hash,
            segment_size,
            segments,
            last_verified: chrono::Utc::now().timestamp(),
            complement_id: None,
        }
    }
    
    /// Set complement ID for cross-verification
    pub fn with_complement(mut self, complement_id: [u8; 32]) -> Self {
        self.complement_id = Some(complement_id);
        self
    }
}

/// Damage detection result
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DamageReport {
    /// String ID
    pub string_id: [u8; 32],
    
    /// Detected damage type
    pub damage_type: Option<DamageType>,
    
    /// Damaged segments (indices)
    pub damaged_segments: Vec<usize>,
    
    /// Total segments checked
    pub total_segments: usize,
    
    /// Corruption percentage
    pub corruption_percent: f64,
    
    /// Detection timestamp
    pub detected_at: i64,
    
    /// Detection method
    pub method: DetectionMethod,
}

/// How damage was detected
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetectionMethod {
    /// Detected during routine background scan
    BackgroundScan,
    
    /// Detected when data was accessed
    AccessTimeDetection,
    
    /// Detected by comparing with complement
    ComplementComparison,
    
    /// Detected by peer verification
    PeerVerification,
    
    /// Detected by user/application request
    ManualCheck,
}

/// Active damage detector
pub struct DamageDetector {
    /// Node ID
    node_id: [u8; 32],
    
    /// String integrity metadata
    integrity_db: RwLock<HashMap<[u8; 32], StringIntegrity>>,
    
    /// Recent damage reports
    reports: RwLock<Vec<DamageReport>>,
    
    /// Statistics
    stats: RwLock<DetectorStats>,
    
    /// Scan interval in seconds
    scan_interval: u64,
}

/// Detector statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DetectorStats {
    pub total_scans: u64,
    pub strings_checked: u64,
    pub segments_verified: u64,
    pub damage_detected: u64,
    pub false_positives: u64,
    pub avg_scan_time_ms: f64,
}

impl DamageDetector {
    /// Create new damage detector
    pub fn new(node_id: [u8; 32]) -> Self {
        Self {
            node_id,
            integrity_db: RwLock::new(HashMap::new()),
            reports: RwLock::new(Vec::new()),
            stats: RwLock::new(DetectorStats::default()),
            scan_interval: 3600, // 1 hour default
        }
    }
    
    /// Set scan interval
    pub fn with_scan_interval(mut self, seconds: u64) -> Self {
        self.scan_interval = seconds;
        self
    }
    
    /// Register a string for integrity monitoring
    pub fn register_string(&self, string_id: [u8; 32], content: &[u8]) {
        let integrity = StringIntegrity::new(string_id, content, 4096);
        self.integrity_db.write().insert(string_id, integrity);
    }
    
    /// Register a string with its complement
    pub fn register_string_pair(
        &self, 
        primary_id: [u8; 32], 
        primary_content: &[u8],
        complement_id: [u8; 32],
        complement_content: &[u8],
    ) {
        let primary_integrity = StringIntegrity::new(primary_id, primary_content, 4096)
            .with_complement(complement_id);
        let complement_integrity = StringIntegrity::new(complement_id, complement_content, 4096)
            .with_complement(primary_id);
        
        let mut db = self.integrity_db.write();
        db.insert(primary_id, primary_integrity);
        db.insert(complement_id, complement_integrity);
    }
    
    /// Quick check using CRC32 (fast, for frequent checks)
    pub fn quick_check(&self, string_id: &[u8; 32], content: &[u8]) -> Option<DamageReport> {
        let integrity = self.integrity_db.read().get(string_id)?.clone();
        let mut damaged_segments = Vec::new();
        
        for (i, chunk) in content.chunks(integrity.segment_size).enumerate() {
            if let Some(seg) = integrity.segments.get(i) {
                if !seg.quick_verify(chunk) {
                    damaged_segments.push(i);
                }
            }
        }
        
        if damaged_segments.is_empty() {
            return None;
        }
        
        let corruption_percent = (damaged_segments.len() as f64 / integrity.segments.len() as f64) * 100.0;
        let damage_type = self.classify_damage(&damaged_segments, &integrity, content);
        
        let report = DamageReport {
            string_id: *string_id,
            damage_type: Some(damage_type),
            damaged_segments,
            total_segments: integrity.segments.len(),
            corruption_percent,
            detected_at: chrono::Utc::now().timestamp(),
            method: DetectionMethod::AccessTimeDetection,
        };
        
        self.reports.write().push(report.clone());
        self.stats.write().damage_detected += 1;
        
        Some(report)
    }
    
    /// Full integrity check using BLAKE3 (slower, thorough)
    pub fn full_check(&self, string_id: &[u8; 32], content: &[u8]) -> DamageReport {
        let integrity_opt = self.integrity_db.read().get(string_id).cloned();
        
        let (damaged_segments, total_segments) = if let Some(integrity) = integrity_opt {
            let mut damaged = Vec::new();
            
            for (i, chunk) in content.chunks(integrity.segment_size).enumerate() {
                if let Some(seg) = integrity.segments.get(i) {
                    if !seg.full_verify(chunk) {
                        damaged.push(i);
                    }
                }
            }
            
            (damaged, integrity.segments.len())
        } else {
            (Vec::new(), 0)
        };
        
        let corruption_percent = if total_segments > 0 {
            (damaged_segments.len() as f64 / total_segments as f64) * 100.0
        } else {
            0.0
        };
        
        let damage_type = if damaged_segments.is_empty() {
            None
        } else if let Some(integrity) = self.integrity_db.read().get(string_id) {
            Some(self.classify_damage(&damaged_segments, integrity, content))
        } else {
            Some(DamageType::MismatchError {
                computed: *blake3::hash(content).as_bytes(),
                expected: *string_id,
            })
        };
        
        self.stats.write().strings_checked += 1;
        self.stats.write().segments_verified += total_segments as u64;
        
        DamageReport {
            string_id: *string_id,
            damage_type,
            damaged_segments,
            total_segments,
            corruption_percent,
            detected_at: chrono::Utc::now().timestamp(),
            method: DetectionMethod::ManualCheck,
        }
    }
    
    /// Classify damage based on pattern
    fn classify_damage(&self, damaged_segments: &[usize], integrity: &StringIntegrity, content: &[u8]) -> DamageType {
        let damage_ratio = damaged_segments.len() as f64 / integrity.segments.len() as f64;
        
        if content.is_empty() {
            return DamageType::TotalLoss;
        }
        
        if damage_ratio > 0.5 {
            return DamageType::SevereCorruption {
                recovery_chance_percent: ((1.0 - damage_ratio) * 100.0) as u8,
            };
        }
        
        if damaged_segments.len() == 1 {
            let seg_idx = damaged_segments[0];
            let start = seg_idx * integrity.segment_size;
            let end = (start + integrity.segment_size).min(content.len());
            let segment = &content[start..end];
            
            // Check if it's a small error
            let zero_count = segment.iter().filter(|&&b| b == 0).count();
            if zero_count < 32 {
                return DamageType::SingleNucleotide {
                    offset: start,
                    expected_hash: integrity.segments[seg_idx].checksum,
                };
            }
        }
        
        // Multiple segment corruption
        let start = damaged_segments.first().unwrap() * integrity.segment_size;
        let end = (damaged_segments.last().unwrap() + 1) * integrity.segment_size;
        
        DamageType::SegmentCorruption {
            start,
            end: end.min(content.len()),
            severity_percent: (damage_ratio * 100.0) as u8,
        }
    }
    
    /// Compare primary and complement for consistency
    pub fn compare_with_complement(
        &self,
        primary_id: &[u8; 32],
        primary_content: &[u8],
        complement_content: &[u8],
    ) -> Option<DamageReport> {
        // In DNA, complement is derived via base-pairing rules
        // In Datachain Rope, complement stores same data with different encoding
        // Check if either is corrupted by comparing lengths and structure
        
        if primary_content.len() != complement_content.len() {
            return Some(DamageReport {
                string_id: *primary_id,
                damage_type: Some(DamageType::ComplementDesync),
                damaged_segments: vec![],
                total_segments: 0,
                corruption_percent: 100.0,
                detected_at: chrono::Utc::now().timestamp(),
                method: DetectionMethod::ComplementComparison,
            });
        }
        
        // XOR comparison to find differences
        let mut different_positions = Vec::new();
        for (i, (a, b)) in primary_content.iter().zip(complement_content.iter()).enumerate() {
            // Complement should be XOR'd with a specific pattern
            // For simplicity, we check if they're different where they shouldn't be
            if a != b {
                different_positions.push(i);
            }
        }
        
        if different_positions.is_empty() {
            return None;
        }
        
        let segment_size = 4096;
        let damaged_segments: HashSet<_> = different_positions
            .iter()
            .map(|&pos| pos / segment_size)
            .collect();
        
        Some(DamageReport {
            string_id: *primary_id,
            damage_type: Some(DamageType::ComplementDesync),
            damaged_segments: damaged_segments.into_iter().collect(),
            total_segments: (primary_content.len() + segment_size - 1) / segment_size,
            corruption_percent: (different_positions.len() as f64 / primary_content.len() as f64) * 100.0,
            detected_at: chrono::Utc::now().timestamp(),
            method: DetectionMethod::ComplementComparison,
        })
    }
    
    /// Get recent damage reports
    pub fn recent_reports(&self, limit: usize) -> Vec<DamageReport> {
        let reports = self.reports.read();
        reports.iter().rev().take(limit).cloned().collect()
    }
    
    /// Get statistics
    pub fn stats(&self) -> DetectorStats {
        self.stats.read().clone()
    }
}

impl Default for DamageDetector {
    fn default() -> Self {
        Self::new([0u8; 32])
    }
}

// ============================================================================
// Reed-Solomon Error Correction
// ============================================================================

/// Reed-Solomon parameters
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReedSolomonParams {
    /// Number of data shards
    pub data_shards: usize,
    
    /// Number of parity shards
    pub parity_shards: usize,
    
    /// Shard size in bytes
    pub shard_size: usize,
}

impl Default for ReedSolomonParams {
    fn default() -> Self {
        Self {
            data_shards: 4,
            parity_shards: 2,
            shard_size: 1024,
        }
    }
}

/// Reed-Solomon encoded data
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReedSolomonData {
    /// Parameters used for encoding
    pub params: ReedSolomonParams,
    
    /// Data shards
    pub data_shards: Vec<Vec<u8>>,
    
    /// Parity shards
    pub parity_shards: Vec<Vec<u8>>,
    
    /// Original data length
    pub original_length: usize,
}

/// Reed-Solomon encoder/decoder
pub struct ReedSolomonCodec {
    params: ReedSolomonParams,
}

impl ReedSolomonCodec {
    /// Create new codec with default params
    pub fn new() -> Self {
        Self {
            params: ReedSolomonParams::default(),
        }
    }
    
    /// Create codec with custom params
    pub fn with_params(params: ReedSolomonParams) -> Self {
        Self { params }
    }
    
    /// Encode data with Reed-Solomon
    pub fn encode(&self, data: &[u8]) -> ReedSolomonData {
        let shard_size = self.params.shard_size;
        let data_shards_count = self.params.data_shards;
        let parity_shards_count = self.params.parity_shards;
        
        // Pad data to fit into shards
        let total_data_size = data_shards_count * shard_size;
        let mut padded_data = data.to_vec();
        padded_data.resize(total_data_size, 0);
        
        // Split into data shards
        let data_shards: Vec<Vec<u8>> = padded_data
            .chunks(shard_size)
            .map(|c| c.to_vec())
            .collect();
        
        // Generate parity shards using XOR (simplified Reed-Solomon)
        // In production, use a proper RS library like reed-solomon-erasure
        let mut parity_shards = vec![vec![0u8; shard_size]; parity_shards_count];
        
        for (i, parity) in parity_shards.iter_mut().enumerate() {
            // Each parity shard is XOR of a subset of data shards
            // Parity[i] = XOR of data shards with different patterns
            for (j, data_shard) in data_shards.iter().enumerate() {
                if (j + i) % (parity_shards_count + 1) != parity_shards_count {
                    for (k, &byte) in data_shard.iter().enumerate() {
                        parity[k] ^= byte;
                    }
                }
            }
        }
        
        ReedSolomonData {
            params: self.params.clone(),
            data_shards,
            parity_shards,
            original_length: data.len(),
        }
    }
    
    /// Attempt to recover data from shards
    pub fn decode(&self, mut rs_data: ReedSolomonData) -> Result<Vec<u8>, String> {
        let data_shards_count = rs_data.params.data_shards;
        let shard_size = rs_data.params.shard_size;
        
        // Check how many data shards are available (non-empty)
        let available_count = rs_data.data_shards.iter()
            .filter(|s| !s.is_empty() && s.iter().any(|&b| b != 0))
            .count();
        
        // If all data shards available, just reconstruct
        if available_count == data_shards_count {
            let mut result: Vec<u8> = rs_data.data_shards.into_iter().flatten().collect();
            result.truncate(rs_data.original_length);
            return Ok(result);
        }
        
        // Find missing shards
        let missing_indices: Vec<_> = rs_data.data_shards.iter()
            .enumerate()
            .filter(|(_, s)| s.is_empty() || s.iter().all(|&b| b == 0))
            .map(|(i, _)| i)
            .collect();
        
        // Check if recovery is possible
        if missing_indices.len() > rs_data.parity_shards.len() {
            return Err(format!(
                "Cannot recover: {} missing shards, only {} parity shards",
                missing_indices.len(),
                rs_data.parity_shards.len()
            ));
        }
        
        // Simplified recovery using XOR (works for single missing shard)
        if missing_indices.len() == 1 && !rs_data.parity_shards.is_empty() {
            let missing_idx = missing_indices[0];
            let mut recovered = vec![0u8; shard_size];
            
            // XOR all available data shards with first parity
            for (j, data_shard) in rs_data.data_shards.iter().enumerate() {
                if j != missing_idx {
                    for (k, &byte) in data_shard.iter().enumerate() {
                        recovered[k] ^= byte;
                    }
                }
            }
            
            // XOR with parity to recover missing shard
            for (k, &byte) in rs_data.parity_shards[0].iter().enumerate() {
                recovered[k] ^= byte;
            }
            
            rs_data.data_shards[missing_idx] = recovered;
        }
        
        let mut result: Vec<u8> = rs_data.data_shards.into_iter().flatten().collect();
        result.truncate(rs_data.original_length);
        Ok(result)
    }
    
    /// Check if data can be recovered
    pub fn can_recover(&self, rs_data: &ReedSolomonData) -> bool {
        let missing = rs_data.data_shards.iter()
            .filter(|s| s.is_empty() || s.iter().all(|&b| b == 0))
            .count();
        
        missing <= rs_data.parity_shards.len()
    }
}

impl Default for ReedSolomonCodec {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Network Repair Integration
// ============================================================================

/// Repair request to send over the network
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkRepairRequest {
    /// Request ID
    pub id: [u8; 32],
    
    /// String ID to repair
    pub string_id: [u8; 32],
    
    /// Damaged segments (if known)
    pub damaged_segments: Vec<usize>,
    
    /// Strategy to use
    pub strategy: RepairStrategy,
    
    /// Requester node
    pub requester: [u8; 32],
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Signature
    pub signature: Vec<u8>,
}

/// Repair data from network peer
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkRepairData {
    /// Request ID this responds to
    pub request_id: [u8; 32],
    
    /// Provider node ID
    pub provider: [u8; 32],
    
    /// Segment data (index -> data)
    pub segments: HashMap<usize, Vec<u8>>,
    
    /// Full content (if requested)
    pub full_content: Option<Vec<u8>>,
    
    /// Content hash
    pub content_hash: [u8; 32],
    
    /// Signature
    pub signature: Vec<u8>,
}

/// Network repair coordinator
pub struct NetworkRepairCoordinator {
    /// Node ID
    node_id: [u8; 32],
    
    /// Active repair requests
    active_requests: RwLock<HashMap<[u8; 32], NetworkRepairRequest>>,
    
    /// Received repair data
    received_data: RwLock<HashMap<[u8; 32], Vec<NetworkRepairData>>>,
    
    /// Repair callbacks
    repair_callbacks: RwLock<HashMap<[u8; 32], Box<dyn Fn(Vec<u8>) + Send + Sync>>>,
}

impl NetworkRepairCoordinator {
    /// Create new coordinator
    pub fn new(node_id: [u8; 32]) -> Self {
        Self {
            node_id,
            active_requests: RwLock::new(HashMap::new()),
            received_data: RwLock::new(HashMap::new()),
            repair_callbacks: RwLock::new(HashMap::new()),
        }
    }
    
    /// Create a repair request
    pub fn create_request(
        &self,
        string_id: [u8; 32],
        damaged_segments: Vec<usize>,
        strategy: RepairStrategy,
    ) -> NetworkRepairRequest {
        let timestamp = chrono::Utc::now().timestamp();
        
        let mut id_data = string_id.to_vec();
        id_data.extend_from_slice(&self.node_id);
        id_data.extend_from_slice(&timestamp.to_le_bytes());
        let id = *blake3::hash(&id_data).as_bytes();
        
        let request = NetworkRepairRequest {
            id,
            string_id,
            damaged_segments,
            strategy,
            requester: self.node_id,
            timestamp,
            signature: vec![], // Signature added by caller
        };
        
        self.active_requests.write().insert(id, request.clone());
        self.received_data.write().insert(id, Vec::new());
        
        request
    }
    
    /// Handle incoming repair data
    pub fn receive_repair_data(&self, data: NetworkRepairData) -> bool {
        let request_id = data.request_id;
        
        // Verify we have this request
        if !self.active_requests.read().contains_key(&request_id) {
            return false;
        }
        
        // Store the data
        if let Some(list) = self.received_data.write().get_mut(&request_id) {
            list.push(data);
            return true;
        }
        
        false
    }
    
    /// Try to complete repair with received data
    pub fn try_complete(&self, request_id: &[u8; 32]) -> Option<Vec<u8>> {
        let request = self.active_requests.read().get(request_id)?.clone();
        let received = self.received_data.read().get(request_id)?.clone();
        
        if received.is_empty() {
            return None;
        }
        
        // Find consensus on content hash
        let mut hash_votes: HashMap<[u8; 32], Vec<&NetworkRepairData>> = HashMap::new();
        for data in &received {
            hash_votes.entry(data.content_hash).or_default().push(data);
        }
        
        let (best_hash, best_data) = hash_votes.into_iter()
            .max_by_key(|(_, v)| v.len())?;
        
        // Need majority for multi-source strategies
        if matches!(request.strategy, RepairStrategy::MultiSourceVerify | RepairStrategy::MultiSourceReconstruct) {
            if best_data.len() < (received.len() + 1) / 2 {
                return None;
            }
        }
        
        // Get the repaired content
        let repaired = if let Some(content) = &best_data[0].full_content {
            content.clone()
        } else {
            // Reconstruct from segments
            let mut segments: HashMap<usize, &[u8]> = HashMap::new();
            for data in &best_data {
                for (idx, seg_data) in &data.segments {
                    segments.entry(*idx).or_insert(seg_data);
                }
            }
            
            // Sort by index and concatenate
            let mut indices: Vec<_> = segments.keys().copied().collect();
            indices.sort();
            
            indices.iter()
                .filter_map(|i| segments.get(i).copied())
                .flatten()
                .copied()
                .collect()
        };
        
        // Verify hash
        if *blake3::hash(&repaired).as_bytes() == best_hash {
            // Clean up
            self.active_requests.write().remove(request_id);
            self.received_data.write().remove(request_id);
            
            // Call callback if registered
            if let Some(callback) = self.repair_callbacks.write().remove(request_id) {
                callback(repaired.clone());
            }
            
            Some(repaired)
        } else {
            None
        }
    }
    
    /// Register a callback for when repair completes
    pub fn on_repair_complete<F>(&self, request_id: [u8; 32], callback: F)
    where
        F: Fn(Vec<u8>) + Send + Sync + 'static,
    {
        self.repair_callbacks.write().insert(request_id, Box::new(callback));
    }
    
    /// Get active request count
    pub fn active_requests_count(&self) -> usize {
        self.active_requests.read().len()
    }
}

impl Default for NetworkRepairCoordinator {
    fn default() -> Self {
        Self::new([0u8; 32])
    }
}

#[cfg(test)]
mod damage_detection_tests {
    use super::*;
    
    #[test]
    fn test_segment_checksum() {
        let data = b"Hello, World! This is test data for checksum verification.";
        let checksum = SegmentChecksum::new(0, 0, data);
        
        assert!(checksum.quick_verify(data));
        assert!(checksum.full_verify(data));
        assert!(!checksum.quick_verify(b"Different data"));
    }
    
    #[test]
    fn test_damage_detector() {
        let detector = DamageDetector::new([1u8; 32]);
        let content = b"Test content for damage detection";
        let string_id = *blake3::hash(content).as_bytes();
        
        detector.register_string(string_id, content);
        
        // No damage
        let report = detector.full_check(&string_id, content);
        assert!(report.damage_type.is_none());
        
        // Introduce damage
        let mut corrupted = content.to_vec();
        corrupted[0] = 0xFF;
        
        let report = detector.full_check(&string_id, &corrupted);
        assert!(report.damage_type.is_some());
    }
    
    #[test]
    fn test_reed_solomon() {
        let codec = ReedSolomonCodec::new();
        let original = b"Test data for Reed-Solomon encoding and recovery!";
        
        let encoded = codec.encode(original);
        assert_eq!(encoded.data_shards.len(), 4);
        assert_eq!(encoded.parity_shards.len(), 2);
        
        // Recover without damage
        let recovered = codec.decode(encoded.clone()).unwrap();
        assert_eq!(&recovered, original);
        
        // Simulate one missing shard
        let mut damaged = encoded;
        damaged.data_shards[1] = vec![0; damaged.params.shard_size];
        
        assert!(codec.can_recover(&damaged));
    }
}

