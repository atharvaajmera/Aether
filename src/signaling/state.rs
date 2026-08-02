//! Signaling state and routing.

use std::collections::{BTreeMap, BTreeSet};

use crate::signaling::{SignalMessage, SignalingError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedSignal {
    pub recipient_peer_id: String,
    pub message: SignalMessage,
}

#[derive(Debug, Default)]
pub struct SignalingState {
    sessions: BTreeMap<String, BTreeSet<String>>,
    peer_sessions: BTreeMap<String, String>,
}

impl SignalingState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_of(&self, peer_id: &str) -> Option<&str> {
        self.peer_sessions.get(peer_id).map(String::as_str)
    }

    pub fn peers_in_session(&self, session_id: &str) -> Option<Vec<String>> {
        self.sessions
            .get(session_id)
            .map(|peers| peers.iter().cloned().collect())
    }

    pub fn handle(&mut self, message: SignalMessage) -> Result<Vec<RoutedSignal>, SignalingError> {
        match message {
            SignalMessage::JoinSession {
                peer_id,
                session_id,
            } => self.handle_join(peer_id, session_id),
            SignalMessage::LeaveSession {
                peer_id,
                session_id,
            } => self.handle_leave(peer_id, session_id),
            SignalMessage::Offer {
                session_id,
                from_peer_id,
                to_peer_id,
                sdp,
                nat,
            } => {
                let routed = SignalMessage::Offer {
                    session_id: session_id.clone(),
                    from_peer_id: from_peer_id.clone(),
                    to_peer_id: to_peer_id.clone(),
                    sdp,
                    nat,
                };
                self.route_peer_signal(&session_id, &from_peer_id, &to_peer_id, routed)
            }
            SignalMessage::Answer {
                session_id,
                from_peer_id,
                to_peer_id,
                sdp,
            } => {
                let routed = SignalMessage::Answer {
                    session_id: session_id.clone(),
                    from_peer_id: from_peer_id.clone(),
                    to_peer_id: to_peer_id.clone(),
                    sdp,
                };
                self.route_peer_signal(&session_id, &from_peer_id, &to_peer_id, routed)
            }
            SignalMessage::IceCandidate {
                session_id,
                from_peer_id,
                to_peer_id,
                candidate,
                sdp_mid,
                sdp_mline_index,
            } => {
                let routed = SignalMessage::IceCandidate {
                    session_id: session_id.clone(),
                    from_peer_id: from_peer_id.clone(),
                    to_peer_id: to_peer_id.clone(),
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                };
                self.route_peer_signal(&session_id, &from_peer_id, &to_peer_id, routed)
            }
            SignalMessage::PeerJoined { .. }
            | SignalMessage::PeerLeft { .. }
            | SignalMessage::Error { .. } => Err(SignalingError::InvalidSignal(
                "server-generated signals cannot be handled as inbound client messages".into(),
            )),
        }
    }

    fn handle_join(
        &mut self,
        peer_id: String,
        session_id: String,
    ) -> Result<Vec<RoutedSignal>, SignalingError> {
        if peer_id.trim().is_empty() {
            return Err(SignalingError::EmptyPeerId);
        }
        if session_id.trim().is_empty() {
            return Err(SignalingError::EmptySessionId);
        }

        if let Some(current_session) = self.peer_sessions.get(&peer_id) {
            if current_session != &session_id {
                return Err(SignalingError::InvalidSignal(format!(
                    "peer is already attached to session {current_session}"
                )));
            }
        }

        let peers = self.sessions.entry(session_id.clone()).or_default();
        let notifications = peers
            .iter()
            .filter(|existing| *existing != &peer_id)
            .cloned()
            .map(|recipient_peer_id| RoutedSignal {
                recipient_peer_id,
                message: SignalMessage::PeerJoined {
                    peer_id: peer_id.clone(),
                    session_id: session_id.clone(),
                },
            })
            .collect();

        peers.insert(peer_id.clone());
        self.peer_sessions.insert(peer_id, session_id);

        Ok(notifications)
    }

    fn handle_leave(
        &mut self,
        peer_id: String,
        session_id: String,
    ) -> Result<Vec<RoutedSignal>, SignalingError> {
        self.ensure_peer_in_session(&peer_id, &session_id)?;

        let Some(peers) = self.sessions.get_mut(&session_id) else {
            return Err(SignalingError::UnknownSession { session_id });
        };

        peers.remove(&peer_id);
        let notifications = peers
            .iter()
            .cloned()
            .map(|recipient_peer_id| RoutedSignal {
                recipient_peer_id,
                message: SignalMessage::PeerLeft {
                    peer_id: peer_id.clone(),
                    session_id: session_id.clone(),
                },
            })
            .collect();

        if peers.is_empty() {
            self.sessions.remove(&session_id);
        }
        self.peer_sessions.remove(&peer_id);

        Ok(notifications)
    }

    fn route_peer_signal(
        &self,
        session_id: &str,
        from_peer_id: &str,
        to_peer_id: &str,
        message: SignalMessage,
    ) -> Result<Vec<RoutedSignal>, SignalingError> {
        self.ensure_peer_in_session(from_peer_id, session_id)?;
        self.ensure_target_in_session(from_peer_id, to_peer_id, session_id)?;

        Ok(vec![RoutedSignal {
            recipient_peer_id: to_peer_id.to_string(),
            message,
        }])
    }

    fn ensure_peer_in_session(
        &self,
        peer_id: &str,
        session_id: &str,
    ) -> Result<(), SignalingError> {
        match self.peer_sessions.get(peer_id) {
            Some(current) if current == session_id => Ok(()),
            Some(_) | None if !self.sessions.contains_key(session_id) => {
                Err(SignalingError::UnknownSession {
                    session_id: session_id.to_string(),
                })
            }
            Some(_) | None => Err(SignalingError::PeerNotInSession {
                peer_id: peer_id.to_string(),
                session_id: session_id.to_string(),
            }),
        }
    }

    fn ensure_target_in_session(
        &self,
        peer_id: &str,
        target_peer_id: &str,
        session_id: &str,
    ) -> Result<(), SignalingError> {
        let peers =
            self.sessions
                .get(session_id)
                .ok_or_else(|| SignalingError::UnknownSession {
                    session_id: session_id.to_string(),
                })?;

        if peers.contains(target_peer_id) {
            Ok(())
        } else {
            Err(SignalingError::TargetPeerNotInSession {
                peer_id: peer_id.to_string(),
                session_id: session_id.to_string(),
                target_peer_id: target_peer_id.to_string(),
            })
        }
    }
}
