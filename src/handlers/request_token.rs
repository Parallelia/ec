use base64::Engine;
use nostr_sdk::prelude::PublicKey;
use sqlx::SqlitePool;

use crate::nostr::messages::OutboundMessage;

/// Handle a request for a blind-signed voting token.
///
/// Flow:
/// 1. Validate the election exists and is in_progress (voting phase).
/// 2. Check the sender is an authorized voter who hasn't already received a token.
/// 3. Retrieve the election's RSA private key.
/// 4. Blind-sign the voter's blinded nonce.
/// 5. Mark the voter as having received their token (atomically).
/// 6. Return the blind signature.
pub async fn handle(
    pool: &SqlitePool,
    sender: &PublicKey,
    election_id: &str,
    blinded_nonce: &str,
) -> OutboundMessage {
    match handle_inner(pool, sender, election_id, blinded_nonce).await {
        Ok(blind_sig_b64) => OutboundMessage::ok_with_signature("token-issued", blind_sig_b64),
        Err(e) => {
            // Only known protocol codes are relayed to the voter; anything
            // else (sqlx, crypto errors) stays in the logs.
            let msg = e.to_string();
            if let Some((code, description)) = msg.split_once(": ")
                && let Some(known) = error_code(code)
            {
                OutboundMessage::error(known, description)
            } else {
                tracing::error!(error = %e, "Unexpected error in request-token handler");
                OutboundMessage::error("INTERNAL_ERROR", "An unexpected error occurred")
            }
        }
    }
}

async fn handle_inner(
    pool: &SqlitePool,
    sender: &PublicKey,
    election_id: &str,
    blinded_nonce_b64: &str,
) -> anyhow::Result<String> {
    // 1. Validate election exists and is in voting phase
    let election = crate::db::get_election(pool, election_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ELECTION_NOT_FOUND: Election does not exist"))?;

    if election.status != "in_progress" {
        anyhow::bail!("ELECTION_CLOSED: Election is not in voting phase");
    }

    // 2. Check voter is authorized and hasn't already received a token
    let voter_hex = sender.to_hex();
    let voter = crate::db::get_authorized_voter(pool, election_id, &voter_hex)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("NOT_AUTHORIZED: Voter is not registered for this election")
        })?;

    if voter.token_issued != 0 {
        anyhow::bail!("ALREADY_ISSUED: Voting token has already been issued");
    }

    // 3. Get election's RSA private key
    let sk_der_b64 = crate::db::get_election_key(pool, election_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("INTERNAL_ERROR: Election key not found"))?;

    // 4. Decode the blinded nonce and blind-sign it
    let blinded_message = base64::engine::general_purpose::STANDARD
        .decode(blinded_nonce_b64)
        .map_err(|_| {
            anyhow::anyhow!("INVALID_TOKEN: Malformed blinded nonce (not valid base64)")
        })?;

    let blind_sig = crate::crypto::blind_sign(&sk_der_b64, &blinded_message)?;

    // 5. Mark token as issued (atomically — prevents double issuance)
    let mut tx = pool.begin().await?;
    let rows = crate::db::mark_token_issued(&mut tx, election_id, &voter_hex).await?;

    if rows == 0 {
        // Race condition: another request already marked it
        anyhow::bail!("ALREADY_ISSUED: Voting token has already been issued");
    }

    tx.commit().await?;

    tracing::info!(
        election_id = %election_id,
        "Blind-signed voting token issued"
    );

    // 6. Return base64-encoded blind signature
    Ok(base64::engine::general_purpose::STANDARD.encode(&blind_sig))
}

fn error_code(code: &str) -> Option<&'static str> {
    match code {
        "ELECTION_NOT_FOUND" => Some("ELECTION_NOT_FOUND"),
        "ELECTION_CLOSED" => Some("ELECTION_CLOSED"),
        "NOT_AUTHORIZED" => Some("NOT_AUTHORIZED"),
        "ALREADY_ISSUED" => Some("ALREADY_ISSUED"),
        "INVALID_TOKEN" => Some("INVALID_TOKEN"),
        _ => None,
    }
}
