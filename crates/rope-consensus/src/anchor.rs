//! Anchor strings for consensus synchronization

use rope_core::string::RopeString;
use rope_core::types::StringId;
use serde::{Deserialize, Serialize};

/// Anchor String - Synchronization point in the lattice
/// 
/// Equivalent to hashgraph's "famous witnesses"
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnchorString {
    /// The underlying string
    pub string: RopeString,
    
    /// Consensus round number
    pub round: u64,
    
    /// Previous anchors this one strongly sees
    pub strongly_sees: Vec<StringId>,
    
    /// Number of testimonies received
    pub testimony_count: u32,
    
    /// Whether this is a famous anchor
    pub is_famous: bool,
}

impl AnchorString {
    /// Create new anchor string
    pub fn new(string: RopeString, round: u64) -> Self {
        Self {
            string,
            round,
            strongly_sees: Vec::new(),
            testimony_count: 0,
            is_famous: false,
        }
    }

    /// Get the anchor's string ID
    pub fn id(&self) -> StringId {
        self.string.id()
    }
}

