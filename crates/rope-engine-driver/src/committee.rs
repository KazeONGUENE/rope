//! The EVM-quorum committee roster.
//!
//! This is the generic, N-node membership list that lets new nodes join
//! the EVM block-production quorum by data change alone (add one JSON
//! entry + redistribute the file + restart the attester service on every
//! node) rather than a code change. `deploy/scripts/onboard-evm-quorum-node.sh`
//! automates exactly that step for future nodes.
//!
//! Quorum threshold reuses the identical `2f+1` Byzantine math already
//! implemented and tested for the native String-Lattice testimony
//! layer in `rope-node/src/consensus_orchestrator.rs`, so both layers of
//! Datachain Rope consensus (native knots and EVM blocks) agree on what
//! "enough of the committee" means for an N-member committee.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitteeMember {
    /// Human label, e.g. "BLUE", "GREEN", "DO-rpc-1". Cosmetic only.
    pub name: String,
    /// Ed25519 pubkey (hex), taken from this node's own
    /// `~/.rope/validator_key.bin` — the same identity already registered
    /// in the native `validator_set.json` roster.
    pub pubkey_hex: String,
    /// Base URL of this node's attester HTTP service, e.g.
    /// `http://10.x.x.x:9600`. Reachable from the proposer only — this is
    /// an internal control-plane endpoint, never exposed publicly.
    pub attester_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Committee {
    pub members: Vec<CommitteeMember>,
}

impl Committee {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading committee roster at {}", path.display()))?;
        let committee: Committee = serde_json::from_str(&text)
            .with_context(|| format!("parsing {} as committee roster", path.display()))?;
        if committee.members.is_empty() {
            bail!("committee roster at {} has zero members", path.display());
        }
        let mut seen = std::collections::HashSet::new();
        for m in &committee.members {
            if !seen.insert(m.pubkey_hex.clone()) {
                bail!("committee roster has duplicate pubkey {}", m.pubkey_hex);
            }
        }
        Ok(committee)
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn contains_pubkey(&self, pubkey_hex: &str) -> bool {
        self.members.iter().any(|m| m.pubkey_hex == pubkey_hex)
    }

    /// `2f+1` out of `n = 3f+1` members, same formula as
    /// `consensus_orchestrator.rs`'s `finality_quorum`. For committee
    /// sizes that aren't an exact `3f+1`, this is still the standard
    /// conservative BFT threshold: `floor(2n/3) + 1`.
    pub fn quorum_threshold(&self) -> usize {
        quorum_threshold_for(self.len())
    }
}

pub fn quorum_threshold_for(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let f = (n.saturating_sub(1)) / 3;
    (2 * f + 1).max(1).min(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_roster(dir: &Path, members: &[(&str, &str, &str)]) -> std::path::PathBuf {
        let committee = Committee {
            members: members
                .iter()
                .map(|(name, pk, url)| CommitteeMember {
                    name: name.to_string(),
                    pubkey_hex: pk.to_string(),
                    attester_url: url.to_string(),
                })
                .collect(),
        };
        let path = dir.join("committee.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(serde_json::to_string_pretty(&committee).unwrap().as_bytes())
            .unwrap();
        path
    }

    #[test]
    fn test_quorum_threshold_four_nodes_is_three() {
        // n=4 -> f=1 -> 2f+1=3 (matches consensus_orchestrator.rs's
        // documented "committee grows to 4 -> f=1 -> quorum=3" test).
        assert_eq!(quorum_threshold_for(4), 3);
    }

    #[test]
    fn test_quorum_threshold_single_node_is_one() {
        assert_eq!(quorum_threshold_for(1), 1);
    }

    #[test]
    fn test_quorum_threshold_scales_with_committee() {
        assert_eq!(quorum_threshold_for(7), 5); // f=2 -> 2*2+1=5
        assert_eq!(quorum_threshold_for(10), 7); // f=3 -> 2*3+1=7
    }

    #[test]
    fn test_load_committee_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_roster(
            dir.path(),
            &[
                ("BLUE", "aa", "http://10.0.0.1:9600"),
                ("GREEN", "bb", "http://10.0.0.2:9600"),
                ("DO-rpc-1", "cc", "http://10.0.0.3:9600"),
                ("DO-rpc-2", "dd", "http://10.0.0.4:9600"),
            ],
        );
        let committee = Committee::load(&path).unwrap();
        assert_eq!(committee.len(), 4);
        assert_eq!(committee.quorum_threshold(), 3);
        assert!(committee.contains_pubkey("bb"));
        assert!(!committee.contains_pubkey("zz"));
    }

    #[test]
    fn test_load_rejects_duplicate_pubkeys() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_roster(
            dir.path(),
            &[
                ("BLUE", "aa", "http://10.0.0.1:9600"),
                ("GREEN-IMPOSTOR", "aa", "http://10.0.0.2:9600"),
            ],
        );
        assert!(Committee::load(&path).is_err());
    }

    #[test]
    fn test_load_rejects_empty_roster() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_roster(dir.path(), &[]);
        assert!(Committee::load(&path).is_err());
    }

    #[test]
    fn test_load_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.json");
        assert!(Committee::load(&missing).is_err());
    }
}
