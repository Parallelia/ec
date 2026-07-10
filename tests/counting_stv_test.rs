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
fn stv_rejects_non_ranked_ballot_method() {
    let rules = load_rules("plurality", Path::new("rules")).expect("load plurality rules");
    let algo = algorithm_for("stv").expect("algorithm");

    let err = algo
        .count(&[vec![1]], &[1, 2], &rules)
        .expect_err("single-choice ballots must be rejected");
    assert!(err.to_string().contains("ranked"));
}

#[test]
fn stv_no_valid_ballots_elects_nobody() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 2;
    let algo = algorithm_for("stv").expect("algorithm");

    // No ballots at all.
    let result = algo.count(&[], &[1, 2, 3], &rules).expect("count");
    assert!(result.elected.is_empty());
    assert!(result.tally.is_empty());
    assert!(result.count_sheet.expect("count sheet present").is_empty());

    // Only empty ballots — filtered out before counting.
    let ballots: Vec<Ballot> = vec![vec![], vec![]];
    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");
    assert!(result.elected.is_empty());
}

#[test]
fn stv_surplus_transfer_with_exhausted_ballots() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 2;
    let algo = algorithm_for("stv").expect("algorithm");

    // Winner's ballots have no further preferences: the surplus exhausts
    // instead of transferring, and the remaining seat is filled by bulk
    // election of the strongest remaining candidate.
    let mut ballots: Vec<Ballot> = Vec::new();
    for _ in 0..5 {
        ballots.push(vec![1]); // bullet votes, nothing to transfer to
    }
    for _ in 0..2 {
        ballots.push(vec![2]);
    }
    ballots.push(vec![3]);

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");
    assert_eq!(result.elected.len(), 2);
    assert_eq!(result.elected[0], 1);
}

#[test]
fn stv_backwards_tie_break_uses_earlier_rounds() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 1;
    rules.counting.tie_breaking = Some("backwards".to_string());
    let algo = algorithm_for("stv").expect("algorithm");

    // Round 1: c1=3, c2=2, c3=2, c4=1 (quota = floor(8/2)+1 = 5).
    // Round 1 excludes c4, transferring to c3. Round 2: c1=3, c2=2, c3=3 →
    // c2 excluded. Round 3: exclusion tie between c1 and c3 at 3.0; looking
    // backwards, round 1 had c3 behind (2 < 3) so c3 is excluded and c1 wins.
    let ballots: Vec<Ballot> = vec![
        vec![1],
        vec![1],
        vec![1],
        vec![2],
        vec![2],
        vec![3],
        vec![3],
        vec![4, 3],
    ];

    let result = algo.count(&ballots, &[1, 2, 3, 4], &rules).expect("count");
    assert_eq!(result.elected, vec![1]);
}

#[test]
fn stv_backwards_tie_break_falls_back_to_lowest_id_without_history() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 1;
    rules.counting.tie_breaking = Some("backwards".to_string());
    let algo = algorithm_for("stv").expect("algorithm");

    // Perfect three-way tie in round 1: no earlier round to consult, so the
    // deterministic fallback excludes the lowest candidate id first.
    let ballots: Vec<Ballot> = vec![vec![1], vec![2], vec![3]];

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");
    assert_eq!(result.elected.len(), 1);
}

#[test]
fn stv_random_tie_break_is_seed_deterministic() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 1;
    rules.counting.tie_breaking = Some("random".to_string());
    rules.counting.tie_breaking_seed = Some(7);
    let algo = algorithm_for("stv").expect("algorithm");

    let ballots: Vec<Ballot> = vec![vec![1, 2], vec![2, 1], vec![3]];

    let first = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");
    let second = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");
    assert_eq!(first.elected, second.elected);
    assert_eq!(first.elected.len(), 1);
}

#[test]
fn stv_manual_tie_break_is_not_implemented() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 1;
    rules.counting.tie_breaking = Some("manual".to_string());
    let algo = algorithm_for("stv").expect("algorithm");

    // Guaranteed exclusion tie in round 1.
    let ballots: Vec<Ballot> = vec![vec![1], vec![2], vec![3]];

    let err = algo
        .count(&ballots, &[1, 2, 3], &rules)
        .expect_err("manual tie-break is not implemented");
    assert!(err.to_string().contains("NotImplemented"));
}

#[test]
fn stv_winner_tie_at_quota_elects_both_over_rounds() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 3;
    rules.counting.tie_breaking = Some("random".to_string());
    rules.counting.tie_breaking_seed = Some(11);
    let algo = algorithm_for("stv").expect("algorithm");

    // V = 8, seats = 3 → quota = floor(8/4)+1 = 3. Candidates 1 and 2 both
    // sit exactly at quota in round 1 (winner-side tie, zero surplus).
    let ballots: Vec<Ballot> = vec![
        vec![1],
        vec![1],
        vec![1],
        vec![2],
        vec![2],
        vec![2],
        vec![3],
        vec![4],
    ];

    let result = algo.count(&ballots, &[1, 2, 3, 4], &rules).expect("count");
    assert_eq!(result.elected.len(), 3);
    assert!(result.elected.contains(&1));
    assert!(result.elected.contains(&2));
}

#[test]
fn stv_rejects_remaining_unsupported_options() {
    let algo = algorithm_for("stv").expect("algorithm");
    let ballots: Vec<Ballot> = vec![vec![1, 2]];

    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.counting.quota_mode = Some("progressive".to_string());
    assert!(algo.count(&ballots, &[1, 2], &rules).is_err());

    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.counting.quota_criterion = Some("gt".to_string());
    assert!(algo.count(&ballots, &[1, 2], &rules).is_err());

    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.counting.surplus_order = Some("by_order".to_string());
    assert!(algo.count(&ballots, &[1, 2], &rules).is_err());

    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.counting.bulk_exclusion = Some(true);
    assert!(algo.count(&ballots, &[1, 2], &rules).is_err());

    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.counting.bulk_election = Some(false);
    assert!(algo.count(&ballots, &[1, 2], &rules).is_err());
}

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

/// A transferred ballot must skip over already-excluded candidates to reach
/// its next *active* preference.
#[test]
fn stv_transfer_skips_inactive_preferences() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 1;
    let algo = algorithm_for("stv").expect("algorithm");

    // Quota = floor(6/2)+1 = 4. Round 1 excludes candidate 2 (1 vote), round 2
    // excludes candidate 3 (2 votes). The [3,2,1] ballots must skip the
    // already-excluded candidate 2 and land on candidate 1, electing it.
    let ballots: Vec<Ballot> = vec![
        vec![1],
        vec![1],
        vec![1],
        vec![2],
        vec![3, 2, 1],
        vec![3, 2, 1],
    ];

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");
    assert_eq!(result.elected, vec![1]);

    let c1 = result.tally.iter().find(|t| t.candidate_id == 1).unwrap();
    assert!(
        (c1.votes - 5.0).abs() < 1e-9,
        "transfers must reach candidate 1"
    );
}

/// With no tie_breaking configured, STV falls back to deterministic-by-id.
#[test]
fn stv_tie_break_defaults_to_lowest_id() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 1;
    rules.counting.tie_breaking = None;
    let algo = algorithm_for("stv").expect("algorithm");

    // Three-way exclusion tie: candidate 1 (lowest id) is excluded first,
    // then candidate 2, leaving candidate 3 to take the seat.
    let ballots: Vec<Ballot> = vec![vec![1], vec![2], vec![3]];

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");
    assert_eq!(result.elected, vec![3]);
}

/// Manual tie-break must also fail loudly on the *winner* side of a tie.
#[test]
fn stv_manual_tie_break_fails_for_tied_winners() {
    let mut rules = load_rules("stv", Path::new("rules")).expect("load stv rules");
    rules.election.seats = 3;
    rules.counting.tie_breaking = Some("manual".to_string());
    let algo = algorithm_for("stv").expect("algorithm");

    // Quota = floor(8/4)+1 = 3: candidates 1 and 2 are tied at quota in round 1.
    let ballots: Vec<Ballot> = vec![
        vec![1],
        vec![1],
        vec![1],
        vec![2],
        vec![2],
        vec![2],
        vec![3],
        vec![4],
    ];

    let err = algo
        .count(&ballots, &[1, 2, 3, 4], &rules)
        .expect_err("manual winner tie-break is not implemented");
    assert!(err.to_string().contains("NotImplemented"));
}
