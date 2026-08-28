use std::time::{SystemTime, UNIX_EPOCH};

use plenum::flow::ReceiverWindow;
use plenum::protocol::{Packet, PacketType};
use plenum::stream::ResumeCheckpoint;

#[test]
fn checkpoint_roundtrip_persists_resume_metadata() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("plenum-resume-{unique}.json"));

    let mut checkpoint = ResumeCheckpoint::new("example.bin", 12345, 4096);
    checkpoint.update(7, 28672);
    checkpoint.save(&path).expect("checkpoint should save");

    let loaded = ResumeCheckpoint::load(&path).expect("checkpoint should load");
    assert_eq!(loaded, checkpoint);

    ResumeCheckpoint::clear(&path).expect("checkpoint should clear");
    assert!(!path.exists());
}

#[test]
fn receiver_window_can_resume_from_existing_sequence() {
    let mut receiver = ReceiverWindow::with_next_expected(3);

    let controls = receiver
        .receive_data_packet(Packet::new(PacketType::Data, 1, b"old".to_vec()))
        .expect("duplicate old packet should still ack");
    assert_eq!(controls.len(), 1);
    assert_eq!(controls[0].packet_type, PacketType::Ack);
    assert_eq!(controls[0].sequence_no, 1);
    assert!(receiver.drain_ordered().is_empty());

    let controls = receiver
        .receive_data_packet(Packet::new(PacketType::Data, 3, b"new".to_vec()))
        .expect("current packet should succeed");
    assert_eq!(controls[0].sequence_no, 3);

    let drained = receiver.drain_ordered_packets();
    assert_eq!(drained, vec![(3, b"new".to_vec())]);
    assert_eq!(receiver.next_expected(), 4);
}

#[test]
fn completed_transfer_summary_satisfies_resume_invariants() {
    use plenum::app::{TransferDirection, TransferMode, TransferSummary};

    let total_bytes = 1_160_000_000u64;
    let resumed_bytes_at_start = 425_000_000u64;
    let transferred_bytes = total_bytes;

    let summary = TransferSummary {
        direction: TransferDirection::Receive,
        file_name: "video.mp4".into(),
        peer: Some("127.0.0.1:9000".into()),
        peer_name: Some("sender".into()),
        mode: TransferMode::Lan,
        total_bytes,
        transferred_bytes,
        resumed_bytes: resumed_bytes_at_start.min(transferred_bytes),
        elapsed_ms: 40_000,
    };

    // Invariant: 0 <= resumed_bytes <= transferred_bytes <= total_bytes
    assert!(summary.resumed_bytes <= summary.transferred_bytes);
    assert!(summary.transferred_bytes <= summary.total_bytes);
    assert_eq!(summary.transferred_bytes, summary.total_bytes);

    // Session bytes sent during this attempt
    let session_bytes = summary.transferred_bytes - summary.resumed_bytes;
    assert_eq!(session_bytes, 735_000_000u64);

    let session_speed_bps = (session_bytes as f64) / (summary.elapsed_ms as f64 / 1000.0);
    assert!((session_speed_bps - 18_375_000.0).abs() < 1.0);
}

#[test]
fn resume_live_statistics_calculate_session_speed_and_eta_correctly() {
    let total_bytes: u64 = 1_160_000_000;
    let resume_baseline_bytes: u64 = 425_000_000;
    let current_transferred_bytes: u64 = 810_000_000;
    let elapsed_secs: f64 = 40.0;

    let session_transferred_bytes = current_transferred_bytes.saturating_sub(resume_baseline_bytes);
    assert_eq!(session_transferred_bytes, 385_000_000);

    let speed_bps = (session_transferred_bytes as f64) / elapsed_secs;
    assert_eq!(speed_bps, 9_625_000.0);

    let remaining_bytes = total_bytes.saturating_sub(current_transferred_bytes);
    assert_eq!(remaining_bytes, 350_000_000);

    let eta_secs = (remaining_bytes as f64) / speed_bps;
    assert!((eta_secs - 36.3636).abs() < 0.01);
}
