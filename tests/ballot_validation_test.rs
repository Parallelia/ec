use std::path::Path;

use ec::handlers::cast_vote::validate_ballot;
use ec::rules::load_rules;

#[test]
fn plurality_valid_single_choice() {
    let rules = load_rules("plurality", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3];
    assert!(validate_ballot(&[2], &rules, &candidates).is_ok());
}

#[test]
fn plurality_too_few_choices() {
    let rules = load_rules("plurality", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3];
    let err = validate_ballot(&[], &rules, &candidates).unwrap_err();
    assert!(err.to_string().contains("BALLOT_INVALID"));
    assert!(err.to_string().contains("Too few choices"));
}

#[test]
fn plurality_too_many_choices() {
    let rules = load_rules("plurality", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3];
    let err = validate_ballot(&[1, 2], &rules, &candidates).unwrap_err();
    assert!(err.to_string().contains("BALLOT_INVALID"));
    assert!(err.to_string().contains("Too many choices"));
}

#[test]
fn plurality_invalid_candidate() {
    let rules = load_rules("plurality", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3];
    let err = validate_ballot(&[99], &rules, &candidates).unwrap_err();
    assert!(err.to_string().contains("INVALID_CANDIDATE"));
}

#[test]
fn stv_valid_ranked_ballot() {
    let rules = load_rules("stv", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3, 4];
    assert!(validate_ballot(&[3, 1, 4, 2], &rules, &candidates).is_ok());
}

#[test]
fn stv_partial_ranking_valid() {
    let rules = load_rules("stv", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3, 4];
    // min_choices is 1 for STV, so partial ranking is valid
    assert!(validate_ballot(&[3, 1], &rules, &candidates).is_ok());
}

#[test]
fn stv_duplicate_candidate_rejected() {
    let rules = load_rules("stv", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3, 4];
    let err = validate_ballot(&[1, 2, 1], &rules, &candidates).unwrap_err();
    assert!(err.to_string().contains("BALLOT_INVALID"));
    assert!(err.to_string().contains("Duplicate candidate"));
}

#[test]
fn stv_too_few_choices() {
    let rules = load_rules("stv", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3, 4];
    let err = validate_ballot(&[], &rules, &candidates).unwrap_err();
    assert!(err.to_string().contains("BALLOT_INVALID"));
    assert!(err.to_string().contains("Too few choices"));
}

#[test]
fn stv_invalid_candidate() {
    let rules = load_rules("stv", Path::new("rules")).expect("load rules");
    let candidates = vec![1, 2, 3, 4];
    let err = validate_ballot(&[1, 2, 99], &rules, &candidates).unwrap_err();
    assert!(err.to_string().contains("INVALID_CANDIDATE"));
}

#[test]
fn ballot_longer_than_u8_max_is_rejected_before_counting() {
    let rules = load_rules("stv", Path::new("rules")).expect("load rules");
    let candidates: Vec<u8> = (0..=255).collect();
    // 300 entries: longer than any possible candidate set (u8::MAX).
    let oversized: Vec<u8> = (0..300).map(|i| (i % 256) as u8).collect();
    let err = validate_ballot(&oversized, &rules, &candidates).unwrap_err();
    assert!(err.to_string().contains("Too many choices"));
}
