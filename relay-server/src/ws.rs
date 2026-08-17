//! `GET /ws` WebSocket signaling endpoint.

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use plenum::signaling::{RoutedSignal, SignalMessage};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// TEMP DIAG: short human label for a signaling message, including from/to peer
/// ids where present, so relay logs show the full negotiation flow.
fn signal_label(msg: &SignalMessage) -> String {
    match msg {
        SignalMessage::JoinSession {
            peer_id,
            session_id,
        } => {
            format!("JoinSession(peer={peer_id}, session={session_id})")
        }
        SignalMessage::LeaveSession {
            peer_id,
            session_id,
        } => {
            format!("LeaveSession(peer={peer_id}, session={session_id})")
        }
        SignalMessage::PeerJoined {
            peer_id,
            session_id,
        } => {
            format!("PeerJoined(peer={peer_id}, session={session_id})")
        }
        SignalMessage::PeerLeft {
            peer_id,
            session_id,
        } => {
            format!("PeerLeft(peer={peer_id}, session={session_id})")
        }
        SignalMessage::Offer {
            from_peer_id,
            to_peer_id,
            nat,
            ..
        } => {
            format!(
                "Offer(from={from_peer_id}, to={to_peer_id}, nat={})",
                nat.is_some()
            )
        }
        SignalMessage::Answer {
            from_peer_id,
            to_peer_id,
            ..
        } => {
            format!("Answer(from={from_peer_id}, to={to_peer_id})")
        }
        SignalMessage::IceCandidate {
            from_peer_id,
            to_peer_id,
            candidate,
            ..
        } => {
            format!("IceCandidate(from={from_peer_id}, to={to_peer_id}, cand={candidate:?})")
        }
        SignalMessage::Error { message } => format!("Error({message})"),
    }
}

use crate::state::{AppState, PeerHandle};

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Tracks per-connection identity once the peer has successfully joined a
/// session, so we can synthesize a `LeaveSession` on disconnect even if the
/// socket dies without ever sending one.
struct JoinedPeer {
    peer_id: String,
    session_id: String,
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Message>();

    // Task that drains the outbound channel into the actual socket sender half.
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = outbound_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut joined: Option<JoinedPeer> = None;

    loop {
        let next = ws_receiver.next().await;
        let Some(result) = next else {
            // Stream ended (connection closed).
            break;
        };

        let msg = match result {
            Ok(msg) => msg,
            Err(err) => {
                warn!("websocket read error: {err}");
                break;
            }
        };

        let text = match msg {
            Message::Text(text) => text,
            Message::Binary(_) => {
                warn!("ignoring unexpected binary websocket frame");
                continue;
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
        };

        let parsed = match SignalMessage::from_json(text.as_str()) {
            Ok(parsed) => parsed,
            Err(err) => {
                let _ = outbound_tx.send(Message::from(
                    SignalMessage::Error {
                        message: format!("invalid signal message: {err}"),
                    }
                    .to_json()
                    .unwrap_or_default(),
                ));
                continue;
            }
        };

        match parsed {
            SignalMessage::JoinSession {
                peer_id,
                session_id,
            } => {
                if joined.is_some() {
                    let _ = send_error(
                        &outbound_tx,
                        "connection has already joined a session".to_string(),
                    );
                    continue;
                }

                match do_join(&state, &outbound_tx, peer_id.clone(), session_id.clone()).await {
                    Ok(()) => {
                        joined = Some(JoinedPeer {
                            peer_id,
                            session_id,
                        });
                    }
                    Err(err) => {
                        let _ = send_error(&outbound_tx, err.to_string());
                    }
                }
            }
            other => {
                if joined.is_none() {
                    let _ = send_error(
                        &outbound_tx,
                        "must send JoinSession before any other message".to_string(),
                    );
                    continue;
                }

                // A peer may only leave itself / signal on its own behalf; the
                // wire format already carries peer_id/session_id per-message, so
                // we simply route whatever was sent through the shared state.
                let is_leave = matches!(&other, SignalMessage::LeaveSession { .. });
                info!("RECV {}", signal_label(&other));

                let routed = {
                    let mut signaling = state.signaling.lock().expect("signaling mutex poisoned");
                    signaling.handle(other)
                };

                match routed {
                    Ok(signals) => {
                        route_signals(&state, signals).await;
                    }
                    Err(err) => {
                        let _ = send_error(&outbound_tx, err.to_string());
                    }
                }

                if is_leave {
                    // The peer explicitly left; drop our local bookkeeping so
                    // a later JoinSession on the same connection isn't rejected
                    // and so we don't double-send LeaveSession on disconnect.
                    if let Some(j) = joined.take() {
                        state
                            .peers
                            .lock()
                            .expect("peers mutex poisoned")
                            .remove(&j.peer_id);
                    }
                }
            }
        }
    }

    // Connection is closing (gracefully or not). If we ever joined a session
    // and haven't already left it, synthesize a LeaveSession so the other
    // peer(s) reliably get PeerLeft.
    if let Some(JoinedPeer {
        peer_id,
        session_id,
    }) = joined
    {
        info!("DISCONNECT peer={peer_id} session={session_id}");
        state
            .peers
            .lock()
            .expect("peers mutex poisoned")
            .remove(&peer_id);

        let routed = {
            let mut signaling = state.signaling.lock().expect("signaling mutex poisoned");
            signaling.handle(SignalMessage::LeaveSession {
                peer_id: peer_id.clone(),
                session_id,
            })
        };

        match routed {
            Ok(signals) => route_signals(&state, signals).await,
            Err(err) => {
                debug!("leave-on-disconnect for {peer_id} produced no-op: {err}");
            }
        }
    }

    send_task.abort();
    let _ = &mut send_task;
}

fn send_error(
    outbound_tx: &mpsc::UnboundedSender<Message>,
    message: String,
) -> Result<(), mpsc::error::SendError<Message>> {
    let payload = SignalMessage::Error { message }
        .to_json()
        .unwrap_or_else(|_| "{\"type\":\"error\",\"message\":\"internal error\"}".to_string());
    outbound_tx.send(Message::from(payload))
}

/// Handles a `JoinSession` message end-to-end: registers the peer's outbound
/// channel, runs it through the shared `SignalingState`, forwards the
/// resulting notifications, and then synthesizes `PeerJoined` messages back
/// to the newly-joined peer for every peer that was already in the room.
///
/// This last step is intentionally *not* part of `SignalingState::handle`:
/// the shared state only notifies pre-existing members about the new
/// joiner. Doing the reverse here (server-side only) lets a peer that joins
/// second learn about peers that joined earlier, without changing the
/// tested `SignalingState` semantics.
async fn do_join(
    state: &Arc<AppState>,
    outbound_tx: &mpsc::UnboundedSender<Message>,
    peer_id: String,
    session_id: String,
) -> Result<(), plenum::signaling::SignalingError> {
    info!("JOIN peer={peer_id} session={session_id}");
    // Register the peer's outbound handle before routing, so that if another
    // peer's notification races in concurrently it can find us. (The
    // signaling mutex serializes this against concurrent handle() calls
    // anyway, since we hold the peers lock only briefly below.)
    state.peers.lock().expect("peers mutex poisoned").insert(
        peer_id.clone(),
        PeerHandle {
            sender: outbound_tx.clone(),
        },
    );

    let join_msg = SignalMessage::JoinSession {
        peer_id: peer_id.clone(),
        session_id: session_id.clone(),
    };

    let result = {
        let mut signaling = state.signaling.lock().expect("signaling mutex poisoned");
        signaling.handle(join_msg)
    };

    let notifications = match result {
        Ok(notifications) => notifications,
        Err(err) => {
            // Roll back peer registration on failure.
            state
                .peers
                .lock()
                .expect("peers mutex poisoned")
                .remove(&peer_id);
            return Err(err);
        }
    };

    // Forward notifications about the new joiner to pre-existing peers.
    route_signals(state, notifications).await;

    // Synthesize PeerJoined for the newly-joined peer, one per peer that was
    // already in the room (i.e. everyone except ourselves).
    let existing_peers = {
        let signaling = state.signaling.lock().expect("signaling mutex poisoned");
        signaling.peers_in_session(&session_id).unwrap_or_default()
    };

    info!(
        "JOIN peer={peer_id} session={session_id} existing_in_room={}",
        existing_peers.len()
    );
    for other_peer_id in existing_peers {
        if other_peer_id == peer_id {
            continue;
        }
        let synthesized = SignalMessage::PeerJoined {
            peer_id: other_peer_id,
            session_id: session_id.clone(),
        };
        if let Ok(json) = synthesized.to_json() {
            let _ = outbound_tx.send(Message::from(json));
        }
    }

    Ok(())
}

/// Forwards routed signals to their recipients' outbound channels, if
/// currently connected. If a recipient isn't connected, logs a warning and
/// drops the message rather than erroring the connection.
async fn route_signals(state: &Arc<AppState>, signals: Vec<RoutedSignal>) {
    for signal in signals {
        info!(
            "RELAY {} -> {}",
            signal_label(&signal.message),
            signal.recipient_peer_id
        );
        let handle = state
            .peers
            .lock()
            .expect("peers mutex poisoned")
            .get(&signal.recipient_peer_id)
            .cloned();

        match handle {
            Some(handle) => {
                let json = match signal.message.to_json() {
                    Ok(json) => json,
                    Err(err) => {
                        warn!("failed to serialize routed signal: {err}");
                        continue;
                    }
                };
                if handle.sender.send(Message::from(json)).is_err() {
                    warn!(
                        "recipient {} channel closed, dropping signal",
                        signal.recipient_peer_id
                    );
                }
            }
            None => {
                warn!(
                    "recipient {} is not currently connected, dropping signal",
                    signal.recipient_peer_id
                );
            }
        }
    }
}
