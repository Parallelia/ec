//! Coverage of Nostr event publishing (Kind 35000 announcements and
//! Kind 35001 results) against an in-memory fake relay.

mod common;

use std::time::Duration;

use nostr_sdk::prelude::{Client, Filter, Keys, Kind, RelayPoolNotification};

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

/// Connect a client subscribed to `kind`, ready to observe what gets published.
async fn observer(relay_url: &str, kind: Kind) -> Client {
    let client = online_client(relay_url).await;
    client
        .subscribe(Filter::new().kind(kind), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    client
}

/// Collect the `created_at` of the next `count` events of `kind`, in arrival
/// order. The receiver must be taken before publishing, or events are missed.
async fn next_created_at(
    notifications: &mut tokio::sync::broadcast::Receiver<RelayPoolNotification>,
    kind: Kind,
    count: usize,
) -> Vec<u64> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut stamps = Vec::with_capacity(count);
        while stamps.len() < count {
            let Ok(RelayPoolNotification::Event { event, .. }) = notifications.recv().await else {
                continue;
            };
            if event.kind == kind {
                stamps.push(event.created_at.as_secs());
            }
        }
        stamps
    })
    .await
    .expect("relay must deliver every published event")
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

/// Announcements are addressable events: relays keep a single version per
/// `d` tag and, on equal `created_at`, retain the lowest event id (NIP-01).
/// `AddCandidate` republishes the whole announcement once per candidate, so a
/// burst within one second would leave the relay holding an arbitrary — often
/// incomplete — candidate list. Timestamps must be strictly increasing.
#[tokio::test]
async fn election_republished_in_the_same_second_gets_increasing_created_at() {
    let relay_url = common::start_fake_relay().await;
    let observer = observer(&relay_url, Kind::Custom(35_000)).await;
    let client = online_client(&relay_url).await;
    let election = make_election("e-burst");
    let candidates: Vec<Candidate> = (1..=3)
        .map(|id| Candidate {
            id,
            election_id: "e-burst".to_string(),
            name: format!("Candidate {id}"),
        })
        .collect();
    let mut notifications = observer.notifications();

    // Mirrors AddCandidate: create, then republish the growing candidate list.
    for upto in 0..3 {
        publisher::publish_election_event(&client, &election, &candidates[..upto])
            .await
            .expect("publish must succeed");
    }

    let stamps = next_created_at(&mut notifications, Kind::Custom(35_000), 3).await;
    let now = nostr_sdk::prelude::Timestamp::now().as_secs();
    assert!(
        stamps[0] < stamps[1] && stamps[1] < stamps[2],
        "created_at must be strictly increasing, got {stamps:?}"
    );
    assert!(
        stamps[0].abs_diff(now) < 60,
        "first publish must use the current time, got {} vs now {now}",
        stamps[0]
    );
}

/// Kind 35001 is addressable too and is republished on every live tally
/// update, so it needs the same strictly increasing timestamps.
#[tokio::test]
async fn result_republished_in_the_same_second_gets_increasing_created_at() {
    let relay_url = common::start_fake_relay().await;
    let observer = observer(&relay_url, Kind::Custom(35_001)).await;
    let client = online_client(&relay_url).await;
    let election = make_election("r-burst");
    let mut notifications = observer.notifications();

    for with_count_sheet in [false, true] {
        publisher::publish_result_event(&client, &election, &make_result(with_count_sheet))
            .await
            .expect("publish must succeed");
    }

    let stamps = next_created_at(&mut notifications, Kind::Custom(35_001), 2).await;
    assert!(
        stamps[0] < stamps[1],
        "created_at must be strictly increasing, got {stamps:?}"
    );
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
