use serde::{Deserialize, Serialize};

/// Inbound message from a voter to the EC (JSON inside Gift Wrap rumor content).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum InboundMessage {
    Register {
        election_id: String,
        registration_token: String,
    },
    RequestToken {
        election_id: String,
        blinded_nonce: String,
    },
    CastVote {
        election_id: String,
        candidate_ids: Vec<u8>,
        h_n: String,
        token: String,
    },
}

/// Outbound message from the EC to a voter (JSON inside Gift Wrap rumor content).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum OutboundMessage {
    Ok(OkResponse),
    Error(ErrorResponse),
}

#[derive(Debug, Clone, Serialize)]
pub struct OkResponse {
    pub status: &'static str,
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blind_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    pub status: &'static str,
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Longest `request_id` the EC will echo back. Anything larger is treated as
/// absent rather than reflected into the reply.
pub const MAX_REQUEST_ID_LEN: usize = 64;

/// Best-effort extraction of the voter-supplied `request_id` from an already
/// parsed JSON value. Works on any JSON object, even when the message fails
/// to parse as a known action, so INVALID_MESSAGE errors stay correlatable.
pub fn request_id_from_value(value: &serde_json::Value) -> Option<String> {
    value
        .get("request_id")?
        .as_str()
        .filter(|id| id.len() <= MAX_REQUEST_ID_LEN)
        .map(String::from)
}

/// [`request_id_from_value`] over raw rumor content.
pub fn extract_request_id(content: &str) -> Option<String> {
    request_id_from_value(&serde_json::from_str(content).ok()?)
}

impl OutboundMessage {
    pub fn ok(action: &'static str) -> Self {
        Self::Ok(OkResponse {
            status: "ok",
            action,
            blind_signature: None,
            request_id: None,
        })
    }

    pub fn ok_with_signature(action: &'static str, blind_signature: String) -> Self {
        Self::Ok(OkResponse {
            status: "ok",
            action,
            blind_signature: Some(blind_signature),
            request_id: None,
        })
    }

    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self::Error(ErrorResponse {
            status: "error",
            code,
            message: message.into(),
            request_id: None,
        })
    }

    /// Stamp the voter-supplied `request_id` onto this reply so the client
    /// can correlate it with its in-flight request. Handlers stay unaware of
    /// correlation; the listener applies this just before sending.
    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        match &mut self {
            Self::Ok(ok) => ok.request_id = request_id,
            Self::Error(err) => err.request_id = request_id,
        }
        self
    }
}
