//! End-to-end integration test for `RtcTransport`: spins up the real
//! relay-server axum router in-process, then drives two real
//! `RtcTransport::connect_as_offerer` / `connect_as_answerer` sessions against
//! it over loopback, performing genuine WebRTC ICE/DTLS negotiation (no fake
//! SDP, unlike `relay-server/tests/ws_routing.rs`).
//!
//! This is inherently slower and more timing-sensitive than the rest of this
//! repo's synchronous transport tests (see `tests/transport_tcp.rs`), since
//! real ICE gathering and DTLS handshakes take real wall-clock time even on
//! 127.0.0.1.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use plenum::rtc::{RtcError, RtcTransport};
use plenum::signaling::IceServer;
use plenum::transport::Transport;
use relay_server::AppState;
use tokio::net::TcpListener;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Spawns the real relay-server axum app bound to an ephemeral loopback port
/// inside `runtime`, returning its `ws://.../ws` signaling URL.
fn spawn_relay(runtime: &tokio::runtime::Runtime) -> String {
    runtime.block_on(async {
        let state = Arc::new(AppState::new(None, Vec::new()));
        let app = relay_server::build_router(state);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr: SocketAddr = listener.local_addr().expect("local addr");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("relay server error");
        });

        format!("ws://{addr}/ws")
    })
}

#[test]
fn rtc_transport_roundtrip_over_loopback_relay() {
    // The relay server runs on its own dedicated multi-thread tokio runtime,
    // kept alive for the duration of the test by keeping `relay_runtime` in
    // scope. `RtcTransport::connect_as_offerer`/`connect_as_answerer` are
    // blocking calls that each spin up their own *separate* background
    // runtime internally (see `src/rtc/runtime.rs`), so they must be invoked
    // from plain OS threads, not from inside this (or any) tokio runtime.
    let relay_runtime = tokio::runtime::Runtime::new().expect("build relay runtime");
    let relay_url = spawn_relay(&relay_runtime);

    let session_id = "rtc-roundtrip-test-session".to_string();

    let offerer_url = relay_url.clone();
    let offerer_session = session_id.clone();
    let offerer_thread = std::thread::spawn(move || {
        RtcTransport::connect_as_offerer(
            &offerer_url,
            &offerer_session,
            "rtc-test-offerer",
            vec![] as Vec<IceServer>,
            CONNECT_TIMEOUT,
        )
    });

    let answerer_url = relay_url.clone();
    let answerer_session = session_id.clone();
    let answerer_thread = std::thread::spawn(move || {
        RtcTransport::connect_as_answerer(
            &answerer_url,
            &answerer_session,
            "rtc-test-answerer",
            vec![] as Vec<IceServer>,
            CONNECT_TIMEOUT,
        )
    });

    let offerer_result = offerer_thread.join().expect("offerer thread panicked");
    let answerer_result = answerer_thread.join().expect("answerer thread panicked");

    let mut offerer = match offerer_result {
        Ok(transport) => transport,
        Err(error) => panic!("offerer failed to connect: {error}"),
    };
    let mut answerer = match answerer_result {
        Ok(transport) => transport,
        Err(error) => panic!("answerer failed to connect: {error}"),
    };

    // Build a 200KB pseudo-random-ish (deterministic) payload and split it
    // into 32KB chunks to respect SCTP message-size limits.
    const TOTAL_LEN: usize = 200 * 1024;
    const CHUNK_LEN: usize = 32 * 1024;

    let payload: Vec<u8> = (0..TOTAL_LEN).map(|i| (i % 256) as u8).collect();

    for chunk in payload.chunks(CHUNK_LEN) {
        offerer.send(chunk).expect("offerer send should succeed");
    }

    let mut received = Vec::with_capacity(TOTAL_LEN);
    let recv_deadline = std::time::Instant::now() + Duration::from_secs(30);
    while received.len() < TOTAL_LEN {
        match answerer.recv() {
            Ok(Some(bytes)) => received.extend_from_slice(&bytes),
            Ok(None) => {
                if std::time::Instant::now() > recv_deadline {
                    panic!(
                        "timed out waiting for data; received {} of {} bytes so far",
                        received.len(),
                        TOTAL_LEN
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("answerer recv failed: {error:?}"),
        }
    }

    assert_eq!(received.len(), TOTAL_LEN);
    assert_eq!(received, payload);

    offerer.close().expect("offerer close should succeed");
    answerer.close().expect("answerer close should succeed");

    relay_runtime.shutdown_background();
}

#[test]
fn rejects_when_relay_unreachable() {
    // Nothing is listening on this port; the connect attempt should fail
    // (connection refused, or WS handshake never completes) well within the
    // short timeout below, rather than hanging indefinitely.
    let unreachable_url = "ws://127.0.0.1:1/ws";
    let short_timeout = Duration::from_secs(3);

    let start = std::time::Instant::now();
    let result = RtcTransport::connect_as_offerer(
        unreachable_url,
        "rtc-unreachable-session",
        "rtc-test-offerer-unreachable",
        vec![] as Vec<IceServer>,
        short_timeout,
    );
    let elapsed = start.elapsed();

    match result {
        Ok(_) => panic!("expected connect_as_offerer to fail against an unreachable relay"),
        Err(error) => {
            println!("connect_as_offerer failed as expected: {error}");
        }
    }

    assert!(
        elapsed <= short_timeout + Duration::from_secs(5),
        "connect attempt took {elapsed:?}, expected it to fail within roughly {short_timeout:?}"
    );
}

// Keep RtcError referenced so the import isn't flagged unused if the compiler
// can't infer it's needed via the `Err(error)` binding's type alone in all
// configurations.
#[allow(dead_code)]
fn _use_rtc_error(_: &RtcError) {}
