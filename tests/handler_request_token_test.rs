mod common;

use base64::Engine;
use blind_rsa_signatures::{DefaultRng, PSS, Randomized, Sha384};
use nostr_sdk::prelude::*;
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use ec::db;
use ec::handlers::request_token;
use ec::types::Election;

async fn setup_db() -> SqlitePool {
    common::init_tracing();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

fn generate_rsa_keypair() -> (String, String) {
    ec::crypto::generate_keypair().unwrap()
}

async fn seed_election_with_key(pool: &SqlitePool, status: &str, pk_b64: &str, sk_b64: &str) {
    let election = Election {
        id: "test-election-1".to_string(),
        name: "Test Election".to_string(),
        start_time: 1000,
        end_time: 2000,
        status: status.to_string(),
        rules_id: "plurality".to_string(),
        rsa_pub_key: pk_b64.to_string(),
        created_at: 1000,
        results_published: 0,
    };
    db::create_election(pool, &election, &SecretString::new(sk_b64.into()))
        .await
        .unwrap();
}

async fn seed_authorized_voter(pool: &SqlitePool, election_id: &str, voter_hex: &str) {
    let mut tx = pool.begin().await.unwrap();
    db::authorize_voter(&mut tx, election_id, voter_hex)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn request_token_success() {
    let pool = setup_db().await;
    let (pk_b64, sk_b64) = generate_rsa_keypair();
    seed_election_with_key(&pool, "in_progress", &pk_b64, &sk_b64).await;

    let voter_keys = Keys::generate();
    seed_authorized_voter(&pool, "test-election-1", &voter_keys.public_key().to_hex()).await;

    // Create a blinded message (simulating voter side)
    let pk_der = base64::engine::general_purpose::STANDARD
        .decode(&pk_b64)
        .unwrap();
    let pk = blind_rsa_signatures::PublicKey::<Sha384, PSS, Randomized>::from_der(&pk_der).unwrap();
    let mut rng = DefaultRng;
    let message = b"test-nonce-hash";
    let blinding_result = pk.blind(&mut rng, message).unwrap();
    let blinded_b64 =
        base64::engine::general_purpose::STANDARD.encode(&blinding_result.blind_message);

    let response = request_token::handle(
        &pool,
        &voter_keys.public_key(),
        "test-election-1",
        &blinded_b64,
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["action"], "token-issued");
    assert!(json["blind_signature"].is_string());

    // Verify voter is marked as token_issued
    let voter =
        db::get_authorized_voter(&pool, "test-election-1", &voter_keys.public_key().to_hex())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(voter.token_issued, 1);
}

#[tokio::test]
async fn request_token_not_authorized() {
    let pool = setup_db().await;
    let (pk_b64, sk_b64) = generate_rsa_keypair();
    seed_election_with_key(&pool, "in_progress", &pk_b64, &sk_b64).await;

    let voter_keys = Keys::generate();
    let blinded_b64 = base64::engine::general_purpose::STANDARD.encode(b"some-blinded-data");

    let response = request_token::handle(
        &pool,
        &voter_keys.public_key(),
        "test-election-1",
        &blinded_b64,
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "NOT_AUTHORIZED");
}

#[tokio::test]
async fn request_token_already_issued() {
    let pool = setup_db().await;
    let (pk_b64, sk_b64) = generate_rsa_keypair();
    seed_election_with_key(&pool, "in_progress", &pk_b64, &sk_b64).await;

    let voter_keys = Keys::generate();
    seed_authorized_voter(&pool, "test-election-1", &voter_keys.public_key().to_hex()).await;

    // Mark token as already issued
    let mut tx = pool.begin().await.unwrap();
    db::mark_token_issued(
        &mut tx,
        "test-election-1",
        &voter_keys.public_key().to_hex(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let blinded_b64 = base64::engine::general_purpose::STANDARD.encode(b"some-blinded-data");

    let response = request_token::handle(
        &pool,
        &voter_keys.public_key(),
        "test-election-1",
        &blinded_b64,
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "ALREADY_ISSUED");
}

#[tokio::test]
async fn request_token_election_not_in_progress() {
    let pool = setup_db().await;
    let (pk_b64, sk_b64) = generate_rsa_keypair();
    seed_election_with_key(&pool, "open", &pk_b64, &sk_b64).await;

    let voter_keys = Keys::generate();
    seed_authorized_voter(&pool, "test-election-1", &voter_keys.public_key().to_hex()).await;

    let blinded_b64 = base64::engine::general_purpose::STANDARD.encode(b"some-blinded-data");

    let response = request_token::handle(
        &pool,
        &voter_keys.public_key(),
        "test-election-1",
        &blinded_b64,
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "ELECTION_CLOSED");
}

#[tokio::test]
async fn request_token_election_not_found() {
    let pool = setup_db().await;

    let voter_keys = Keys::generate();
    let response = request_token::handle(&pool, &voter_keys.public_key(), "missing", "AAAA").await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "ELECTION_NOT_FOUND");
}

#[tokio::test]
async fn request_token_rejects_malformed_blinded_nonce() {
    let pool = setup_db().await;
    let (pk_b64, sk_b64) = generate_rsa_keypair();
    seed_election_with_key(&pool, "in_progress", &pk_b64, &sk_b64).await;

    let voter_keys = Keys::generate();
    seed_authorized_voter(&pool, "test-election-1", &voter_keys.public_key().to_hex()).await;

    let response = request_token::handle(
        &pool,
        &voter_keys.public_key(),
        "test-election-1",
        "!!!not-base64!!!",
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "INVALID_TOKEN");
}

#[tokio::test]
async fn request_token_internal_error_when_key_missing() {
    let pool = setup_db().await;

    // Insert an election row WITHOUT a matching election_keys row.
    sqlx::query(
        "INSERT INTO elections (id, name, start_time, end_time, status, rules_id, rsa_pub_key, created_at)
         VALUES ('test-election-1', 'No Key', 1000, 2000, 'in_progress', 'plurality', 'pk', 1000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let voter_keys = Keys::generate();
    seed_authorized_voter(&pool, "test-election-1", &voter_keys.public_key().to_hex()).await;

    let blinded_b64 = base64::engine::general_purpose::STANDARD.encode(b"blinded");
    let response = request_token::handle(
        &pool,
        &voter_keys.public_key(),
        "test-election-1",
        &blinded_b64,
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "INTERNAL_ERROR");
}

#[tokio::test]
async fn request_token_internal_error_on_crypto_failure() {
    let pool = setup_db().await;
    let (pk_b64, _) = generate_rsa_keypair();
    // The stored "private key" is garbage: blind_sign must fail internally.
    seed_election_with_key(&pool, "in_progress", &pk_b64, "bm90LWEta2V5").await;

    let voter_keys = Keys::generate();
    seed_authorized_voter(&pool, "test-election-1", &voter_keys.public_key().to_hex()).await;

    let blinded_b64 = base64::engine::general_purpose::STANDARD.encode(b"blinded");
    let response = request_token::handle(
        &pool,
        &voter_keys.public_key(),
        "test-election-1",
        &blinded_b64,
    )
    .await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "INTERNAL_ERROR");
}

#[tokio::test]
async fn request_token_internal_error_on_db_failure() {
    let pool = setup_db().await;
    pool.close().await;

    let voter_keys = Keys::generate();
    let response = request_token::handle(&pool, &voter_keys.public_key(), "any", "AAAA").await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "INTERNAL_ERROR");
}

#[tokio::test]
async fn request_token_db_error_with_colon_is_not_leaked() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    let voter_keys = Keys::generate();
    let response = request_token::handle(&pool, &voter_keys.public_key(), "any", "AAAA").await;

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "INTERNAL_ERROR");
}
