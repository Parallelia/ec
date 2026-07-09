use anyhow::Result;
use secrecy::{ExposeSecret, SecretString};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::types::{AuthorizedVoter, Candidate, Election, RegistrationToken, Vote};

pub async fn create_election(
    pool: &SqlitePool,
    election: &Election,
    rsa_priv_key: &SecretString,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        r#"
        INSERT INTO elections (id, name, start_time, end_time, status, rules_id, rsa_pub_key, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        "#,
    )
    .bind(&election.id)
    .bind(&election.name)
    .bind(election.start_time)
    .bind(election.end_time)
    .bind(&election.status)
    .bind(&election.rules_id)
    .bind(&election.rsa_pub_key)
    .bind(election.created_at)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO election_keys (election_id, rsa_priv_key)
        VALUES (?1, ?2)
        "#,
    )
    .bind(&election.id)
    .bind(rsa_priv_key.expose_secret())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

pub async fn get_election_key(pool: &SqlitePool, election_id: &str) -> Result<Option<String>> {
    let key: Option<(String,)> = sqlx::query_as(
        r#"
        SELECT rsa_priv_key
        FROM election_keys
        WHERE election_id = ?1
        "#,
    )
    .bind(election_id)
    .fetch_optional(pool)
    .await?;

    Ok(key.map(|(k,)| k))
}

pub async fn get_election(pool: &SqlitePool, id: &str) -> Result<Option<Election>> {
    let election = sqlx::query_as::<_, Election>(
        r#"
        SELECT id, name, start_time, end_time, status, rules_id, rsa_pub_key, created_at, results_published
        FROM elections
        WHERE id = ?1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(election)
}

pub async fn list_elections(pool: &SqlitePool) -> Result<Vec<Election>> {
    let elections = sqlx::query_as::<_, Election>(
        r#"
        SELECT id, name, start_time, end_time, status, rules_id, rsa_pub_key, created_at, results_published
        FROM elections
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(elections)
}

pub async fn cancel_election(pool: &SqlitePool, election_id: &str) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE elections
        SET status = 'cancelled'
        WHERE id = ?1 AND status IN ('open', 'in_progress')
        "#,
    )
    .bind(election_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Fetch elections whose `start_time` has passed but are still `open`.
pub async fn elections_ready_to_start(pool: &SqlitePool, now: i64) -> Result<Vec<Election>> {
    let rows = sqlx::query_as::<_, Election>(
        r#"
        SELECT id, name, start_time, end_time, status, rules_id, rsa_pub_key, created_at, results_published
        FROM elections
        WHERE status = 'open' AND start_time <= ?1
        "#,
    )
    .bind(now)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Fetch elections whose `end_time` has passed but are still `in_progress`.
pub async fn elections_ready_to_finish(pool: &SqlitePool, now: i64) -> Result<Vec<Election>> {
    let rows = sqlx::query_as::<_, Election>(
        r#"
        SELECT id, name, start_time, end_time, status, rules_id, rsa_pub_key, created_at, results_published
        FROM elections
        WHERE status = 'in_progress' AND end_time <= ?1
        "#,
    )
    .bind(now)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Atomically transition an election from `open` to `in_progress`.
/// Returns rows affected (0 if already transitioned or not found).
pub async fn start_election(pool: &SqlitePool, election_id: &str) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE elections
        SET status = 'in_progress'
        WHERE id = ?1 AND status = 'open'
        "#,
    )
    .bind(election_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Atomically transition an election from `in_progress` to `finished`.
/// Returns rows affected (0 if already transitioned or not found).
pub async fn finish_election(pool: &SqlitePool, election_id: &str) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE elections
        SET status = 'finished'
        WHERE id = ?1 AND status = 'in_progress'
        "#,
    )
    .bind(election_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Fetch finished elections whose results have not yet been published.
pub async fn elections_pending_results(pool: &SqlitePool) -> Result<Vec<Election>> {
    let rows = sqlx::query_as::<_, Election>(
        r#"
        SELECT id, name, start_time, end_time, status, rules_id, rsa_pub_key, created_at, results_published
        FROM elections
        WHERE status = 'finished' AND results_published = 0
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Mark an election's results as successfully published.
pub async fn mark_results_published(pool: &SqlitePool, election_id: &str) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE elections
        SET results_published = 1
        WHERE id = ?1 AND status = 'finished' AND results_published = 0
        "#,
    )
    .bind(election_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

pub async fn add_candidate(pool: &SqlitePool, candidate: &Candidate) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO candidates (id, election_id, name)
        VALUES (?1, ?2, ?3)
        "#,
    )
    .bind(candidate.id)
    .bind(&candidate.election_id)
    .bind(&candidate.name)
    .execute(pool)
    .await?;

    Ok(())
}

/// Insert a candidate only if the referenced election has status = 'open'.
/// Returns the number of rows inserted (0 if election not found or not open).
pub async fn add_candidate_if_open(pool: &SqlitePool, candidate: &Candidate) -> Result<u64> {
    let result = sqlx::query(
        r#"
        INSERT INTO candidates (id, election_id, name)
        SELECT ?1, ?2, ?3
        WHERE EXISTS (
            SELECT 1 FROM elections WHERE id = ?2 AND status = 'open'
        )
        "#,
    )
    .bind(candidate.id)
    .bind(&candidate.election_id)
    .bind(&candidate.name)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

pub async fn get_candidates_for_election(
    pool: &SqlitePool,
    election_id: &str,
) -> Result<Vec<Candidate>> {
    let candidates = sqlx::query_as::<_, Candidate>(
        r#"
        SELECT id, election_id, name
        FROM candidates
        WHERE election_id = ?1
        ORDER BY id ASC
        "#,
    )
    .bind(election_id)
    .fetch_all(pool)
    .await?;

    Ok(candidates)
}

pub async fn insert_registration_tokens(
    tx: &mut Transaction<'_, Sqlite>,
    election_id: &str,
    tokens: &[String],
) -> Result<u64> {
    let mut total: u64 = 0;
    for token in tokens {
        let result = sqlx::query(
            r#"
            INSERT INTO registration_tokens (token, election_id)
            VALUES (?1, ?2)
            "#,
        )
        .bind(token)
        .bind(election_id)
        .execute(tx.as_mut())
        .await?;
        total += result.rows_affected();
    }

    Ok(total)
}

pub async fn list_registration_tokens(
    pool: &SqlitePool,
    election_id: &str,
) -> Result<Vec<RegistrationToken>> {
    let tokens = sqlx::query_as::<_, RegistrationToken>(
        r#"
        SELECT token, election_id, used, voter_pubkey, created_at, used_at
        FROM registration_tokens
        WHERE election_id = ?1
        ORDER BY created_at ASC
        "#,
    )
    .bind(election_id)
    .fetch_all(pool)
    .await?;

    Ok(tokens)
}

/// Atomically consume a registration token, marking it as used by the given voter.
/// Filters by both token and election_id to prevent cross-election token use.
pub async fn consume_registration_token(
    tx: &mut Transaction<'_, Sqlite>,
    token: &str,
    election_id: &str,
    voter_pubkey: &str,
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE registration_tokens
        SET used = 1, voter_pubkey = ?1, used_at = unixepoch()
        WHERE token = ?2 AND election_id = ?3 AND used = 0
        "#,
    )
    .bind(voter_pubkey)
    .bind(token)
    .bind(election_id)
    .execute(tx.as_mut())
    .await?;

    let rows = result.rows_affected();
    tracing::debug!(election_id, rows, "consume_registration_token");
    Ok(rows)
}

/// Insert an authorized voter record. Returns the number of rows affected.
/// Uses INSERT OR IGNORE so a duplicate (voter_pubkey, election_id) returns 0.
pub async fn authorize_voter(
    tx: &mut Transaction<'_, Sqlite>,
    election_id: &str,
    voter_pubkey: &str,
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        INSERT OR IGNORE INTO authorized_voters (voter_pubkey, election_id)
        VALUES (?1, ?2)
        "#,
    )
    .bind(voter_pubkey)
    .bind(election_id)
    .execute(tx.as_mut())
    .await?;

    let rows = result.rows_affected();
    tracing::debug!(election_id, rows, "authorize_voter");
    Ok(rows)
}

pub async fn get_authorized_voter(
    pool: &SqlitePool,
    election_id: &str,
    voter_pubkey: &str,
) -> Result<Option<AuthorizedVoter>> {
    let voter = sqlx::query_as::<_, AuthorizedVoter>(
        r#"
        SELECT voter_pubkey, election_id, registered_at, token_issued
        FROM authorized_voters
        WHERE election_id = ?1 AND voter_pubkey = ?2
        "#,
    )
    .bind(election_id)
    .bind(voter_pubkey)
    .fetch_optional(pool)
    .await?;

    Ok(voter)
}

pub async fn mark_token_issued(
    tx: &mut Transaction<'_, Sqlite>,
    election_id: &str,
    voter_pubkey: &str,
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE authorized_voters
        SET token_issued = 1
        WHERE election_id = ?1 AND voter_pubkey = ?2 AND token_issued = 0
        "#,
    )
    .bind(election_id)
    .bind(voter_pubkey)
    .execute(tx.as_mut())
    .await?;

    let rows = result.rows_affected();
    tracing::debug!(election_id, rows, "mark_token_issued");
    Ok(rows)
}

/// Atomically attempt to mark a nonce as used via INSERT.
/// Returns `true` if the nonce was newly inserted, `false` if it already existed
/// (relies on the PRIMARY KEY constraint on (h_n, election_id)).
/// The timestamp is truncated to the hour so it cannot be correlated with
/// relay traffic to deanonymize voters.
pub async fn try_mark_nonce_used(
    tx: &mut Transaction<'_, Sqlite>,
    election_id: &str,
    h_n: &str,
) -> Result<bool> {
    let result = sqlx::query(
        r#"
        INSERT OR IGNORE INTO used_nonces (h_n, election_id, recorded_at)
        VALUES (?1, ?2, ?3)
        "#,
    )
    .bind(h_n)
    .bind(election_id)
    .bind(truncate_to_hour(chrono::Utc::now().timestamp()))
    .execute(tx.as_mut())
    .await?;

    let was_new = result.rows_affected() == 1;
    tracing::debug!(election_id, was_new, "try_mark_nonce_used");
    Ok(was_new)
}

/// Truncate a unix timestamp to the start of its hour. Vote-related rows only
/// store coarse timestamps so they cannot be correlated with observed relay
/// traffic to link a vote to a voter.
pub fn truncate_to_hour(ts: i64) -> i64 {
    ts - ts.rem_euclid(3600)
}

pub async fn insert_vote_tx(tx: &mut Transaction<'_, Sqlite>, vote: &Vote) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO votes (election_id, candidate_ids, recorded_at)
        VALUES (?1, ?2, ?3)
        "#,
    )
    .bind(&vote.election_id)
    .bind(&vote.candidate_ids)
    .bind(vote.recorded_at)
    .execute(tx.as_mut())
    .await?;

    Ok(())
}

pub async fn get_votes_for_election(pool: &SqlitePool, election_id: &str) -> Result<Vec<Vote>> {
    let votes = sqlx::query_as::<_, Vote>(
        r#"
        SELECT id, election_id, candidate_ids, recorded_at
        FROM votes
        WHERE election_id = ?1
        ORDER BY id ASC
        "#,
    )
    .bind(election_id)
    .fetch_all(pool)
    .await?;

    Ok(votes)
}
