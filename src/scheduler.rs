use std::time::Duration;

use anyhow::Result;
use nostr_sdk::prelude::Client;
use sqlx::SqlitePool;
use std::path::Path;

use crate::{counting, db, nostr, rules};

const TICK_INTERVAL: Duration = Duration::from_secs(30);

/// Run the scheduler loop: every 30 seconds, check for elections that need
/// status transitions (open → in_progress, in_progress → finished).
pub async fn run(pool: SqlitePool, nostr_client: Client, rules_dir: std::path::PathBuf) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);

    loop {
        interval.tick().await;

        if let Err(e) = tick(&pool, &nostr_client, &rules_dir).await {
            tracing::error!(error = %e, "Scheduler tick failed");
        }
    }
}

async fn tick(pool: &SqlitePool, nostr_client: &Client, rules_dir: &Path) -> Result<()> {
    let now = chrono::Utc::now().timestamp();

    // Transition open → in_progress for elections whose start_time has arrived.
    let to_start = db::elections_ready_to_start(pool, now).await?;
    for election in &to_start {
        let rows = db::start_election(pool, &election.id).await?;
        if rows > 0 {
            tracing::info!(election_id = %election.id, "Election started (open → in_progress)");
            republish_announcement(pool, nostr_client, &election.id).await;
        }
    }

    // Transition in_progress → finished for elections whose end_time has passed.
    let to_finish = db::elections_ready_to_finish(pool, now).await?;
    for election in &to_finish {
        let rows = db::finish_election(pool, &election.id).await?;
        if rows > 0 {
            tracing::info!(election_id = %election.id, "Election finished (in_progress → finished)");
            republish_announcement(pool, nostr_client, &election.id).await;
        }
    }

    // Count and publish results for any finished elections that haven't been published yet.
    // This covers both newly finished elections and retries from previous failures.
    let pending = db::elections_pending_results(pool).await?;
    for election in &pending {
        match count_and_publish(pool, nostr_client, election, rules_dir).await {
            Ok(()) => {
                db::mark_results_published(pool, &election.id).await?;
                tracing::info!(election_id = %election.id, "Results published");
            }
            Err(e) => {
                tracing::error!(
                    election_id = %election.id,
                    error = %e,
                    "Failed to count/publish results — will retry next tick"
                );
            }
        }
    }

    Ok(())
}

/// Re-publish the Kind 35000 announcement so relay clients see the updated
/// election status. Failures are logged, never fatal: the announcement is a
/// replaceable event and the next state change will publish a fresh one.
async fn republish_announcement(pool: &SqlitePool, nostr_client: &Client, election_id: &str) {
    let result: Result<()> = async {
        let election = db::get_election(pool, election_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("election not found"))?;
        let candidates = db::get_candidates_for_election(pool, election_id).await?;
        nostr::publisher::publish_election_event(nostr_client, &election, &candidates).await?;
        Ok(())
    }
    .await;

    if let Err(e) = result {
        tracing::warn!(
            election_id = %election_id,
            error = %e,
            "Failed to re-publish election announcement"
        );
    }
}

/// Load rules, count ballots, and publish the result event to Nostr.
async fn count_and_publish(
    pool: &SqlitePool,
    nostr_client: &Client,
    election: &crate::types::Election,
    rules_dir: &Path,
) -> Result<()> {
    let rules = rules::load_rules(&election.rules_id, rules_dir)?;
    let algorithm = counting::algorithm_for(&election.rules_id)?;

    let votes = db::get_votes_for_election(pool, &election.id).await?;
    let ballots: Vec<counting::Ballot> = votes
        .iter()
        .map(|v| serde_json::from_str(&v.candidate_ids))
        .collect::<Result<_, _>>()?;

    let candidates = db::get_candidates_for_election(pool, &election.id).await?;
    let candidate_ids: Vec<u8> = candidates.iter().map(|c| c.id).collect();

    let result = algorithm.count(&ballots, &candidate_ids, &rules)?;

    tracing::info!(
        election_id = %election.id,
        elected = ?result.elected,
        total_ballots = ballots.len(),
        "Vote counting complete"
    );

    nostr::publisher::publish_result_event(nostr_client, election, &result).await?;

    Ok(())
}
