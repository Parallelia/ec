//! Coverage of election rule loading from TOML files.

use std::path::Path;

use ec::rules::{BallotMethod, load_rules};

#[test]
fn loads_plurality_rules_with_expected_fields() {
    let rules = load_rules("plurality", Path::new("rules")).expect("load plurality");
    assert_eq!(rules.meta.id, "plurality");
    assert_eq!(rules.ballot.method, BallotMethod::Single);
    assert_eq!(rules.ballot.min_choices, 1);
    assert_eq!(rules.ballot.max_choices, 1);
    assert_eq!(rules.election.seats, 1);
    assert_eq!(rules.counting.algorithm, "plurality");
    assert_eq!(rules.results.publish_tally, "live");
}

#[test]
fn loads_stv_rules_with_optional_counting_fields() {
    let rules = load_rules("stv", Path::new("rules")).expect("load stv");
    assert_eq!(rules.meta.id, "stv");
    assert_eq!(rules.ballot.method, BallotMethod::Ranked);
    assert_eq!(rules.counting.quota.as_deref(), Some("droop"));
    assert_eq!(
        rules.counting.transfer_method.as_deref(),
        Some("weighted_inclusive_gregory")
    );
    assert_eq!(rules.results.publish_count_sheet, Some(true));
}

#[test]
fn missing_rules_file_is_an_error() {
    let err = load_rules("does-not-exist", Path::new("rules")).expect_err("must fail");
    assert!(err.to_string().contains("Rules file not found"));
}

#[test]
fn malformed_rules_file_is_a_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.toml"), "this is not [valid toml").unwrap();

    let err = load_rules("broken", dir.path()).expect_err("must fail");
    assert!(err.to_string().contains("Failed to parse rules file"));
}

#[test]
fn structurally_valid_toml_with_missing_sections_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("partial.toml"), "[meta]\nname = \"x\"\n").unwrap();

    assert!(load_rules("partial", dir.path()).is_err());
}
