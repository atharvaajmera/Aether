// End-to-end LAN transfers over a real TCP loopback socket, exercising the
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use plenum::app::{
    ConnectionState, CorePermissions, PlenumCore, PlenumEvent, ReceiveRequest, SendRequest,
    TransferDirection, TransferEvent, TransferMode, TransferOptions,
};

fn unique_dir(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("plenum-stream-test-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn patterned_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|idx| (idx % 251) as u8).collect()
}

/// Runs a full send/receive round trip on 127.0.0.1 and returns every event
/// both sides emitted (receiver events first, then sender events).
fn transfer_roundtrip(
    label: &str,
    payload_len: usize,
    options: TransferOptions,
) -> Vec<PlenumEvent> {
    let source_dir = unique_dir(&format!("{label}-src"));
    let output_dir = unique_dir(&format!("{label}-out"));

    let file_path = source_dir.join("payload.bin");
    let payload = patterned_bytes(payload_len);
    fs::write(&file_path, &payload).expect("write source file");

    let (event_tx, event_rx) = mpsc::channel::<PlenumEvent>();
    let receiver_events = event_tx.clone();
    let receiver_options = options.clone();
    let receiver_output_dir = output_dir.clone();

    let receiver = thread::spawn(move || {
        let mut core = PlenumCore::new();
        let mut sink = move |event: PlenumEvent| {
            let _ = receiver_events.send(event);
        };
        core.receive_file(
            ReceiveRequest {
                port: 0,
                output_dir: receiver_output_dir,
                announce_on_lan: false,
                device_name: Some("test-receiver".into()),
                require_pin: false,
                auto_accept: true,
                permissions: CorePermissions::full(),
                options: receiver_options,
            },
            &mut sink,
        )
    });

    // The receiver reports its auto-assigned port through the Listening event.
    let port = loop {
        let event = event_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("receiver should report a listening port");
        if let PlenumEvent::Transfer(TransferEvent::StateChanged {
            state: ConnectionState::Listening,
            peer: Some(peer),
            ..
        }) = &event
        {
            let port = peer
                .rsplit(':')
                .next()
                .and_then(|raw| raw.parse::<u16>().ok())
                .expect("listening peer should end in a port");
            let _ = event_tx.send(event);
            break port;
        }
        let _ = event_tx.send(event);
    };

    let mut sender_core = PlenumCore::new();
    let sender_events = event_tx.clone();
    let mut sender_sink = move |event: PlenumEvent| {
        let _ = sender_events.send(event);
    };
    let send_result = sender_core.send_file(
        SendRequest {
            file_path,
            address: Some(format!("127.0.0.1:{port}")),
            discovery_token: None,
            device_name: Some("test-sender".into()),
            permissions: CorePermissions::full(),
            options,
        },
        &mut sender_sink,
    );

    let receive_result = receiver.join().expect("receiver thread should not panic");
    let send_summary = send_result
        .unwrap_or_else(|error| panic!("send failed: {error} (receiver: {receive_result:?})"));
    let receive_summary = receive_result.expect("receive should succeed");

    assert_eq!(send_summary.direction, TransferDirection::Send);
    assert_eq!(send_summary.mode, TransferMode::Lan);
    assert_eq!(send_summary.total_bytes, payload_len as u64);
    assert_eq!(receive_summary.transferred_bytes, payload_len as u64);
    assert_eq!(receive_summary.peer_name.as_deref(), Some("test-sender"));
    assert_eq!(send_summary.peer_name.as_deref(), Some("test-receiver"));

    let received = fs::read(output_dir.join("payload.bin")).expect("read received file");
    assert_eq!(received.len(), payload.len());
    assert_eq!(received, payload, "received bytes must match the source");

    drop(event_tx);
    let events: Vec<PlenumEvent> = event_rx.try_iter().collect();

    let _ = fs::remove_dir_all(source_dir);
    let _ = fs::remove_dir_all(output_dir);
    events
}

fn streaming_diag_count(events: &[PlenumEvent]) -> usize {
    events
        .iter()
        .filter(|event| match event {
            PlenumEvent::Log { message, .. } => message.contains("streaming mode enabled"),
            _ => false,
        })
        .count()
}

#[test]
fn lan_transfer_streams_large_file_with_cumulative_acks() {
    let events = transfer_roundtrip(
        "large",
        5 * 1024 * 1024,
        TransferOptions {
            chunk_size: 256 * 1024,
            window_size: 64,
            timeout_ticks: 15_000,
        },
    );

    assert_eq!(
        streaming_diag_count(&events),
        2,
        "both sender and receiver should negotiate streaming mode"
    );
}

#[test]
fn lan_transfer_streams_small_file_via_finish_ack() {
    let events = transfer_roundtrip(
        "small",
        100 * 1024,
        TransferOptions {
            chunk_size: 32 * 1024,
            window_size: 64,
            timeout_ticks: 15_000,
        },
    );

    assert_eq!(
        streaming_diag_count(&events),
        2,
        "both sender and receiver should negotiate streaming mode"
    );
}
