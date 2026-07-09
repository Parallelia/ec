use std::path::Path;

use ec::counting::{Ballot, CandidateStatus, algorithm_for};
use ec::rules::load_rules;

#[test]
fn plurality_single_winner_simple_majority() {
    let rules = load_rules("plurality", Path::new("rules")).expect("load plurality rules");
    let algo = algorithm_for("plurality").expect("algorithm");

    // 5 ballots, candidate 1 has 3 votes, candidate 2 has 2 votes.
    let ballots: Vec<Ballot> = vec![vec![1], vec![1], vec![1], vec![2], vec![2]];

    let result = algo.count(&ballots, &[1, 2], &rules).expect("count");

    assert_eq!(result.elected, vec![1]);
    let c1 = result.tally.iter().find(|t| t.candidate_id == 1).unwrap();
    let c2 = result.tally.iter().find(|t| t.candidate_id == 2).unwrap();
    assert_eq!(c1.votes, 3.0);
    assert_eq!(c2.votes, 2.0);
}

#[test]
fn plurality_tie_broken_deterministically() {
    // plurality.toml configures tie_breaking = "random" with no explicit seed,
    // which falls back to a deterministic seed derived from the candidate set.
    let rules = load_rules("plurality", Path::new("rules")).expect("load plurality rules");
    let algo = algorithm_for("plurality").expect("algorithm");

    // 2 votes each: candidates 1 and 2 are tied for the single seat.
    let ballots: Vec<Ballot> = vec![vec![1], vec![2], vec![1], vec![2]];

    let first = algo.count(&ballots, &[1, 2], &rules).expect("count");
    assert_eq!(first.elected.len(), 1);
    assert!(first.elected[0] == 1 || first.elected[0] == 2);

    // Auditable: recounting produces the identical result.
    let second = algo.count(&ballots, &[1, 2], &rules).expect("count");
    assert_eq!(first.elected, second.elected);
}

#[test]
fn plurality_multi_seat_top_two_elected() {
    let mut rules = load_rules("plurality", Path::new("rules")).expect("load plurality rules");
    // Override to a 2-seat election.
    rules.election.seats = 2;
    let algo = algorithm_for("plurality").expect("algorithm");

    // Candidate 1: 3 votes, candidate 2: 2 votes, candidate 3: 1 vote.
    let ballots: Vec<Ballot> = vec![vec![1], vec![1], vec![1], vec![2], vec![2], vec![3]];

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");
    assert_eq!(result.elected, vec![1, 2]);
}

#[test]
fn plurality_zero_vote_candidate_appears_in_tally() {
    let rules = load_rules("plurality", Path::new("rules")).expect("load plurality rules");
    let algo = algorithm_for("plurality").expect("algorithm");

    // Candidate 3 receives no votes but is a registered candidate.
    let ballots: Vec<Ballot> = vec![vec![1], vec![1], vec![2]];

    let result = algo.count(&ballots, &[1, 2, 3], &rules).expect("count");

    let c3 = result
        .tally
        .iter()
        .find(|t| t.candidate_id == 3)
        .expect("zero-vote candidate must appear in the published tally");
    assert_eq!(c3.votes, 0.0);
    assert_eq!(c3.status, CandidateStatus::Excluded);
}

#[test]
fn plurality_min_winner_votes_leaves_seat_unfilled() {
    let mut rules = load_rules("plurality", Path::new("rules")).expect("load plurality rules");
    rules.counting.min_winner_votes = Some(3);
    let algo = algorithm_for("plurality").expect("algorithm");

    // Top candidate only has 2 votes — below the threshold.
    let ballots: Vec<Ballot> = vec![vec![1], vec![1], vec![2]];

    let result = algo.count(&ballots, &[1, 2], &rules).expect("count");
    assert!(result.elected.is_empty());
}

#[test]
fn plurality_tie_breaking_none_declares_no_winner() {
    let mut rules = load_rules("plurality", Path::new("rules")).expect("load plurality rules");
    rules.counting.tie_breaking = Some("none".to_string());
    let algo = algorithm_for("plurality").expect("algorithm");

    let ballots: Vec<Ballot> = vec![vec![1], vec![2]];

    let result = algo.count(&ballots, &[1, 2], &rules).expect("count");
    assert!(
        result.elected.is_empty(),
        "tie with 'none' must not elect anyone"
    );
}
