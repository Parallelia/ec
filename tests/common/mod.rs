//! Shared test helpers: a minimal in-memory Nostr relay.
//!
//! The relay speaks just enough of the protocol (NIP-01) for the EC daemon
//! and test voter clients to exchange events: `EVENT` is acknowledged with
//! `OK` and broadcast to every open subscription, `REQ` is answered with
//! `EOSE`. Filters are ignored — clients receive everything and filter
//! locally, which is sufficient for tests.
#![allow(dead_code)]

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

/// Install a TRACE-level subscriber so `tracing` macro bodies execute during
/// tests. Idempotent: only the first call in the process wins.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_test_writer()
        .try_init();
}

/// Start a fake relay on an ephemeral port and return its `ws://` URL.
/// The relay runs until the test process exits.
pub async fn start_fake_relay() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, _rx) = broadcast::channel::<String>(256);

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                let (mut sink, mut source) = ws.split();
                let mut rx = tx.subscribe();
                let mut subs: Vec<String> = Vec::new();
                loop {
                    tokio::select! {
                        msg = source.next() => {
                            let Some(Ok(msg)) = msg else { break };
                            let Message::Text(text) = msg else { continue };
                            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                                continue;
                            };
                            match v.get(0).and_then(|t| t.as_str()) {
                                Some("EVENT") => {
                                    let event = v[1].clone();
                                    let id = event["id"].as_str().unwrap_or_default();
                                    let ok = format!(r#"["OK","{id}",true,""]"#);
                                    if sink.send(Message::from(ok)).await.is_err() {
                                        break;
                                    }
                                    let _ = tx.send(event.to_string());
                                }
                                Some("REQ") => {
                                    let sub_id =
                                        v.get(1).and_then(|s| s.as_str()).unwrap_or_default();
                                    let eose = format!(r#"["EOSE","{sub_id}"]"#);
                                    if sink.send(Message::from(eose)).await.is_err() {
                                        break;
                                    }
                                    subs.push(sub_id.to_string());
                                }
                                Some("CLOSE") => {
                                    if let Some(sub_id) = v.get(1).and_then(|s| s.as_str()) {
                                        subs.retain(|s| s != sub_id);
                                    }
                                }
                                _ => {}
                            }
                        }
                        ev = rx.recv() => {
                            let Ok(ev) = ev else { continue };
                            for sub in &subs {
                                let msg = format!(r#"["EVENT","{sub}",{ev}]"#);
                                if sink.send(Message::from(msg)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            });
        }
    });

    format!("ws://{addr}")
}
