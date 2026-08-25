//! Signaling errors.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalingError {
    EmptyPeerId,
    EmptySessionId,
    UnknownPeer {
        peer_id: String,
    },
    UnknownSession {
        session_id: String,
    },
    PeerAlreadyInSession {
        peer_id: String,
        session_id: String,
    },
    PeerNotInSession {
        peer_id: String,
        session_id: String,
    },
    TargetPeerNotInSession {
        peer_id: String,
        session_id: String,
        target_peer_id: String,
    },
    DuplicatePeerId {
        peer_id: String,
    },
    InvalidSignal(String),
    Json(String),
}

impl fmt::Display for SignalingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPeerId => write!(f, "peer id must not be empty"),
            Self::EmptySessionId => write!(f, "session id must not be empty"),
            Self::UnknownPeer { peer_id } => write!(f, "unknown peer: {peer_id}"),
            Self::UnknownSession { session_id } => write!(f, "unknown session: {session_id}"),
            Self::PeerAlreadyInSession {
                peer_id,
                session_id,
            } => write!(f, "peer {peer_id} is already in session {session_id}"),
            Self::PeerNotInSession {
                peer_id,
                session_id,
            } => write!(f, "peer {peer_id} is not in session {session_id}"),
            Self::TargetPeerNotInSession {
                peer_id,
                session_id,
                target_peer_id,
            } => write!(
                f,
                "peer {peer_id} cannot signal {target_peer_id} outside session {session_id}"
            ),
            Self::DuplicatePeerId { peer_id } => {
                write!(f, "peer id {peer_id} is already in use by another connection")
            }
            Self::InvalidSignal(message) => write!(f, "invalid signaling message: {message}"),
            Self::Json(message) => write!(f, "json signaling error: {message}"),
        }
    }
}

impl std::error::Error for SignalingError {}

impl From<serde_json::Error> for SignalingError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error.to_string())
    }
}
