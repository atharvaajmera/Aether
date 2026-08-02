use plenum::signaling::{
    IceServer, NatTraversalConfig, RoutedSignal, SignalMessage, SignalingError, SignalingState,
};

#[test]
fn signal_message_json_roundtrip_preserves_nat_config() {
    let message = SignalMessage::Offer {
        session_id: "session-1".into(),
        from_peer_id: "alice".into(),
        to_peer_id: "bob".into(),
        sdp: "offer-sdp".into(),
        nat: Some(NatTraversalConfig::new(vec![
            IceServer::new(vec!["stun:stun.example.com:3478".into()]),
            IceServer::with_credentials(
                vec!["turn:turn.example.com:3478?transport=udp".into()],
                "user",
                "secret",
            ),
        ])),
    };

    let json = message.to_json().expect("message should serialize");
    let decoded = SignalMessage::from_json(&json).expect("message should deserialize");

    assert_eq!(decoded, message);
}

#[test]
fn join_session_notifies_existing_peers() {
    let mut state = SignalingState::new();

    let notifications = state
        .handle(SignalMessage::JoinSession {
            peer_id: "alice".into(),
            session_id: "room-1".into(),
        })
        .expect("first join should succeed");
    assert!(notifications.is_empty());

    let notifications = state
        .handle(SignalMessage::JoinSession {
            peer_id: "bob".into(),
            session_id: "room-1".into(),
        })
        .expect("second join should succeed");

    assert_eq!(notifications.len(), 1);
    assert_eq!(
        notifications[0],
        RoutedSignal {
            recipient_peer_id: "alice".into(),
            message: SignalMessage::PeerJoined {
                peer_id: "bob".into(),
                session_id: "room-1".into(),
            },
        }
    );
    assert_eq!(state.session_of("alice"), Some("room-1"));
    assert_eq!(state.session_of("bob"), Some("room-1"));
}

#[test]
fn routes_offer_answer_and_ice_between_session_peers() {
    let mut state = SignalingState::new();
    for peer_id in ["alice", "bob"] {
        state
            .handle(SignalMessage::JoinSession {
                peer_id: peer_id.into(),
                session_id: "room-2".into(),
            })
            .expect("join should succeed");
    }

    let offer = state
        .handle(SignalMessage::Offer {
            session_id: "room-2".into(),
            from_peer_id: "alice".into(),
            to_peer_id: "bob".into(),
            sdp: "offer".into(),
            nat: None,
        })
        .expect("offer should route");
    assert_eq!(offer.len(), 1);
    assert_eq!(offer[0].recipient_peer_id, "bob");

    let answer = state
        .handle(SignalMessage::Answer {
            session_id: "room-2".into(),
            from_peer_id: "bob".into(),
            to_peer_id: "alice".into(),
            sdp: "answer".into(),
        })
        .expect("answer should route");
    assert_eq!(answer.len(), 1);
    assert_eq!(answer[0].recipient_peer_id, "alice");

    let ice = state
        .handle(SignalMessage::IceCandidate {
            session_id: "room-2".into(),
            from_peer_id: "alice".into(),
            to_peer_id: "bob".into(),
            candidate: "candidate:1 1 UDP 1234 10.0.0.1 5000 typ host".into(),
            sdp_mid: Some("data".into()),
            sdp_mline_index: Some(0),
        })
        .expect("ice candidate should route");
    assert_eq!(ice.len(), 1);
    assert_eq!(ice[0].recipient_peer_id, "bob");
}

#[test]
fn rejects_cross_session_signaling() {
    let mut state = SignalingState::new();
    state
        .handle(SignalMessage::JoinSession {
            peer_id: "alice".into(),
            session_id: "room-a".into(),
        })
        .expect("join should succeed");
    state
        .handle(SignalMessage::JoinSession {
            peer_id: "bob".into(),
            session_id: "room-b".into(),
        })
        .expect("join should succeed");

    let err = state
        .handle(SignalMessage::Offer {
            session_id: "room-a".into(),
            from_peer_id: "alice".into(),
            to_peer_id: "bob".into(),
            sdp: "offer".into(),
            nat: None,
        })
        .expect_err("cross-session offer should fail");

    assert_eq!(
        err,
        SignalingError::TargetPeerNotInSession {
            peer_id: "alice".into(),
            session_id: "room-a".into(),
            target_peer_id: "bob".into(),
        }
    );
}

#[test]
fn leave_session_notifies_remaining_peers_and_removes_membership() {
    let mut state = SignalingState::new();
    for peer_id in ["alice", "bob"] {
        state
            .handle(SignalMessage::JoinSession {
                peer_id: peer_id.into(),
                session_id: "room-3".into(),
            })
            .expect("join should succeed");
    }

    let notifications = state
        .handle(SignalMessage::LeaveSession {
            peer_id: "bob".into(),
            session_id: "room-3".into(),
        })
        .expect("leave should succeed");

    assert_eq!(notifications.len(), 1);
    assert_eq!(
        notifications[0],
        RoutedSignal {
            recipient_peer_id: "alice".into(),
            message: SignalMessage::PeerLeft {
                peer_id: "bob".into(),
                session_id: "room-3".into(),
            },
        }
    );
    assert_eq!(state.session_of("bob"), None);
    assert_eq!(state.session_of("alice"), Some("room-3"));
}

#[test]
fn rejects_invalid_server_generated_inbound_message() {
    let mut state = SignalingState::new();

    let err = state
        .handle(SignalMessage::PeerJoined {
            peer_id: "alice".into(),
            session_id: "room-1".into(),
        })
        .expect_err("server-generated inbound message should fail");

    assert_eq!(
        err,
        SignalingError::InvalidSignal(
            "server-generated signals cannot be handled as inbound client messages".into()
        )
    );
}

#[test]
fn rejoining_same_session_is_allowed_and_renotifies_others() {
    let mut state = SignalingState::new();
    for peer_id in ["alice", "bob"] {
        state
            .handle(SignalMessage::JoinSession {
                peer_id: peer_id.into(),
                session_id: "room-4".into(),
            })
            .expect("join should succeed");
    }

    let notifications = state
        .handle(SignalMessage::JoinSession {
            peer_id: "alice".into(),
            session_id: "room-4".into(),
        })
        .expect("rejoin of the same session should succeed");

    assert_eq!(notifications.len(), 1);
    assert_eq!(
        notifications[0],
        RoutedSignal {
            recipient_peer_id: "bob".into(),
            message: SignalMessage::PeerJoined {
                peer_id: "alice".into(),
                session_id: "room-4".into(),
            },
        }
    );
    assert_eq!(state.session_of("alice"), Some("room-4"));
    assert_eq!(state.session_of("bob"), Some("room-4"));
}

#[test]
fn joining_a_different_session_while_attached_still_fails() {
    let mut state = SignalingState::new();
    state
        .handle(SignalMessage::JoinSession {
            peer_id: "alice".into(),
            session_id: "room-5".into(),
        })
        .expect("join should succeed");

    let err = state
        .handle(SignalMessage::JoinSession {
            peer_id: "alice".into(),
            session_id: "room-6".into(),
        })
        .expect_err("switching sessions without leaving should fail");

    assert_eq!(
        err,
        SignalingError::InvalidSignal("peer is already attached to session room-5".into())
    );
}
