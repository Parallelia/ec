use anyhow::Result;
use nostr_sdk::prelude::*;

use crate::handlers;
use crate::nostr::messages::{self, InboundMessage, OutboundMessage};
use crate::state::SharedState;

/// Start listening for NIP-59 Gift Wrap messages addressed to the EC.
///
/// Subscribes to Kind::GiftWrap events targeting the EC's public key,
/// unwraps each message, parses the JSON content, and dispatches to
/// the appropriate handler. Replies are sent back via Gift Wrap.
pub async fn listen(state: SharedState) -> Result<()> {
    let ec_pubkey = state.ec_nostr_keys.public_key();

    // Subscribe to Gift Wrap events addressed to the EC.
    // limit(0) because NIP-59 tweaks timestamps, so historical fetch is unreliable.
    let filter = Filter::new()
        .kind(Kind::GiftWrap)
        .pubkey(ec_pubkey)
        .limit(0);

    state.nostr_client.subscribe(filter, None).await?;

    tracing::info!("Nostr listener subscribed for Gift Wrap messages");

    state
        .nostr_client
        .handle_notifications(|notification| {
            let state = state.clone();
            async move {
                if let RelayPoolNotification::Event { event, .. } = notification
                    && event.kind == Kind::GiftWrap
                    && let Err(e) = handle_gift_wrap(&state, &event).await
                {
                    tracing::warn!(error = %e, "Failed to process Gift Wrap message");
                }
                Ok(false)
            }
        })
        .await?;

    Ok(())
}

/// Unwrap a Gift Wrap event, parse the inner message, dispatch to handler,
/// and send the reply back via Gift Wrap.
async fn handle_gift_wrap(state: &SharedState, event: &Event) -> Result<()> {
    let unwrapped = state.nostr_client.unwrap_gift_wrap(event).await?;

    let sender = unwrapped.sender;
    let content = &unwrapped.rumor.content;

    // Extracted from the raw JSON (not the parsed message) so even an
    // INVALID_MESSAGE reply stays correlatable to the request that caused it.
    let request_id = messages::extract_request_id(content);

    let response = match serde_json::from_str::<InboundMessage>(content) {
        Ok(msg) => dispatch(state, &sender, msg).await,
        Err(e) => {
            tracing::warn!(error = %e, "Invalid inbound message format");
            OutboundMessage::error("INVALID_MESSAGE", format!("Malformed request: {e}"))
        }
    }
    .with_request_id(request_id);

    send_reply(state, &sender, &response).await?;

    Ok(())
}

/// Route an inbound message to the appropriate handler.
async fn dispatch(state: &SharedState, sender: &PublicKey, msg: InboundMessage) -> OutboundMessage {
    match msg {
        InboundMessage::Register {
            election_id,
            registration_token,
        } => handlers::register::handle(&state.db, sender, &election_id, &registration_token).await,
        InboundMessage::RequestToken {
            election_id,
            blinded_nonce,
        } => handlers::request_token::handle(&state.db, sender, &election_id, &blinded_nonce).await,
        InboundMessage::CastVote {
            election_id,
            candidate_ids,
            h_n,
            token,
        } => {
            handlers::cast_vote::handle(
                &state.db,
                &election_id,
                &candidate_ids,
                &h_n,
                &token,
                &state.config.rules_dir,
            )
            .await
        }
    }
}

/// Send a reply to a voter via NIP-59 Gift Wrap.
async fn send_reply(
    state: &SharedState,
    receiver: &PublicKey,
    response: &OutboundMessage,
) -> Result<()> {
    let content = serde_json::to_string(response)?;

    let rumor = EventBuilder::text_note(content).build(state.ec_nostr_keys.public_key());

    state.nostr_client.gift_wrap(receiver, rumor, []).await?;

    Ok(())
}
