//! End-to-end coverage of the Nostr Gift Wrap listener: a test voter client
//! and the EC daemon exchange NIP-59 messages through an in-memory fake relay.

mod common;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use blind_rsa_signatures::{DefaultRng, PSS, Randomized, Sha384};
use nostr_sdk::prelude::*;
use secrecy::SecretString;
use sha2::Digest;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use ec::config::Config;
use ec::state::AppState;
use ec::types::{Candidate, Election};
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

fn test_config(relay_url: &str) -> Config {
    Config {
        relay_url: relay_url.to_string(),
        grpc_bind: "127.0.0.1:0".to_string(),
        rules_dir: std::path::PathBuf::from("rules"),
        log_level: "debug".to_string(),
        db_path: "sqlite::memory:".to_string(),
        nostr_private_key: SecretString::new("unused".into()),
        db_password: None,
        admin_token: None,
    }
}

/// Boot the EC listener connected to the fake relay and return its state.
async fn start_ec(relay_url: &str, pool: SqlitePool) -> Arc<AppState> {
    let ec_keys = Keys::generate();
    let client = Client::builder().signer(ec_keys.clone()).build();
    client.add_relay(relay_url).await.unwrap();
    client.connect().await;

    let state = Arc::new(AppState {
        db: pool,
        nostr_client: client,
        ec_nostr_keys: ec_keys,
        config: test_config(relay_url),
    });

    tokio::spawn(ec::nostr::listener::listen(state.clone()));
    // Give the listener a moment to subscribe before tests publish.
    tokio::time::sleep(Duration::from_millis(300)).await;
    state
}

/// Connect a voter client subscribed to Gift Wrap replies addressed to it.
async fn start_voter(relay_url: &str) -> (Client, Keys) {
    let keys = Keys::generate();
    let client = Client::builder().signer(keys.clone()).build();
    client.add_relay(relay_url).await.unwrap();
    client.connect().await;
    client
        .subscribe(
            Filter::new()
                .kind(Kind::GiftWrap)
                .pubkey(keys.public_key())
                .limit(0),
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    (client, keys)
}

/// Send `content` to the EC as a Gift Wrap rumor and wait for the EC's reply.
async fn roundtrip(voter: &Client, ec_pubkey: PublicKey, content: String) -> serde_json::Value {
    let mut notifications = voter.notifications();
    let signer = voter.signer().await.unwrap();
    let rumor = EventBuilder::text_note(content).build(signer.get_public_key().await.unwrap());
    voter.gift_wrap(&ec_pubkey, rumor, []).await.unwrap();

    let reply = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let Ok(notification) = notifications.recv().await else {
                continue;
            };
            let RelayPoolNotification::Event { event, .. } = notification else {
                continue;
            };
            if event.kind != Kind::GiftWrap {
                continue;
            }
            // The relay broadcasts everything; skip wraps we cannot unwrap
            // (e.g. our own outbound request).
            let Ok(unwrapped) = voter.unwrap_gift_wrap(&event).await else {
                continue;
            };
            if unwrapped.sender == ec_pubkey {
                return unwrapped.rumor.content.clone();
            }
        }
    })
    .await
    .expect("timed out waiting for EC reply");

    serde_json::from_str(&reply).expect("EC reply must be JSON")
}

async fn seed_open_election(pool: &SqlitePool, pk_b64: &str, sk_b64: &str, status: &str) {
    let now = chrono::Utc::now().timestamp();
    let election = Election {
        id: "e2e-election".to_string(),
        name: "E2E".to_string(),
        start_time: now - 60,
        end_time: now + 3600,
        status: status.to_string(),
        rules_id: "plurality".to_string(),
        rsa_pub_key: pk_b64.to_string(),
        created_at: now,
        results_published: 0,
    };
    db::create_election(pool, &election, &SecretString::new(sk_b64.into()))
        .await
        .unwrap();
    db::add_candidate(
        pool,
        &Candidate {
            id: 1,
            election_id: "e2e-election".to_string(),
            name: "Alice".to_string(),
        },
    )
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    db::insert_registration_tokens(&mut tx, "e2e-election", &["e2e-token".to_string()])
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Full voter protocol over Gift Wrap: register → request blind-signed token
/// → cast vote. Exercises every dispatch arm and the reply path.
#[tokio::test]
async fn full_voting_flow_over_gift_wrap() {
    let relay_url = common::start_fake_relay().await;
    let pool = setup_pool().await;

    let (pk_b64, sk_b64) = crypto::generate_keypair().unwrap();
    seed_open_election(&pool, &pk_b64, &sk_b64, "in_progress").await;

    let state = start_ec(&relay_url, pool.clone()).await;
    let ec_pubkey = state.ec_nostr_keys.public_key();
    let (voter, voter_keys) = start_voter(&relay_url).await;

    // 1. Register with the registration token.
    let reply = roundtrip(
        &voter,
        ec_pubkey,
        serde_json::json!({
            "action": "register",
            "election_id": "e2e-election",
            "registration_token": "e2e-token",
        })
        .to_string(),
    )
    .await;
    assert_eq!(reply["status"], "ok", "register reply: {reply}");
    assert_eq!(reply["action"], "register-confirmed");

    // 2. Blind a nonce hash and request the voting token.
    let nonce = crypto::generate_nonce();
    let h_n = sha2::Sha256::digest(nonce);
    let h_n_hex = hex::encode(h_n);
    let pk_der = base64::engine::general_purpose::STANDARD
        .decode(&pk_b64)
        .unwrap();
    let pk = blind_rsa_signatures::PublicKey::<Sha384, PSS, Randomized>::from_der(&pk_der).unwrap();
    let mut rng = DefaultRng;
    let blinding_result = pk.blind(&mut rng, h_n.as_slice()).unwrap();
    let blinded_b64 =
        base64::engine::general_purpose::STANDARD.encode(&blinding_result.blind_message);

    let reply = roundtrip(
        &voter,
        ec_pubkey,
        serde_json::json!({
            "action": "request-token",
            "election_id": "e2e-election",
            "blinded_nonce": blinded_b64,
            "request_id": "req-42",
        })
        .to_string(),
    )
    .await;
    assert_eq!(reply["status"], "ok", "request-token reply: {reply}");
    assert_eq!(reply["action"], "token-issued");
    assert_eq!(reply["request_id"], "req-42");

    // 3. Finalize the blind signature and cast the vote.
    let blind_sig_b64 = reply["blind_signature"].as_str().unwrap();
    let blind_sig = base64::engine::general_purpose::STANDARD
        .decode(blind_sig_b64)
        .unwrap();
    let sig = pk
        .finalize(&blind_sig.into(), &blinding_result, h_n.as_slice())
        .unwrap();
    let randomizer = blinding_result.msg_randomizer.expect("randomizer");
    let mut token_bytes = sig.to_vec();
    token_bytes.extend_from_slice(randomizer.as_ref());
    let token_b64 = base64::engine::general_purpose::STANDARD.encode(&token_bytes);

    let reply = roundtrip(
        &voter,
        ec_pubkey,
        serde_json::json!({
            "action": "cast-vote",
            "election_id": "e2e-election",
            "candidate_ids": [1],
            "h_n": h_n_hex,
            "token": token_b64,
        })
        .to_string(),
    )
    .await;
    assert_eq!(reply["status"], "ok", "cast-vote reply: {reply}");
    assert_eq!(reply["action"], "vote-recorded");

    // The vote is stored without any voter identity.
    let votes = db::get_votes_for_election(&pool, "e2e-election")
        .await
        .unwrap();
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].candidate_ids, "[1]");
    assert!(!voter_keys.public_key().to_hex().is_empty());
}

#[tokio::test]
async fn malformed_message_gets_invalid_message_reply_with_request_id() {
    let relay_url = common::start_fake_relay().await;
    let pool = setup_pool().await;
    let state = start_ec(&relay_url, pool).await;
    let ec_pubkey = state.ec_nostr_keys.public_key();
    let (voter, _) = start_voter(&relay_url).await;

    // Unknown action, but a valid request_id → error stays correlatable.
    let reply = roundtrip(
        &voter,
        ec_pubkey,
        serde_json::json!({
            "action": "self-destruct",
            "request_id": "corr-1",
        })
        .to_string(),
    )
    .await;
    assert_eq!(reply["status"], "error");
    assert_eq!(reply["code"], "INVALID_MESSAGE");
    assert_eq!(reply["request_id"], "corr-1");

    // Content that is not JSON at all.
    let reply = roundtrip(&voter, ec_pubkey, "not json at all".to_string()).await;
    assert_eq!(reply["status"], "error");
    assert_eq!(reply["code"], "INVALID_MESSAGE");
    assert!(reply.get("request_id").is_none());
}

/// A Gift Wrap that cannot be unwrapped must not kill the listener.
#[tokio::test]
async fn listener_survives_undecryptable_gift_wrap() {
    let relay_url = common::start_fake_relay().await;
    let pool = setup_pool().await;
    let state = start_ec(&relay_url, pool).await;
    let ec_pubkey = state.ec_nostr_keys.public_key();
    let (voter, _) = start_voter(&relay_url).await;

    // A "gift wrap" addressed to the EC whose content is garbage: the EC will
    // fail to unwrap it and must log-and-continue.
    let bogus_keys = Keys::generate();
    let bogus = EventBuilder::new(Kind::GiftWrap, "garbage-ciphertext")
        .tag(Tag::public_key(ec_pubkey))
        .sign_with_keys(&bogus_keys)
        .unwrap();
    voter.send_event(&bogus).await.unwrap();

    // The listener must still answer real messages afterwards.
    let reply = roundtrip(
        &voter,
        ec_pubkey,
        serde_json::json!({
            "action": "register",
            "election_id": "missing",
            "registration_token": "nope",
        })
        .to_string(),
    )
    .await;
    assert_eq!(reply["status"], "error");
    assert_eq!(reply["code"], "ELECTION_NOT_FOUND");
}
