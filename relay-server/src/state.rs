//! Shared application state for the relay/signaling server.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use axum::extract::ws::Message;
use plenum::signaling::SignalingState;
use tokio::sync::mpsc;

/// A handle to a connected peer's outbound WebSocket sender half.
///
/// Messages pushed onto `sender` are drained by a background task that
/// forwards them to the peer's actual socket.
#[derive(Debug, Clone)]
pub struct PeerHandle {
    pub sender: mpsc::UnboundedSender<Message>,
}

// Process-wide state shared across all WebSocket connections and HTTP
// handlers.
pub struct AppState {
    pub signaling: Mutex<SignalingState>,
    pub peers: Mutex<HashMap<String, PeerHandle>>,
    pub turn_secret: Option<String>,
    pub turn_urls: Vec<String>,
}

impl AppState {
    pub fn new(turn_secret: Option<String>, turn_urls: Vec<String>) -> Self {
        Self {
            signaling: Mutex::new(SignalingState::new()),
            peers: Mutex::new(HashMap::new()),
            turn_secret,
            turn_urls,
        }
    }

    // Locks the signaling state, recovering the guard if a previous lock
    // holder panicked.
    pub fn lock_signaling(&self) -> MutexGuard<'_, SignalingState> {
        self.signaling
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Locks the peer routing table, recovering the guard on poison.
    pub fn lock_peers(&self) -> MutexGuard<'_, HashMap<String, PeerHandle>> {
        self.peers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn room_exists(&self, session_id: &str) -> bool {
        self.lock_signaling()
            .peers_in_session(session_id)
            .is_some()
    }
}
