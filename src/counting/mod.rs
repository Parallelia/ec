use anyhow::Result;

use crate::rules::ElectionRules;

/// A single ballot as stored in SQLite: ordered list of candidate IDs.
/// Plurality: vec![3]
/// STV:       vec![3, 1, 4, 2]
pub type Ballot = Vec<u8>;

#[derive(Debug, Clone)]
pub struct CountResult {
    /// Elected candidate IDs, in order of election (for STV: order matters).
    pub elected: Vec<u8>,
    /// Full per-candidate vote totals or final transfer tallies.
    pub tally: Vec<CandidateTally>,
    /// Optional: serialized count sheet for STV (one entry per round).
    pub count_sheet: Option<Vec<CountRound>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateStatus {
    Active,
    Elected,
    Excluded,
}

impl CandidateStatus {
    /// Stable wire representation — independent of Rust variant names.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Elected => "elected",
            Self::Excluded => "excluded",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CandidateTally {
    pub candidate_id: u8,
    pub votes: f64,
    pub status: CandidateStatus,
}

#[derive(Debug, Clone)]
pub struct CountRound {
    pub round: u32,
    pub tallies: Vec<CandidateTally>,
    pub action: String,
}

pub trait CountingAlgorithm: Send + Sync {
    /// Count ballots for the given election.
    /// `candidates` is the authoritative candidate list from the database, so
    /// candidates without any votes still appear in the tally and can be elected.
    fn count(
        &self,
        ballots: &[Ballot],
        candidates: &[u8],
        rules: &ElectionRules,
    ) -> Result<CountResult>;
}

/// Deterministic default seed for "random" tie-breaking, derived from the
/// candidate set. Predictable by design: it can be recomputed by any auditor.
/// Elections that need an unpredictable-but-auditable draw must set
/// `tie_breaking_seed` in their rules file.
pub(crate) fn default_tie_seed(candidate_ids: &std::collections::BTreeSet<u8>) -> u64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for id in candidate_ids {
        hasher.update([*id]);
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes)
}

mod plurality;
mod stv;

use plurality::PluralityAlgorithm;
use stv::StvAlgorithm;

/// Registry: given a rules_id string, return the correct algorithm.
pub fn algorithm_for(rules_id: &str) -> Result<Box<dyn CountingAlgorithm>> {
    match rules_id {
        "plurality" => Ok(Box::new(PluralityAlgorithm)),
        "stv" => Ok(Box::new(StvAlgorithm)),
        other => anyhow::bail!("UNKNOWN_RULES: {}", other),
    }
}
