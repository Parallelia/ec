use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use anyhow::{Context, Result};
use nostr_sdk::prelude::*;
use serde_json::json;

use crate::counting::CountResult;
use crate::types::{Candidate, Election};

/// Nostr event kind for election announcements (addressable, replaceable by `d` tag).
const ELECTION_EVENT_KIND: u16 = 35_000;

/// Nostr event kind for election results / live tally updates.
const RESULT_EVENT_KIND: u16 = 35_001;

/// Last `created_at` published per addressable coordinate (kind + `d` tag).
///
/// Relays keep a single version of an addressable event and, per NIP-01, break
/// ties on equal `created_at` by retaining the *lowest event id* — the others
/// are dropped and never relayed. Both kinds here are republished in bursts
/// (once per `AddCandidate`, once per tally update), which routinely lands
/// several versions inside the same wall-clock second, leaving the relay with
/// an arbitrary — often incomplete — version.
///
/// One `u64` per election; the map is never pruned because its size is bounded
/// by the number of elections the daemon publishes.
static LAST_PUBLISHED_AT: LazyLock<Mutex<HashMap<(u16, String), u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Timestamp for the next publish of `kind`/`identifier`: the current time, or
/// one second past the previous publish when they would collide.
///
/// The `last + 1` path can push `created_at` ahead of wall time, bounded by the
/// number of republishes that land in the same second — a handful, since each
/// one is a serial gRPC call plus a relay round-trip. On restart the in-memory
/// map is lost, so a later republish for the same coordinate reverts to real
/// `now`; if that happens while `now` is still behind a future-dated event on
/// the relay, NIP-01 keeps the older content until wall time catches up. We
/// accept this: the skew is only seconds and self-heals, whereas closing it
/// fully would mean a relay fetch to reseed every cold coordinate.
fn next_created_at(kind: u16, identifier: &str) -> Timestamp {
    let now = Timestamp::now().as_secs();
    // A poisoned lock only means another publish panicked; the map itself is
    // still consistent, so recover instead of failing the publish.
    let mut last_published = LAST_PUBLISHED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let key = (kind, identifier.to_string());
    let created_at = match last_published.get(&key) {
        Some(&last) if last >= now => last + 1,
        _ => now,
    };
    last_published.insert(key, created_at);

    Timestamp::from_secs(created_at)
}

/// Publish (or replace) the election announcement event (Kind 35000).
///
/// Uses the election ID as the `d` tag so relays keep only the latest version.
pub async fn publish_election_event(
    client: &Client,
    election: &Election,
    candidates: &[Candidate],
) -> Result<EventId> {
    let candidate_list: Vec<serde_json::Value> = candidates
        .iter()
        .map(|c| json!({ "id": c.id, "name": c.name }))
        .collect();

    let content = json!({
        "election_id": election.id,
        "name": election.name,
        "start_time": election.start_time,
        "end_time": election.end_time,
        "status": election.status,
        "rules_id": election.rules_id,
        "rsa_pub_key": election.rsa_pub_key,
        "candidates": candidate_list,
    });

    let builder = EventBuilder::new(Kind::Custom(ELECTION_EVENT_KIND), content.to_string())
        .tag(Tag::identifier(&election.id))
        .custom_created_at(next_created_at(ELECTION_EVENT_KIND, &election.id));

    let output = client
        .send_event_builder(builder)
        .await
        .context("Failed to publish election event to relay")?;

    tracing::info!(
        election_id = %election.id,
        event_id = %output.id(),
        "Published Kind {} election event",
        ELECTION_EVENT_KIND
    );

    Ok(*output.id())
}

/// Publish the election result event (Kind 35001).
///
/// Uses the election ID as the `d` tag so relays keep only the latest tally.
pub async fn publish_result_event(
    client: &Client,
    election: &Election,
    result: &CountResult,
) -> Result<EventId> {
    let tally: Vec<serde_json::Value> = result
        .tally
        .iter()
        .map(|t| {
            json!({
                "candidate_id": t.candidate_id,
                "votes": t.votes,
                "status": t.status.as_str(),
            })
        })
        .collect();

    let elected: Vec<u8> = result.elected.clone();

    let mut content = json!({
        "election_id": election.id,
        "name": election.name,
        "rules_id": election.rules_id,
        "elected": elected,
        "tally": tally,
    });

    if let Some(ref count_sheet) = result.count_sheet {
        let rounds: Vec<serde_json::Value> = count_sheet
            .iter()
            .map(|r| {
                let round_tallies: Vec<serde_json::Value> = r
                    .tallies
                    .iter()
                    .map(|t| {
                        json!({
                            "candidate_id": t.candidate_id,
                            "votes": t.votes,
                            "status": t.status.as_str(),
                        })
                    })
                    .collect();
                json!({
                    "round": r.round,
                    "action": r.action,
                    "tallies": round_tallies,
                })
            })
            .collect();
        content["count_sheet"] = json!(rounds);
    }

    let builder = EventBuilder::new(Kind::Custom(RESULT_EVENT_KIND), content.to_string())
        .tag(Tag::identifier(&election.id))
        .custom_created_at(next_created_at(RESULT_EVENT_KIND, &election.id));

    let output = client
        .send_event_builder(builder)
        .await
        .context("Failed to publish result event to relay")?;

    tracing::info!(
        election_id = %election.id,
        event_id = %output.id(),
        "Published Kind {} result event",
        RESULT_EVENT_KIND
    );

    Ok(*output.id())
}
