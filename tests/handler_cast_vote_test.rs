use base64::Engine;
use blind_rsa_signatures::{DefaultRng, PSS, Randomized, Sha384};
use secrecy::SecretString;
use sha2::Digest;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;

use ec::crypto;
use ec::db;
use ec::handlers::cast_vote;
use ec::types::{Candidate, Election};

async fn setup_db() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

struct TestElection {
    pk_b64: String,
    sk_b64: String,
}

impl TestElection {
    fn new() -> Self {
        let (pk_b64, sk_b64) = crypto::generate_keypair().unwrap();
        Self { pk_b64, sk_b64 }
    }
}

async fn seed_election(pool: &SqlitePool, te: &TestElection, rules_id: &str) {
    let now = chrono::Utc::now().timestamp();
    seed_election_with_times(pool, te, rules_id, now - 100, now + 3600).await;
}

async fn seed_election_with_times(
    pool: &SqlitePool,
    te: &TestElection,
    rules_id: &str,
    start_time: i64,
    end_time: i64,
) {
    let election = Election {
        id: "test-election-1".to_string(),
        name: "Test Election".to_string(),
        start_time,
        end_time,
        status: "in_progress".to_string(),
        rules_id: rules_id.to_string(),
        rsa_pub_key: te.pk_b64.clone(),
        created_at: start_time,
        results_published: 0,
    };
    db::create_election(
        pool,
        &election,
        &SecretString::new(te.sk_b64.clone().into()),
    )
    .await
    .unwrap();
}

async fn seed_candidates(pool: &SqlitePool, election_id: &str, ids: &[u8]) {
    for &id in ids {
        let candidate = Candidate {
            id,
            election_id: election_id.to_string(),
            name: format!("Candidate {id}"),
        };
        db::add_candidate(pool, &candidate).await.unwrap();
    }
}

/// Simulate the full blind signature protocol to create a valid voting token.
/// Returns (h_n_hex, token_b64) ready to submit with cast-vote.
fn create_valid_token(pk_b64: &str, sk_b64: &str) -> (String, String) {
    // 1. Generate nonce and hash it (voter side)
    let nonce = crypto::generate_nonce();
    let h_n = sha2::Sha256::digest(nonce);
    let h_n_bytes: &[u8] = h_n.as_slice();
    let h_n_hex = hex::encode(h_n_bytes);

    // 2. Blind the hash (voter side, using EC's public key)
    let pk_der = base64::engine::general_purpose::STANDARD
        .decode(pk_b64)
        .unwrap();
    let pk = blind_rsa_signatures::PublicKey::<Sha384, PSS, Randomized>::from_der(&pk_der).unwrap();
    let mut rng = DefaultRng;
    let blinding_result = pk.blind(&mut rng, h_n_bytes).unwrap();

    // 3. EC blind-signs the blinded message
    let blind_sig = crypto::blind_sign(sk_b64, &blinding_result.blind_message).unwrap();

    // 4. Voter finalizes the signature
    let sig = pk
        .finalize(&blind_sig.into(), &blinding_result, h_n_bytes)
        .unwrap();

    // 5. Pack signature + msg_randomizer into token
    let randomizer = blinding_result
        .msg_randomizer
        .expect("Randomized mode must have a randomizer");
    let mut token_bytes = sig.to_vec();
    token_bytes.extend_from_slice(randomizer.as_ref());
    let token_b64 = base64::engine::general_purpose::STANDARD.encode(&token_bytes);

    (h_n_hex, token_b64)
}

#[tokio::test]
async fn cast_vote_success_plurality() {
    let pool = setup_db().await;
    let te = TestElection::new();
    seed_election(&pool, &te, "plurality").await;
    seed_candidates(&pool, "test-election-1", &[1, 2, 3]).await;

    let (h_n, token) = create_valid_token(&te.pk_b64, &te.sk_b64);

    let response = cast_vote::handle(
        &pool,
        "test-election-1",
        &[2],
        &h_n,
        &token,
        Path::new("rules"),
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["action"], "vote-recorded");

    // Verify vote was stored
    let votes = db::get_votes_for_election(&pool, "test-election-1")
        .await
        .unwrap();
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].candidate_ids, "[2]");
}

#[tokio::test]
async fn cast_vote_success_stv_ranked() {
    let pool = setup_db().await;
    let te = TestElection::new();
    seed_election(&pool, &te, "stv").await;
    seed_candidates(&pool, "test-election-1", &[1, 2, 3, 4]).await;

    let (h_n, token) = create_valid_token(&te.pk_b64, &te.sk_b64);

    let response = cast_vote::handle(
        &pool,
        "test-election-1",
        &[3, 1, 4, 2],
        &h_n,
        &token,
        Path::new("rules"),
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["action"], "vote-recorded");

    let votes = db::get_votes_for_election(&pool, "test-election-1")
        .await
        .unwrap();
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].candidate_ids, "[3,1,4,2]");
}

#[tokio::test]
async fn cast_vote_nonce_already_used() {
    let pool = setup_db().await;
    let te = TestElection::new();
    seed_election(&pool, &te, "plurality").await;
    seed_candidates(&pool, "test-election-1", &[1, 2, 3]).await;

    let (h_n, token) = create_valid_token(&te.pk_b64, &te.sk_b64);

    // First vote succeeds
    cast_vote::handle(
        &pool,
        "test-election-1",
        &[1],
        &h_n,
        &token,
        Path::new("rules"),
    )
    .await;

    // Second vote with same nonce fails
    let response = cast_vote::handle(
        &pool,
        "test-election-1",
        &[2],
        &h_n,
        &token,
        Path::new("rules"),
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "NONCE_ALREADY_USED");
}

#[tokio::test]
async fn cast_vote_invalid_token() {
    let pool = setup_db().await;
    let te = TestElection::new();
    seed_election(&pool, &te, "plurality").await;
    seed_candidates(&pool, "test-election-1", &[1, 2, 3]).await;

    // Create a fake token (wrong signature)
    let nonce = crypto::generate_nonce();
    let h_n = sha2::Sha256::digest(nonce);
    let h_n_hex = hex::encode(h_n);
    let fake_token = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 300]);

    let response = cast_vote::handle(
        &pool,
        "test-election-1",
        &[1],
        &h_n_hex,
        &fake_token,
        Path::new("rules"),
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "INVALID_TOKEN");
}

#[tokio::test]
async fn cast_vote_election_not_found() {
    let pool = setup_db().await;

    let response = cast_vote::handle(
        &pool,
        "nonexistent",
        &[1],
        "deadbeef",
        "fake-token",
        Path::new("rules"),
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "ELECTION_NOT_FOUND");
}

#[tokio::test]
async fn cast_vote_invalid_ballot() {
    let pool = setup_db().await;
    let te = TestElection::new();
    seed_election(&pool, &te, "plurality").await;
    seed_candidates(&pool, "test-election-1", &[1, 2, 3]).await;

    let (h_n, token) = create_valid_token(&te.pk_b64, &te.sk_b64);

    // Try voting for two candidates in a plurality election (max_choices = 1)
    let response = cast_vote::handle(
        &pool,
        "test-election-1",
        &[1, 2],
        &h_n,
        &token,
        Path::new("rules"),
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "BALLOT_INVALID");
}

/// Votes past end_time must be rejected even if the scheduler has not yet
/// flipped the status from in_progress to finished (it ticks every 30s).
#[tokio::test]
async fn cast_vote_rejected_after_end_time() {
    let pool = setup_db().await;
    let te = TestElection::new();
    let now = chrono::Utc::now().timestamp();
    seed_election_with_times(&pool, &te, "plurality", now - 7200, now - 60).await;
    seed_candidates(&pool, "test-election-1", &[1, 2, 3]).await;

    let (h_n, token) = create_valid_token(&te.pk_b64, &te.sk_b64);

    let response = cast_vote::handle(
        &pool,
        "test-election-1",
        &[1],
        &h_n,
        &token,
        Path::new("rules"),
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "ELECTION_CLOSED");
}

#[tokio::test]
async fn cast_vote_invalid_candidate() {
    let pool = setup_db().await;
    let te = TestElection::new();
    seed_election(&pool, &te, "plurality").await;
    seed_candidates(&pool, "test-election-1", &[1, 2, 3]).await;

    let (h_n, token) = create_valid_token(&te.pk_b64, &te.sk_b64);

    let response = cast_vote::handle(
        &pool,
        "test-election-1",
        &[99],
        &h_n,
        &token,
        Path::new("rules"),
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "INVALID_CANDIDATE");
}
