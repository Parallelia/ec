use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use rand::{RngExt, SeedableRng, rngs::StdRng};

use crate::counting::{
    Ballot, CandidateStatus, CandidateTally, CountResult, CountingAlgorithm, default_tie_seed,
};
use crate::rules::{BallotMethod, ElectionRules};

/// Simple plurality (first-past-the-post) counting algorithm.
pub struct PluralityAlgorithm;

impl CountingAlgorithm for PluralityAlgorithm {
    fn count(
        &self,
        ballots: &[Ballot],
        candidates: &[u8],
        rules: &ElectionRules,
    ) -> Result<CountResult> {
        if rules.ballot.method != BallotMethod::Single {
            anyhow::bail!("Plurality requires single-choice ballots");
        }

        // Authoritative candidate set from the database, so candidates with
        // zero votes still appear in the tally.
        let candidate_set: BTreeSet<u8> = candidates.iter().copied().collect();
        let mut counts: BTreeMap<u8, f64> = candidate_set.iter().map(|&c| (c, 0.0)).collect();

        for ballot in ballots {
            if let Some(&candidate) = ballot.first() {
                *counts.entry(candidate).or_insert(0.0) += 1.0;
            }
        }

        let seats = rules.election.seats.max(1) as usize;
        let min_winner_votes = rules.counting.min_winner_votes.unwrap_or(0) as f64;

        let mut tallies: Vec<CandidateTally> = counts
            .iter()
            .map(|(&id, &votes)| CandidateTally {
                candidate_id: id,
                votes,
                status: CandidateStatus::Active,
            })
            .collect();

        // Sort by votes (desc), then candidate_id (asc).
        tallies.sort_by(|a, b| {
            b.votes
                .partial_cmp(&a.votes)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.candidate_id.cmp(&b.candidate_id))
        });

        // Fill seats group-by-group (a group = candidates with equal votes) so
        // ties at the seat boundary are resolved by the configured rule
        // instead of silently by candidate id.
        let mut elected: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < tallies.len() && elected.len() < seats {
            let mut j = i + 1;
            while j < tallies.len() && tallies[j].votes == tallies[i].votes {
                j += 1;
            }
            if tallies[i].votes < min_winner_votes {
                break;
            }
            let group: Vec<u8> = tallies[i..j].iter().map(|t| t.candidate_id).collect();
            let remaining = seats - elected.len();
            if group.len() <= remaining {
                elected.extend(&group);
            } else {
                elected.extend(break_boundary_tie(
                    &group,
                    remaining,
                    &candidate_set,
                    rules,
                )?);
            }
            i = j;
        }

        for t in tallies.iter_mut() {
            t.status = if elected.contains(&t.candidate_id) {
                CandidateStatus::Elected
            } else {
                CandidateStatus::Excluded
            };
        }

        Ok(CountResult {
            elected,
            tally: tallies,
            count_sheet: None,
        })
    }
}

/// Pick `k` winners from a group of candidates tied at the seat boundary.
/// `group` is sorted by candidate_id ascending (inherited from the tally sort).
fn break_boundary_tie(
    group: &[u8],
    k: usize,
    candidate_set: &BTreeSet<u8>,
    rules: &ElectionRules,
) -> Result<Vec<u8>> {
    match rules.counting.tie_breaking.as_deref() {
        Some("random") => {
            let seed = rules
                .counting
                .tie_breaking_seed
                .unwrap_or_else(|| default_tie_seed(candidate_set));
            let mut rng = StdRng::seed_from_u64(seed);
            let mut shuffled = group.to_vec();
            // Partial Fisher-Yates: the first k positions are a uniform draw.
            for idx in 0..k {
                let swap_with = rng.random_range(idx..shuffled.len());
                shuffled.swap(idx, swap_with);
            }
            shuffled.truncate(k);
            Ok(shuffled)
        }
        // "none": declare a tie — leave the contested seats unfilled.
        Some("none") => Ok(Vec::new()),
        Some("manual") => anyhow::bail!("NotImplemented: manual tie-break"),
        // Fallback: deterministic by lowest candidate id.
        _ => Ok(group[..k].to_vec()),
    }
}
