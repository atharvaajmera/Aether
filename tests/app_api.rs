use plenum::app::{
    AppError, BenchmarkRequest, CorePermissions, DiscoverRequest, PermissionKind, PlenumCore,
    TransferOptions,
};
use plenum::signaling::SignalMessage;

#[test]
fn benchmark_api_runs_through_high_level_core() {
    let mut core = PlenumCore::new();
    let mut events = Vec::new();

    let summary = core
        .benchmark(
            BenchmarkRequest {
                size_mb: 1,
                iterations: 1,
                latency_ticks: 1,
                options: TransferOptions::default(),
            },
            &mut |event| events.push(event),
        )
        .expect("benchmark should succeed");

    assert_eq!(summary.size_mb, 1);
    assert_eq!(summary.iterations.len(), 1);
    assert!(!events.is_empty());
}

#[test]
fn discover_api_requires_local_network_permission() {
    let mut core = PlenumCore::new();
    let mut sink = |_event| {};

    let err = core
        .discover_peer(
            DiscoverRequest {
                token: None,
                timeout_secs: 1,
                permissions: CorePermissions {
                    local_network: false,
                    file_system_read: true,
                    file_system_write: true,
                    background_transfer: false,
                },
            },
            &mut sink,
        )
        .expect_err("discover should require local network permission");

    match err {
        AppError::PermissionDenied { permission, .. } => {
            assert_eq!(permission, PermissionKind::LocalNetwork);
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn signaling_routes_through_high_level_core() {
    let mut core = PlenumCore::new();

    core.handle_signal(SignalMessage::JoinSession {
        peer_id: "alice".into(),
        session_id: "room-1".into(),
    })
    .unwrap();
    core.handle_signal(SignalMessage::JoinSession {
        peer_id: "bob".into(),
        session_id: "room-1".into(),
    })
    .unwrap();

    let routed = core
        .handle_signal(SignalMessage::Offer {
            session_id: "room-1".into(),
            from_peer_id: "alice".into(),
            to_peer_id: "bob".into(),
            sdp: "offer".into(),
            nat: None,
        })
        .expect("offer should route");

    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0].recipient_peer_id, "bob");
    assert_eq!(core.session_of("alice"), Some("room-1".into()));
}
