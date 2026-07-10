//! Coverage of Nostr event publishing (Kind 35000 announcements and
//! Kind 35001 results) against an in-memory fake relay.

mod common;

use nostr_sdk::prelude::{Client, Keys};

use ec::counting::{CandidateStatus, CandidateTally, CountResult, CountRound};
use ec::nostr::publisher;
use ec::types::{Candidate, Election};

fn make_election(id: &str) -> Election {
    Election {
        id: id.to_string(),
        name: "Test Election".to_string(),
        start_time: 1000,
        end_time: 2000,
        status: "open".to_string(),
        rules_id: "plurality".to_string(),
        rsa_pub_key: "pk".to_string(),
        created_at: 1000,
        results_published: 0,
    }
}

fn make_result(with_count_sheet: bool) -> CountResult {
    let tally = vec![
        CandidateTally {
            candidate_id: 1,
            votes: 3.0,
            status: CandidateStatus::Elected,
        },
        CandidateTally {
            candidate_id: 2,
            votes: 1.0,
            status: CandidateStatus::Excluded,
        },
    ];
    CountResult {
        elected: vec![1],
        count_sheet: with_count_sheet.then(|| {
            vec![CountRound {
                round: 1,
                tallies: tally.clone(),
                action: "Elected: 1".to_string(),
            }]
        }),
        tally,
    }
}

async fn online_client(relay_url: &str) -> Client {
    common::init_tracing();
    let client = Client::builder().signer(Keys::generate()).build();
    client.add_relay(relay_url).await.unwrap();
    client.connect().await;
    client
}

#[tokio::test]
async fn publishes_election_announcement_with_candidates() {
    let relay_url = common::start_fake_relay().await;
    let client = online_client(&relay_url).await;

    let candidates = vec![
        Candidate {
            id: 1,
            election_id: "e1".to_string(),
            name: "Alice".to_string(),
        },
        Candidate {
            id: 2,
            election_id: "e1".to_string(),
            name: "Bob".to_string(),
        },
    ];

    let event_id = publisher::publish_election_event(&client, &make_election("e1"), &candidates)
        .await
        .expect("publish must succeed");
    assert!(!event_id.to_hex().is_empty());
}

#[tokio::test]
async fn publishes_result_event_with_and_without_count_sheet() {
    let relay_url = common::start_fake_relay().await;
    let client = online_client(&relay_url).await;
    let election = make_election("e1");

    // Plurality-style result: no count sheet.
    publisher::publish_result_event(&client, &election, &make_result(false))
        .await
        .expect("publish without count sheet must succeed");

    // STV-style result: per-round count sheet included.
    publisher::publish_result_event(&client, &election, &make_result(true))
        .await
        .expect("publish with count sheet must succeed");
}

#[tokio::test]
async fn publishing_fails_without_relay() {
    let client = Client::builder().signer(Keys::generate()).build();
    let election = make_election("e1");

    let err = publisher::publish_election_event(&client, &election, &[])
        .await
        .expect_err("no relay → publish must fail");
    assert!(err.to_string().contains("Failed to publish election event"));

    let err = publisher::publish_result_event(&client, &election, &make_result(false))
        .await
        .expect_err("no relay → publish must fail");
    assert!(err.to_string().contains("Failed to publish result event"));
}
