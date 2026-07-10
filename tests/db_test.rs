//! Direct coverage of the SQLite query layer in `ec::db`.

mod common;

use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use ec::db;
use ec::types::{Candidate, Election, Vote};

async fn setup_pool() -> SqlitePool {
    common::init_tracing();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

fn make_election(id: &str, status: &str, created_at: i64) -> (Election, SecretString) {
    let election = Election {
        id: id.to_string(),
        name: format!("Election {id}"),
        start_time: 1000,
        end_time: 2000,
        status: status.to_string(),
        rules_id: "plurality".to_string(),
        rsa_pub_key: "fake-public-key".to_string(),
        created_at,
        results_published: 0,
    };
    (
        election,
        SecretString::new("fake-private-key".to_string().into_boxed_str()),
    )
}

async fn seed(pool: &SqlitePool, id: &str, status: &str, created_at: i64) {
    let (e, sk) = make_election(id, status, created_at);
    db::create_election(pool, &e, &sk).await.unwrap();
}

#[tokio::test]
async fn create_election_stores_key_and_duplicate_id_fails() {
    let pool = setup_pool().await;
    seed(&pool, "e1", "open", 100).await;

    let key = db::get_election_key(&pool, "e1").await.unwrap();
    assert_eq!(key.as_deref(), Some("fake-private-key"));

    // Missing key for unknown election.
    let key = db::get_election_key(&pool, "nope").await.unwrap();
    assert!(key.is_none());

    // Duplicate primary key must fail loudly.
    let (e, sk) = make_election("e1", "open", 100);
    assert!(db::create_election(&pool, &e, &sk).await.is_err());
}

#[tokio::test]
async fn get_election_returns_none_for_unknown_id() {
    let pool = setup_pool().await;
    assert!(db::get_election(&pool, "missing").await.unwrap().is_none());
}

#[tokio::test]
async fn list_elections_orders_by_created_at_desc() {
    let pool = setup_pool().await;
    seed(&pool, "older", "open", 100).await;
    seed(&pool, "newer", "open", 200).await;

    let elections = db::list_elections(&pool).await.unwrap();
    assert_eq!(elections.len(), 2);
    assert_eq!(elections[0].id, "newer");
    assert_eq!(elections[1].id, "older");
}

#[tokio::test]
async fn cancel_election_only_affects_open_or_in_progress() {
    let pool = setup_pool().await;
    seed(&pool, "open-e", "open", 100).await;
    seed(&pool, "progress-e", "in_progress", 100).await;
    seed(&pool, "finished-e", "finished", 100).await;

    assert_eq!(db::cancel_election(&pool, "open-e").await.unwrap(), 1);
    assert_eq!(db::cancel_election(&pool, "progress-e").await.unwrap(), 1);
    assert_eq!(db::cancel_election(&pool, "finished-e").await.unwrap(), 0);
    assert_eq!(db::cancel_election(&pool, "missing").await.unwrap(), 0);

    let e = db::get_election(&pool, "open-e").await.unwrap().unwrap();
    assert_eq!(e.status, "cancelled");

    // Already cancelled → second cancel is a no-op.
    assert_eq!(db::cancel_election(&pool, "open-e").await.unwrap(), 0);
}

#[tokio::test]
async fn add_candidate_and_fetch_ordered() {
    let pool = setup_pool().await;
    seed(&pool, "e1", "open", 100).await;

    db::add_candidate(
        &pool,
        &Candidate {
            id: 2,
            election_id: "e1".to_string(),
            name: "Bob".to_string(),
        },
    )
    .await
    .unwrap();
    db::add_candidate(
        &pool,
        &Candidate {
            id: 1,
            election_id: "e1".to_string(),
            name: "Alice".to_string(),
        },
    )
    .await
    .unwrap();

    let candidates = db::get_candidates_for_election(&pool, "e1").await.unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].id, 1);
    assert_eq!(candidates[0].name, "Alice");
    assert_eq!(candidates[1].id, 2);
}

#[tokio::test]
async fn add_candidate_if_open_rejects_non_open_elections() {
    let pool = setup_pool().await;
    seed(&pool, "open-e", "open", 100).await;
    seed(&pool, "progress-e", "in_progress", 100).await;

    let c = |election: &str| Candidate {
        id: 1,
        election_id: election.to_string(),
        name: "Alice".to_string(),
    };

    assert_eq!(
        db::add_candidate_if_open(&pool, &c("open-e"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        db::add_candidate_if_open(&pool, &c("progress-e"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db::add_candidate_if_open(&pool, &c("missing"))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn registration_token_lifecycle() {
    let pool = setup_pool().await;
    seed(&pool, "e1", "open", 100).await;
    seed(&pool, "e2", "open", 100).await;

    let tokens = vec!["tok-a".to_string(), "tok-b".to_string()];
    let mut tx = pool.begin().await.unwrap();
    let inserted = db::insert_registration_tokens(&mut tx, "e1", &tokens)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(inserted, 2);

    let listed = db::list_registration_tokens(&pool, "e1").await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(
        listed
            .iter()
            .all(|t| t.used == 0 && t.voter_pubkey.is_none())
    );

    // Consuming with the wrong election id must not work (cross-election use).
    let mut tx = pool.begin().await.unwrap();
    let rows = db::consume_registration_token(&mut tx, "tok-a", "e2", "voter-1")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rows, 0);

    // Correct election consumes exactly once.
    let mut tx = pool.begin().await.unwrap();
    let rows = db::consume_registration_token(&mut tx, "tok-a", "e1", "voter-1")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rows, 1);

    let mut tx = pool.begin().await.unwrap();
    let rows = db::consume_registration_token(&mut tx, "tok-a", "e1", "voter-2")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rows, 0);

    let listed = db::list_registration_tokens(&pool, "e1").await.unwrap();
    let consumed = listed.iter().find(|t| t.token == "tok-a").unwrap();
    assert_eq!(consumed.used, 1);
    assert_eq!(consumed.voter_pubkey.as_deref(), Some("voter-1"));
    assert!(consumed.used_at.is_some());
}

#[tokio::test]
async fn authorize_voter_and_token_issuance() {
    let pool = setup_pool().await;
    seed(&pool, "e1", "open", 100).await;

    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        db::authorize_voter(&mut tx, "e1", "voter-1").await.unwrap(),
        1
    );
    // Duplicate authorization is ignored (INSERT OR IGNORE).
    assert_eq!(
        db::authorize_voter(&mut tx, "e1", "voter-1").await.unwrap(),
        0
    );
    tx.commit().await.unwrap();

    let voter = db::get_authorized_voter(&pool, "e1", "voter-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(voter.token_issued, 0);

    assert!(
        db::get_authorized_voter(&pool, "e1", "someone-else")
            .await
            .unwrap()
            .is_none()
    );

    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        db::mark_token_issued(&mut tx, "e1", "voter-1")
            .await
            .unwrap(),
        1
    );
    // Second issuance attempt returns 0 rows.
    assert_eq!(
        db::mark_token_issued(&mut tx, "e1", "voter-1")
            .await
            .unwrap(),
        0
    );
    // Unknown voter returns 0 rows.
    assert_eq!(
        db::mark_token_issued(&mut tx, "e1", "ghost").await.unwrap(),
        0
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn nonce_is_single_use_and_hour_truncated() {
    let pool = setup_pool().await;
    seed(&pool, "e1", "open", 100).await;

    let mut tx = pool.begin().await.unwrap();
    assert!(
        db::try_mark_nonce_used(&mut tx, "e1", "abc123")
            .await
            .unwrap()
    );
    // Same nonce again → already used.
    assert!(
        !db::try_mark_nonce_used(&mut tx, "e1", "abc123")
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();

    // The stored timestamp must be truncated to the hour (anonymity rule).
    let (recorded_at,): (i64,) =
        sqlx::query_as("SELECT recorded_at FROM used_nonces WHERE h_n = 'abc123'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(recorded_at % 3600, 0);
}

#[tokio::test]
async fn votes_are_stored_and_fetched_in_insertion_order() {
    let pool = setup_pool().await;
    seed(&pool, "e1", "open", 100).await;

    let mut tx = pool.begin().await.unwrap();
    for candidate_ids in ["[1]", "[2]", "[1,3]"] {
        db::insert_vote_tx(
            &mut tx,
            &Vote {
                id: 0,
                election_id: "e1".to_string(),
                candidate_ids: candidate_ids.to_string(),
                recorded_at: 3600,
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    let votes = db::get_votes_for_election(&pool, "e1").await.unwrap();
    assert_eq!(votes.len(), 3);
    assert_eq!(votes[0].candidate_ids, "[1]");
    assert_eq!(votes[2].candidate_ids, "[1,3]");

    assert!(
        db::get_votes_for_election(&pool, "empty")
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn truncate_to_hour_rounds_down() {
    assert_eq!(db::truncate_to_hour(0), 0);
    assert_eq!(db::truncate_to_hour(3600), 3600);
    assert_eq!(db::truncate_to_hour(3661), 3600);
    assert_eq!(db::truncate_to_hour(7199), 3600);
    // Negative timestamps still round towards minus infinity.
    assert_eq!(db::truncate_to_hour(-1), -3600);
}
