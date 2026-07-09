use std::path::Path;

use ec::counting::{Ballot, algorithm_for};
use ec::rules::load_rules;

#[test]
fn stv_simple_three_seat_election() {
    let rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    let algo = algorithm_for("stv").expect("algorithm");

    // 3 candidates, 3 seats, 5 ballots with clear preferences.
    // This is intentionally simple; no surplus handling is required for the outcome.
    let ballots: Vec<Ballot> = vec![
        vec![1, 2, 3],
        vec![1, 2, 3],
        vec![2, 1, 3],
        vec![2, 1, 3],
        vec![3, 2, 1],
    ];

    let mut rules = rules;
    rules.election.seats = 2;

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");

    // Candidates 1 and 2 have the strongest first preferences, so they should be elected.
    assert!(result.elected.contains(&1));
    assert!(result.elected.contains(&2));
    assert_eq!(result.elected.len(), 2);
}

#[test]
fn stv_excludes_lowest_and_transfers_preferences() {
    let rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    let algo = algorithm_for("stv").expect("algorithm");

    // 3 candidates, 2 seats.
    // Candidate 3 starts weakest in first preferences and should be excluded
    // before the last round; seats are filled by the remaining candidates.
    let ballots: Vec<Ballot> = vec![
        vec![1, 2, 3],
        vec![1, 2, 3],
        vec![2, 1, 3],
        vec![2, 1, 3],
        vec![3, 1, 2],
        vec![3, 2, 1],
    ];

    let mut rules = rules;
    rules.election.seats = 2;

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");

    // We only assert that exactly two candidates are elected.
    // The simplified STV implementation is deterministic and must not elect
    // the same candidate twice or fewer than the requested number of seats.
    assert_eq!(result.elected.len(), 2);
}

/// Regression test: only the surplus may transfer, never the full ballot weight.
///
/// V = 8, seats = 2, Droop quota = floor(8/3) + 1 = 3.
/// Candidate 1 is elected with 5 first preferences (surplus 2, transfer value
/// 2/5 = 0.4). Candidate 2 must receive only 5 × 0.4 = 2.0 votes and LOSE the
/// second seat to candidate 3 (3 first preferences). A buggy count that lets
/// the retained portion flow onward gives candidate 2 the full 5.0 votes and
/// elects the wrong candidate.
#[test]
fn stv_surplus_transfers_only_surplus_not_full_weight() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 2;
    let algo = algorithm_for("stv").expect("algorithm");

    let mut ballots: Vec<Ballot> = Vec::new();
    for _ in 0..5 {
        ballots.push(vec![1, 2]);
    }
    for _ in 0..3 {
        ballots.push(vec![3]);
    }

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");

    assert_eq!(result.elected, vec![1, 3]);

    // The elected candidate retains the quota in the final tally.
    let c1 = result.tally.iter().find(|t| t.candidate_id == 1).unwrap();
    assert!(
        (c1.votes - 3.0).abs() < 1e-9,
        "winner retains the quota, got {}",
        c1.votes
    );
    let c2 = result.tally.iter().find(|t| t.candidate_id == 2).unwrap();
    assert!(
        (c2.votes - 2.0).abs() < 1e-9,
        "runner-up gets only the surplus, got {}",
        c2.votes
    );
}

/// A registered candidate that never appears on any ballot must still show up
/// in the tally (with zero votes) instead of silently vanishing from results.
#[test]
fn stv_zero_vote_candidate_appears_in_tally() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 2;
    let algo = algorithm_for("stv").expect("algorithm");

    let ballots: Vec<Ballot> = vec![vec![1, 2], vec![1, 2], vec![2, 1]];

    let result = algo.count(&ballots, &[1, 2, 3, 4], &rules).expect("count");

    assert!(result.tally.iter().any(|t| t.candidate_id == 4));
}

/// Unsupported rule options must be rejected loudly, never silently ignored:
/// counting with a different algorithm than the rules declare would break the
/// verifiability promise.
#[test]
fn stv_rejects_unsupported_options() {
    let algo = algorithm_for("stv").expect("algorithm");
    let ballots: Vec<Ballot> = vec![vec![1, 2]];

    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.counting.quota = Some("hare".to_string());
    assert!(algo.count(&ballots, &[1, 2], &rules).is_err());

    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.counting.transfer_method = Some("meek".to_string());
    assert!(algo.count(&ballots, &[1, 2], &rules).is_err());

    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.counting.exclusion_method = Some("by_parcel".to_string());
    assert!(algo.count(&ballots, &[1, 2], &rules).is_err());
}
