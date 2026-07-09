use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use ec::types::Election;
use ec::{crypto, db};

async fn setup_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

fn make_election(id: &str, start: i64, end: i64, status: &str) -> (Election, SecretString) {
    let (pk, sk) = crypto::generate_keypair().unwrap();
    let election = Election {
        id: id.to_string(),
        name: format!("Election {id}"),
        start_time: start,
        end_time: end,
        status: status.to_string(),
        rules_id: "plurality".to_string(),
        rsa_pub_key: pk,
        created_at: 1000,
        results_published: 0,
    };
    (election, SecretString::new(sk.into_boxed_str()))
}

#[tokio::test]
async fn start_election_when_start_time_reached() {
    let pool = setup_pool().await;
    let now = 2000_i64;

    // Election with start_time in the past → should transition.
    let (e, sk) = make_election("e1", 1500, 3000, "open");
    db::create_election(&pool, &e, &sk).await.unwrap();

    // Election with start_time in the future → should NOT transition.
    let (e2, sk2) = make_election("e2", 2500, 4000, "open");
    db::create_election(&pool, &e2, &sk2).await.unwrap();

    let ready = db::elections_ready_to_start(&pool, now).await.unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, "e1");

    let rows = db::start_election(&pool, "e1").await.unwrap();
    assert_eq!(rows, 1);

    // Verify status changed.
    let updated = db::get_election(&pool, "e1").await.unwrap().unwrap();
    assert_eq!(updated.status, "in_progress");

    // Idempotent: second call returns 0.
    let rows = db::start_election(&pool, "e1").await.unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn finish_election_when_end_time_reached() {
    let pool = setup_pool().await;
    let now = 5000_i64;

    let (e, sk) = make_election("e1", 1000, 4000, "open");
    db::create_election(&pool, &e, &sk).await.unwrap();
    db::start_election(&pool, "e1").await.unwrap();

    let ready = db::elections_ready_to_finish(&pool, now).await.unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, "e1");

    let rows = db::finish_election(&pool, "e1").await.unwrap();
    assert_eq!(rows, 1);

    let updated = db::get_election(&pool, "e1").await.unwrap().unwrap();
    assert_eq!(updated.status, "finished");

    // Idempotent.
    let rows = db::finish_election(&pool, "e1").await.unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn cancelled_election_not_transitioned() {
    let pool = setup_pool().await;
    let now = 5000_i64;

    let (e, sk) = make_election("e1", 1000, 2000, "open");
    db::create_election(&pool, &e, &sk).await.unwrap();
    db::cancel_election(&pool, "e1").await.unwrap();

    // Cancelled elections should not appear in either query.
    let ready_start = db::elections_ready_to_start(&pool, now).await.unwrap();
    assert!(ready_start.is_empty());

    let ready_finish = db::elections_ready_to_finish(&pool, now).await.unwrap();
    assert!(ready_finish.is_empty());
}

#[tokio::test]
async fn pending_results_retried_until_published() {
    let pool = setup_pool().await;

    let (e, sk) = make_election("e1", 1000, 2000, "open");
    db::create_election(&pool, &e, &sk).await.unwrap();
    db::start_election(&pool, "e1").await.unwrap();
    db::finish_election(&pool, "e1").await.unwrap();

    // Finished but not published → should appear in pending.
    let pending = db::elections_pending_results(&pool).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "e1");

    // Mark as published.
    let rows = db::mark_results_published(&pool, "e1").await.unwrap();
    assert_eq!(rows, 1);

    // No longer pending.
    let pending = db::elections_pending_results(&pool).await.unwrap();
    assert!(pending.is_empty());

    // Idempotent.
    let rows = db::mark_results_published(&pool, "e1").await.unwrap();
    assert_eq!(rows, 0);
}
