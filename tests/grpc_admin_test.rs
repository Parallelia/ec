//! Coverage of the gRPC admin service, calling the trait methods directly
//! (no TCP server needed). Nostr publishing runs against an in-memory fake
//! relay where a success is asserted, or against a relay-less client where
//! only the warning path is exercised.

mod common;

use nostr_sdk::prelude::{Client, Keys};
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tonic::Request;

use ec::db;
use ec::grpc::admin::AdminService;
use ec::grpc::proto::admin_server::Admin;
use ec::grpc::proto::{
    AddCandidateRequest, AddElectionRequest, ElectionIdRequest, Empty, GenerateTokensRequest,
};
use ec::types::Election;

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

/// Nostr client with no relays: publishing fails, which the admin service
/// must tolerate (announcements are best-effort).
fn offline_client() -> Client {
    Client::builder().signer(Keys::generate()).build()
}

async fn online_client(relay_url: &str) -> Client {
    let client = Client::builder().signer(Keys::generate()).build();
    client.add_relay(relay_url).await.unwrap();
    client.connect().await;
    client
}

fn service(pool: &SqlitePool, client: Client) -> AdminService {
    AdminService::new(pool.clone(), std::path::PathBuf::from("rules"), client)
}

async fn seed_election(pool: &SqlitePool, id: &str, status: &str) {
    let election = Election {
        id: id.to_string(),
        name: format!("Election {id}"),
        start_time: 1000,
        end_time: 2000,
        status: status.to_string(),
        rules_id: "plurality".to_string(),
        rsa_pub_key: "pk".to_string(),
        created_at: 1000,
        results_published: 0,
    };
    db::create_election(pool, &election, &SecretString::new("sk".into()))
        .await
        .unwrap();
}

// --- AddElection ---

#[tokio::test]
async fn add_election_success_with_relay() {
    let relay_url = common::start_fake_relay().await;
    let pool = setup_pool().await;
    let svc = service(&pool, online_client(&relay_url).await);

    let now = chrono::Utc::now().timestamp();
    let response = svc
        .add_election(Request::new(AddElectionRequest {
            name: "Board 2026".to_string(),
            start_time: now + 3600,
            end_time: now + 7200,
            rules_id: "plurality".to_string(),
        }))
        .await
        .expect("add_election must succeed")
        .into_inner();

    assert_eq!(response.name, "Board 2026");
    assert_eq!(response.status, "open");
    assert_eq!(response.rules_id, "plurality");
    assert!(!response.rsa_pub_key.is_empty());

    // The election and its private key must be persisted.
    let stored = db::get_election(&pool, &response.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "open");
    assert!(
        db::get_election_key(&pool, &response.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn add_election_success_without_relay_still_persists() {
    let pool = setup_pool().await;
    let svc = service(&pool, offline_client());

    let now = chrono::Utc::now().timestamp();
    let response = svc
        .add_election(Request::new(AddElectionRequest {
            name: "Offline".to_string(),
            start_time: now + 3600,
            end_time: now + 7200,
            rules_id: "stv".to_string(),
        }))
        .await
        .expect("publish failure must not fail the call")
        .into_inner();

    assert!(
        db::get_election(&pool, &response.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn add_election_rejects_past_start_time() {
    let pool = setup_pool().await;
    let svc = service(&pool, offline_client());

    let now = chrono::Utc::now().timestamp();
    let status = svc
        .add_election(Request::new(AddElectionRequest {
            name: "Past".to_string(),
            start_time: now - 10,
            end_time: now + 7200,
            rules_id: "plurality".to_string(),
        }))
        .await
        .expect_err("past start_time must fail");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn add_election_rejects_end_before_start() {
    let pool = setup_pool().await;
    let svc = service(&pool, offline_client());

    let now = chrono::Utc::now().timestamp();
    let status = svc
        .add_election(Request::new(AddElectionRequest {
            name: "Backwards".to_string(),
            start_time: now + 7200,
            end_time: now + 3600,
            rules_id: "plurality".to_string(),
        }))
        .await
        .expect_err("end before start must fail");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn add_election_rejects_path_traversal_rules_id() {
    let pool = setup_pool().await;
    let svc = service(&pool, offline_client());
    let now = chrono::Utc::now().timestamp();

    for bad in ["", "../etc/passwd", "a/b", "a\\b", "x.."] {
        let status = svc
            .add_election(Request::new(AddElectionRequest {
                name: "Evil".to_string(),
                start_time: now + 3600,
                end_time: now + 7200,
                rules_id: bad.to_string(),
            }))
            .await
            .expect_err("malicious rules_id must fail");
        assert_eq!(
            status.code(),
            tonic::Code::InvalidArgument,
            "rules_id {bad:?}"
        );
    }
}

#[tokio::test]
async fn add_election_rejects_unknown_rules_id() {
    let pool = setup_pool().await;
    let svc = service(&pool, offline_client());
    let now = chrono::Utc::now().timestamp();

    let status = svc
        .add_election(Request::new(AddElectionRequest {
            name: "Unknown rules".to_string(),
            start_time: now + 3600,
            end_time: now + 7200,
            rules_id: "borda".to_string(),
        }))
        .await
        .expect_err("unknown rules_id must fail");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

// --- AddCandidate ---

#[tokio::test]
async fn add_candidate_success_with_relay() {
    let relay_url = common::start_fake_relay().await;
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "open").await;
    let svc = service(&pool, online_client(&relay_url).await);

    let response = svc
        .add_candidate(Request::new(AddCandidateRequest {
            election_id: "e1".to_string(),
            id: 7,
            name: "Alice".to_string(),
        }))
        .await
        .expect("add_candidate must succeed")
        .into_inner();

    assert_eq!(response.id, 7);
    assert_eq!(response.name, "Alice");

    let candidates = db::get_candidates_for_election(&pool, "e1").await.unwrap();
    assert_eq!(candidates.len(), 1);
}

#[tokio::test]
async fn add_candidate_rejects_id_above_u8() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "open").await;
    let svc = service(&pool, offline_client());

    let status = svc
        .add_candidate(Request::new(AddCandidateRequest {
            election_id: "e1".to_string(),
            id: 256,
            name: "Too big".to_string(),
        }))
        .await
        .expect_err("id above 255 must fail");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn add_candidate_rejects_non_open_election() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "in_progress").await;
    let svc = service(&pool, offline_client());

    let status = svc
        .add_candidate(Request::new(AddCandidateRequest {
            election_id: "e1".to_string(),
            id: 1,
            name: "Late".to_string(),
        }))
        .await
        .expect_err("non-open election must fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);

    // Same for a completely unknown election.
    let status = svc
        .add_candidate(Request::new(AddCandidateRequest {
            election_id: "missing".to_string(),
            id: 1,
            name: "Ghost".to_string(),
        }))
        .await
        .expect_err("unknown election must fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

// --- CancelElection ---

#[tokio::test]
async fn cancel_election_success_with_relay() {
    let relay_url = common::start_fake_relay().await;
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "open").await;
    let svc = service(&pool, online_client(&relay_url).await);

    let response = svc
        .cancel_election(Request::new(ElectionIdRequest {
            election_id: "e1".to_string(),
        }))
        .await
        .expect("cancel must succeed")
        .into_inner();
    assert!(response.success);

    let e = db::get_election(&pool, "e1").await.unwrap().unwrap();
    assert_eq!(e.status, "cancelled");
}

#[tokio::test]
async fn cancel_election_without_relay_still_cancels() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "in_progress").await;
    let svc = service(&pool, offline_client());

    let response = svc
        .cancel_election(Request::new(ElectionIdRequest {
            election_id: "e1".to_string(),
        }))
        .await
        .expect("cancel must succeed")
        .into_inner();
    assert!(response.success);
}

#[tokio::test]
async fn cancel_election_rejects_finished_or_missing() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "finished").await;
    let svc = service(&pool, offline_client());

    for id in ["e1", "missing"] {
        let status = svc
            .cancel_election(Request::new(ElectionIdRequest {
                election_id: id.to_string(),
            }))
            .await
            .expect_err("must fail");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition, "id {id}");
    }
}

// --- GetElection / ListElections ---

#[tokio::test]
async fn get_election_found_and_not_found() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "open").await;
    let svc = service(&pool, offline_client());

    let response = svc
        .get_election(Request::new(ElectionIdRequest {
            election_id: "e1".to_string(),
        }))
        .await
        .expect("must succeed")
        .into_inner();
    assert_eq!(response.id, "e1");

    let status = svc
        .get_election(Request::new(ElectionIdRequest {
            election_id: "missing".to_string(),
        }))
        .await
        .expect_err("unknown election must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn list_elections_returns_all() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "open").await;
    seed_election(&pool, "e2", "finished").await;
    let svc = service(&pool, offline_client());

    let response = svc
        .list_elections(Request::new(Empty {}))
        .await
        .expect("must succeed")
        .into_inner();
    assert_eq!(response.elections.len(), 2);
}

// --- Registration tokens ---

#[tokio::test]
async fn generate_and_list_registration_tokens() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "open").await;
    let svc = service(&pool, offline_client());

    let response = svc
        .generate_registration_tokens(Request::new(GenerateTokensRequest {
            election_id: "e1".to_string(),
            count: 3,
        }))
        .await
        .expect("must succeed")
        .into_inner();
    assert_eq!(response.tokens.len(), 3);

    // Consume one token so the listing shows a used entry.
    let mut tx = pool.begin().await.unwrap();
    db::consume_registration_token(&mut tx, &response.tokens[0], "e1", "voter-1")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let listing = svc
        .list_registration_tokens(Request::new(ElectionIdRequest {
            election_id: "e1".to_string(),
        }))
        .await
        .expect("must succeed")
        .into_inner();

    assert_eq!(listing.tokens.len(), 3);
    assert_eq!(listing.tokens.iter().filter(|t| t.used).count(), 1);
    // Raw tokens are never exposed — only truncated hashes (16 hex chars).
    for info in &listing.tokens {
        assert_eq!(info.token_id.len(), 16);
        assert!(!response.tokens.contains(&info.token_id));
    }
}

#[tokio::test]
async fn generate_tokens_validates_count_bounds() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "open").await;
    let svc = service(&pool, offline_client());

    for count in [0u32, 10_001] {
        let status = svc
            .generate_registration_tokens(Request::new(GenerateTokensRequest {
                election_id: "e1".to_string(),
                count,
            }))
            .await
            .expect_err("invalid count must fail");
        assert_eq!(status.code(), tonic::Code::InvalidArgument, "count {count}");
    }
}

#[tokio::test]
async fn generate_tokens_requires_enrollable_election() {
    let pool = setup_pool().await;
    seed_election(&pool, "done", "finished").await;
    let svc = service(&pool, offline_client());

    let status = svc
        .generate_registration_tokens(Request::new(GenerateTokensRequest {
            election_id: "missing".to_string(),
            count: 1,
        }))
        .await
        .expect_err("unknown election must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    let status = svc
        .generate_registration_tokens(Request::new(GenerateTokensRequest {
            election_id: "done".to_string(),
            count: 1,
        }))
        .await
        .expect_err("finished election must fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn list_registration_tokens_requires_existing_election() {
    let pool = setup_pool().await;
    let svc = service(&pool, offline_client());

    let status = svc
        .list_registration_tokens(Request::new(ElectionIdRequest {
            election_id: "missing".to_string(),
        }))
        .await
        .expect_err("unknown election must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);
}

// --- Database failure paths (Status::internal mappers) ---

#[tokio::test]
async fn all_methods_return_internal_on_closed_pool() {
    let pool = setup_pool().await;
    pool.close().await;
    let svc = service(&pool, offline_client());
    let now = chrono::Utc::now().timestamp();

    let status = svc
        .add_election(Request::new(AddElectionRequest {
            name: "DB down".to_string(),
            start_time: now + 3600,
            end_time: now + 7200,
            rules_id: "plurality".to_string(),
        }))
        .await
        .expect_err("closed pool must fail");
    assert_eq!(status.code(), tonic::Code::Internal);

    let status = svc
        .add_candidate(Request::new(AddCandidateRequest {
            election_id: "e1".to_string(),
            id: 1,
            name: "X".to_string(),
        }))
        .await
        .expect_err("closed pool must fail");
    assert_eq!(status.code(), tonic::Code::Internal);

    let status = svc
        .cancel_election(Request::new(ElectionIdRequest {
            election_id: "e1".to_string(),
        }))
        .await
        .expect_err("closed pool must fail");
    assert_eq!(status.code(), tonic::Code::Internal);

    let status = svc
        .get_election(Request::new(ElectionIdRequest {
            election_id: "e1".to_string(),
        }))
        .await
        .expect_err("closed pool must fail");
    assert_eq!(status.code(), tonic::Code::Internal);

    let status = svc
        .list_elections(Request::new(Empty {}))
        .await
        .expect_err("closed pool must fail");
    assert_eq!(status.code(), tonic::Code::Internal);

    let status = svc
        .generate_registration_tokens(Request::new(GenerateTokensRequest {
            election_id: "e1".to_string(),
            count: 1,
        }))
        .await
        .expect_err("closed pool must fail");
    assert_eq!(status.code(), tonic::Code::Internal);

    let status = svc
        .list_registration_tokens(Request::new(ElectionIdRequest {
            election_id: "e1".to_string(),
        }))
        .await
        .expect_err("closed pool must fail");
    assert_eq!(status.code(), tonic::Code::Internal);
}

#[tokio::test]
async fn token_operations_fail_internally_when_table_is_gone() {
    let pool = setup_pool().await;
    seed_election(&pool, "e1", "open").await;
    // Elections remain readable, but the token table vanishes mid-flight.
    sqlx::query("DROP TABLE registration_tokens")
        .execute(&pool)
        .await
        .unwrap();
    let svc = service(&pool, offline_client());

    let status = svc
        .generate_registration_tokens(Request::new(GenerateTokensRequest {
            election_id: "e1".to_string(),
            count: 1,
        }))
        .await
        .expect_err("missing table must fail");
    assert_eq!(status.code(), tonic::Code::Internal);

    let status = svc
        .list_registration_tokens(Request::new(ElectionIdRequest {
            election_id: "e1".to_string(),
        }))
        .await
        .expect_err("missing table must fail");
    assert_eq!(status.code(), tonic::Code::Internal);
}
