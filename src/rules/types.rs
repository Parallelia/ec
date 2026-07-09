use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ElectionRules {
    pub meta: RulesMeta,
    pub election: ElectionConfig,
    pub ballot: BallotConfig,
    pub counting: CountingConfig,
    pub results: ResultsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RulesMeta {
    pub name: String,
    pub id: String,
    pub version: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ElectionConfig {
    pub seats: u8,
    pub min_candidates: u8,
    pub max_candidates: u8,
    pub voting_required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BallotConfig {
    pub method: BallotMethod,
    pub min_choices: u8,
    pub max_choices: u8,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BallotMethod {
    Single,
    Ranked,
    Approval,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CountingConfig {
    pub algorithm: String,
    #[serde(default)]
    pub quota: Option<String>,
    #[serde(default)]
    pub quota_mode: Option<String>,
    #[serde(default)]
    pub quota_criterion: Option<String>,
    #[serde(default)]
    pub transfer_method: Option<String>,
    #[serde(default)]
    pub surplus_order: Option<String>,
    #[serde(default)]
    pub exclusion_method: Option<String>,
    #[serde(default)]
    pub bulk_exclusion: Option<bool>,
    #[serde(default)]
    pub bulk_election: Option<bool>,
    #[serde(default)]
    pub tie_breaking: Option<String>,
    #[serde(default)]
    pub tie_breaking_seed: Option<u64>,
    #[serde(default)]
    pub min_winner_votes: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultsConfig {
    pub publish_tally: String,
    #[serde(default)]
    pub publish_count_sheet: Option<bool>,
    #[serde(default)]
    pub publish_counts: Option<bool>,
    #[serde(default)]
    pub publish_total_votes: Option<bool>,
    #[serde(default)]
    pub publish_turnout: Option<bool>,
}

/// Convenience type re-exported for counting engine.
pub type RulesPath = PathBuf;
