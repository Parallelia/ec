use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use ec::types::Election;
use ec::{crypto, db};

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

// --- End-to-end scheduler loop tests (fake relay) ---

mod common;

use ec::types::Vote;
use nostr_sdk::prelude::{Client, Keys};
use std::time::Duration;

async fn online_client(relay_url: &str) -> Client {
    let client = Client::builder().signer(Keys::generate()).build();
    client.add_relay(relay_url).await.unwrap();
    client.connect().await;
    client
}

async fn wait_until<F, Fut>(mut condition: F, timeout: Duration) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if condition().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn scheduler_transitions_counts_and_publishes() {
    let relay_url = common::start_fake_relay().await;
    let pool = setup_pool().await;
    let now = chrono::Utc::now().timestamp();

    // Should flip open → in_progress.
    let (starting, sk) = make_election("starting", now - 60, now + 3600, "open");
    db::create_election(&pool, &starting, &sk).await.unwrap();

    // Should flip in_progress → finished, then count + publish results.
    let (finishing, sk) = make_election("finishing", now - 7200, now - 60, "in_progress");
    db::create_election(&pool, &finishing, &sk).await.unwrap();
    for id in [1u8, 2] {
        db::add_candidate(
            &pool,
            &ec::types::Candidate {
                id,
                election_id: "finishing".to_string(),
                name: format!("C{id}"),
            },
        )
        .await
        .unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    for ballot in ["[1]", "[1]", "[2]"] {
        db::insert_vote_tx(
            &mut tx,
            &Vote {
                id: 0,
                election_id: "finishing".to_string(),
                candidate_ids: ballot.to_string(),
                recorded_at: 0,
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    let client = online_client(&relay_url).await;
    let handle = tokio::spawn(ec::scheduler::run(
        pool.clone(),
        client,
        std::path::PathBuf::from("rules"),
    ));

    let done = wait_until(
        || {
            let pool = pool.clone();
            async move {
                let started = db::get_election(&pool, "starting").await.unwrap().unwrap();
                let finished = db::get_election(&pool, "finishing").await.unwrap().unwrap();
                started.status == "in_progress"
                    && finished.status == "finished"
                    && finished.results_published == 1
            }
        },
        Duration::from_secs(10),
    )
    .await;
    handle.abort();

    assert!(
        done,
        "scheduler must transition, count and publish within the first tick"
    );
}

#[tokio::test]
async fn scheduler_retries_when_publishing_fails() {
    let pool = setup_pool().await;
    let now = chrono::Utc::now().timestamp();

    // Ready to start: transition happens even though the announcement
    // republish fails (offline client).
    let (starting, sk) = make_election("starting", now - 60, now + 3600, "open");
    db::create_election(&pool, &starting, &sk).await.unwrap();

    // Finished election whose result publish will fail → stays pending.
    let (finished, sk) = make_election("finished", now - 7200, now - 3600, "finished");
    db::create_election(&pool, &finished, &sk).await.unwrap();

    // Finished election with unknown rules → counting itself fails.
    let (bad_rules, sk) = make_election("bad-rules", now - 7200, now - 3600, "finished");
    let mut bad_rules = bad_rules;
    bad_rules.rules_id = "no-such-rules".to_string();
    db::create_election(&pool, &bad_rules, &sk).await.unwrap();

    // Offline client: every publish fails.
    let client = Client::builder().signer(Keys::generate()).build();
    let handle = tokio::spawn(ec::scheduler::run(
        pool.clone(),
        client,
        std::path::PathBuf::from("rules"),
    ));

    let started = wait_until(
        || {
            let pool = pool.clone();
            async move {
                db::get_election(&pool, "starting")
                    .await
                    .unwrap()
                    .unwrap()
                    .status
                    == "in_progress"
            }
        },
        Duration::from_secs(10),
    )
    .await;
    assert!(started, "transition must happen even if republish fails");

    // Give the tick time to attempt (and fail) result publishing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.abort();

    for id in ["finished", "bad-rules"] {
        let e = db::get_election(&pool, id).await.unwrap().unwrap();
        assert_eq!(e.results_published, 0, "{id} must stay pending for retry");
    }
}

#[tokio::test]
async fn scheduler_survives_tick_errors() {
    let pool = setup_pool().await;
    pool.close().await;

    let client = Client::builder().signer(Keys::generate()).build();
    let handle = tokio::spawn(ec::scheduler::run(
        pool,
        client,
        std::path::PathBuf::from("rules"),
    ));

    // The first tick fails against the closed pool; the loop must not panic.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !handle.is_finished(),
        "scheduler loop must keep running after a failed tick"
    );
    handle.abort();
}
