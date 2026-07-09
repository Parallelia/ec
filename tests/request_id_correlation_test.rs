//! Tests for request/response correlation via the `request_id` echo.
//!
//! Voters include an optional `request_id` in each request; the EC echoes it
//! in every reply (ok and error) so clients can match responses to in-flight
//! requests and ignore replayed Gift Wraps re-delivered by relays.

use ec::nostr::messages::{OutboundMessage, extract_request_id};

// --- extract_request_id ---

#[test]
fn extracts_request_id_from_valid_request() {
    // Arrange
    let content = r#"{"action":"register","election_id":"e1","registration_token":"t","request_id":"abc123"}"#;

    // Act
    let id = extract_request_id(content);

    // Assert
    assert_eq!(id.as_deref(), Some("abc123"));
}

#[test]
fn extracts_request_id_even_when_action_is_unknown() {
    // Correlation must survive INVALID_MESSAGE errors: the id is pulled from
    // the raw JSON, not from a successfully parsed action.
    let content = r#"{"action":"no-such-action","request_id":"abc123"}"#;

    let id = extract_request_id(content);

    assert_eq!(id.as_deref(), Some("abc123"));
}

#[test]
fn returns_none_when_request_id_missing() {
    let content = r#"{"action":"register","election_id":"e1","registration_token":"t"}"#;

    let id = extract_request_id(content);

    assert_eq!(id, None);
}

#[test]
fn returns_none_when_request_id_is_not_a_string() {
    let content = r#"{"action":"register","request_id":42}"#;

    let id = extract_request_id(content);

    assert_eq!(id, None);
}

#[test]
fn returns_none_for_malformed_json() {
    let id = extract_request_id("not json at all");

    assert_eq!(id, None);
}

#[test]
fn returns_none_when_request_id_exceeds_length_cap() {
    // Don't echo unbounded attacker-chosen strings back into replies.
    let long_id = "x".repeat(65);
    let content = format!(r#"{{"action":"register","request_id":"{long_id}"}}"#);

    let id = extract_request_id(&content);

    assert_eq!(id, None);
}

#[test]
fn accepts_request_id_at_length_cap() {
    let max_id = "x".repeat(64);
    let content = format!(r#"{{"action":"register","request_id":"{max_id}"}}"#);

    let id = extract_request_id(&content);

    assert_eq!(id.as_deref(), Some(max_id.as_str()));
}

// --- OutboundMessage echo ---

#[test]
fn ok_response_echoes_request_id() {
    // Arrange
    let response =
        OutboundMessage::ok("register-confirmed").with_request_id(Some("abc123".to_string()));

    // Act
    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();

    // Assert
    assert_eq!(json["status"], "ok");
    assert_eq!(json["action"], "register-confirmed");
    assert_eq!(json["request_id"], "abc123");
}

#[test]
fn ok_with_signature_echoes_request_id() {
    let response = OutboundMessage::ok_with_signature("token-issued", "sig".to_string())
        .with_request_id(Some("id-1".to_string()));

    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();

    assert_eq!(json["action"], "token-issued");
    assert_eq!(json["blind_signature"], "sig");
    assert_eq!(json["request_id"], "id-1");
}

#[test]
fn error_response_echoes_request_id() {
    let response = OutboundMessage::error("INVALID_TOKEN", "bad token")
        .with_request_id(Some("abc123".to_string()));

    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();

    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], "INVALID_TOKEN");
    assert_eq!(json["request_id"], "abc123");
}

#[test]
fn responses_omit_request_id_when_absent() {
    // Legacy voters send no request_id; replies must not grow a null field.
    let ok = OutboundMessage::ok("register-confirmed").with_request_id(None);
    let err = OutboundMessage::error("INTERNAL_ERROR", "boom").with_request_id(None);

    let ok_json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
    let err_json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&err).unwrap()).unwrap();

    assert!(ok_json.get("request_id").is_none());
    assert!(err_json.get("request_id").is_none());
}
