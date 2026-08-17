//! Integration tests exercising the real axum router end-to-end over actual
//! WebSocket connections (via `tokio-tungstenite` clients), covering:
//!   (a) the join-order synthesis fix (a second-joining peer learns about
//!       the first peer via a synthesized `PeerJoined`),
//!   (b) Offer/Answer/IceCandidate routing between two connected peers,
//!   (c) that dropping one peer's socket delivers `PeerLeft` to the other.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use plenum::signaling::SignalMessage;
use relay_server::AppState;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Spawns the real relay-server axum app bound to an ephemeral loopback port
/// and returns its `ws://` base URL.
async fn spawn_test_server() -> String {
    let state = Arc::new(AppState::new(None, Vec::new()));
    let app = relay_server::build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr: SocketAddr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    format!("ws://{addr}")
}

async fn send_signal(
    ws: &mut (impl SinkExt<WsMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin),
    msg: &SignalMessage,
) {
    let json = msg.to_json().expect("serialize signal");
    ws.send(WsMessage::Text(json.into()))
        .await
        .expect("send ws message");
}

async fn recv_signal(
    ws: &mut (impl StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> SignalMessage {
    let next = timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for message")
        .expect("stream ended unexpectedly")
        .expect("websocket read error");

    match next {
        WsMessage::Text(text) => SignalMessage::from_json(&text).expect("parse SignalMessage"),
        other => panic!("expected text message, got {other:?}"),
    }
}

#[tokio::test]
async fn second_joiner_learns_about_first_peer() {
    let base_url = spawn_test_server().await;
    let session_id = "test-session-join-order";

    let (mut ws_a, _) = tokio_tungstenite::connect_async(format!("{base_url}/ws"))
        .await
        .expect("peer A connect");
    send_signal(
        &mut ws_a,
        &SignalMessage::JoinSession {
            peer_id: "peer-a".to_string(),
            session_id: session_id.to_string(),
        },
    )
    .await;

    // Peer A is first to join; there's no one else in the room yet, so it
    // should receive no immediate notification. We verify this by making
    // sure the first message peer A eventually sees (after B joins) is the
    // real PeerJoined for B, not some leftover synthesized message meant for
    // itself.

    let (mut ws_b, _) = tokio_tungstenite::connect_async(format!("{base_url}/ws"))
        .await
        .expect("peer B connect");
    send_signal(
        &mut ws_b,
        &SignalMessage::JoinSession {
            peer_id: "peer-b".to_string(),
            session_id: session_id.to_string(),
        },
    )
    .await;

    // Peer A (already present) gets notified about B joining, via the
    // normal SignalingState::handle_join path.
    let msg_to_a = recv_signal(&mut ws_a).await;
    assert_eq!(
        msg_to_a,
        SignalMessage::PeerJoined {
            peer_id: "peer-b".to_string(),
            session_id: session_id.to_string(),
        }
    );

    // Peer B (joining second) gets the *synthesized* PeerJoined for A, which
    // SignalingState::handle_join alone would not produce.
    let msg_to_b = recv_signal(&mut ws_b).await;
    assert_eq!(
        msg_to_b,
        SignalMessage::PeerJoined {
            peer_id: "peer-a".to_string(),
            session_id: session_id.to_string(),
        }
    );
}

#[tokio::test]
async fn offer_answer_ice_candidate_route_between_peers() {
    let base_url = spawn_test_server().await;
    let session_id = "test-session-negotiation";

    let (mut ws_a, _) = tokio_tungstenite::connect_async(format!("{base_url}/ws"))
        .await
        .expect("peer A connect");
    send_signal(
        &mut ws_a,
        &SignalMessage::JoinSession {
            peer_id: "peer-a".to_string(),
            session_id: session_id.to_string(),
        },
    )
    .await;

    let (mut ws_b, _) = tokio_tungstenite::connect_async(format!("{base_url}/ws"))
        .await
        .expect("peer B connect");
    send_signal(
        &mut ws_b,
        &SignalMessage::JoinSession {
            peer_id: "peer-b".to_string(),
            session_id: session_id.to_string(),
        },
    )
    .await;

    // Drain the join notifications on both sides before starting negotiation.
    let _ = recv_signal(&mut ws_a).await; // PeerJoined(peer-b) to A
    let _ = recv_signal(&mut ws_b).await; // synthesized PeerJoined(peer-a) to B

    // B (offerer) sends an Offer to A.
    send_signal(
        &mut ws_b,
        &SignalMessage::Offer {
            session_id: session_id.to_string(),
            from_peer_id: "peer-b".to_string(),
            to_peer_id: "peer-a".to_string(),
            sdp: "fake-offer-sdp".to_string(),
            nat: None,
        },
    )
    .await;

    let offer_at_a = recv_signal(&mut ws_a).await;
    assert_eq!(
        offer_at_a,
        SignalMessage::Offer {
            session_id: session_id.to_string(),
            from_peer_id: "peer-b".to_string(),
            to_peer_id: "peer-a".to_string(),
            sdp: "fake-offer-sdp".to_string(),
            nat: None,
        }
    );

    // A (answerer) sends an Answer back to B.
    send_signal(
        &mut ws_a,
        &SignalMessage::Answer {
            session_id: session_id.to_string(),
            from_peer_id: "peer-a".to_string(),
            to_peer_id: "peer-b".to_string(),
            sdp: "fake-answer-sdp".to_string(),
        },
    )
    .await;

    let answer_at_b = recv_signal(&mut ws_b).await;
    assert_eq!(
        answer_at_b,
        SignalMessage::Answer {
            session_id: session_id.to_string(),
            from_peer_id: "peer-a".to_string(),
            to_peer_id: "peer-b".to_string(),
            sdp: "fake-answer-sdp".to_string(),
        }
    );

    // Exchange an ICE candidate from A to B.
    send_signal(
        &mut ws_a,
        &SignalMessage::IceCandidate {
            session_id: session_id.to_string(),
            from_peer_id: "peer-a".to_string(),
            to_peer_id: "peer-b".to_string(),
            candidate: "candidate:fake".to_string(),
            sdp_mid: Some("0".to_string()),
            sdp_mline_index: Some(0),
        },
    )
    .await;

    let candidate_at_b = recv_signal(&mut ws_b).await;
    assert_eq!(
        candidate_at_b,
        SignalMessage::IceCandidate {
            session_id: session_id.to_string(),
            from_peer_id: "peer-a".to_string(),
            to_peer_id: "peer-b".to_string(),
            candidate: "candidate:fake".to_string(),
            sdp_mid: Some("0".to_string()),
            sdp_mline_index: Some(0),
        }
    );
}

#[tokio::test]
async fn dropping_peer_socket_delivers_peer_left() {
    let base_url = spawn_test_server().await;
    let session_id = "test-session-disconnect";

    let (mut ws_a, _) = tokio_tungstenite::connect_async(format!("{base_url}/ws"))
        .await
        .expect("peer A connect");
    send_signal(
        &mut ws_a,
        &SignalMessage::JoinSession {
            peer_id: "peer-a".to_string(),
            session_id: session_id.to_string(),
        },
    )
    .await;

    let (ws_b, _) = tokio_tungstenite::connect_async(format!("{base_url}/ws"))
        .await
        .expect("peer B connect");
    let mut ws_b = ws_b;
    send_signal(
        &mut ws_b,
        &SignalMessage::JoinSession {
            peer_id: "peer-b".to_string(),
            session_id: session_id.to_string(),
        },
    )
    .await;

    // Drain the join notification on A (PeerJoined for B).
    let _ = recv_signal(&mut ws_a).await;

    // Ungracefully drop B's socket without sending LeaveSession.
    drop(ws_b);

    // A should eventually receive a synthesized PeerLeft for B.
    let msg = recv_signal(&mut ws_a).await;
    assert_eq!(
        msg,
        SignalMessage::PeerLeft {
            peer_id: "peer-b".to_string(),
            session_id: session_id.to_string(),
        }
    );
}
