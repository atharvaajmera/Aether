use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

use plenum::protocol::{Packet, PacketType, encode_packet, parse_packet};
use plenum::transport::{TcpTransport, Transport, TransportError};

#[test]
fn io_errors_preserve_category_and_operation() {
    assert_eq!(
        TransportError::from_write_io(
            "write payload",
            Duration::from_secs(30),
            std::io::Error::from(std::io::ErrorKind::TimedOut),
        ),
        TransportError::WriteTimedOut {
            timeout: Duration::from_secs(30),
            operation: "write payload",
        }
    );
    assert_eq!(
        TransportError::from_io(
            "write payload",
            std::io::Error::from(std::io::ErrorKind::ConnectionReset),
        ),
        TransportError::ConnectionReset {
            operation: "write payload"
        }
    );
    assert_eq!(
        TransportError::from_io(
            "write prefix",
            std::io::Error::from(std::io::ErrorKind::BrokenPipe),
        ),
        TransportError::BrokenPipe {
            operation: "write prefix"
        }
    );

    let other = TransportError::from_io(
        "flush",
        std::io::Error::new(std::io::ErrorKind::Other, "diagnostic detail"),
    );
    assert!(matches!(
        other,
        TransportError::Io {
            operation: "flush",
            kind: std::io::ErrorKind::Other,
            ..
        }
    ));
}

fn wait_recv(transport: &mut TcpTransport) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = transport.recv().expect("recv should not fail") {
            return frame;
        }
        assert!(Instant::now() < deadline, "timed out waiting for TCP frame");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn empty_nonblocking_socket_returns_none() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || TcpTransport::accept(&listener).unwrap());
    let _client = TcpStream::connect(address).unwrap();
    let mut transport = server.join().unwrap();

    assert_eq!(transport.recv().unwrap(), None);
    assert!(!transport.is_closed());
}

#[test]
fn exchanges_length_prefixed_frames_over_localhost() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");

    let server = thread::spawn(move || {
        let mut server = TcpTransport::accept(&listener).expect("server should accept client");
        let received = wait_recv(&mut server);
        assert_eq!(received, b"ping".to_vec());
        server.send(b"pong").expect("server send should succeed");
    });

    let mut client = TcpTransport::connect(addr).expect("client should connect");
    client.send(b"ping").expect("client send should succeed");

    let response = wait_recv(&mut client);

    assert_eq!(response, b"pong".to_vec());
    server.join().expect("server thread should finish");
}

#[test]
fn carries_encoded_protocol_packets_over_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");
    let packet = Packet::new(PacketType::Data, 12, b"tcp payload".to_vec());
    let expected = packet.clone();

    let server = thread::spawn(move || {
        let mut server = TcpTransport::accept(&listener).expect("server should accept client");
        let frame = wait_recv(&mut server);
        let parsed = parse_packet(&frame).expect("packet should parse");
        assert_eq!(parsed, expected);

        let ack = Packet::new(PacketType::Ack, parsed.sequence_no, Vec::new());
        let encoded_ack = encode_packet(&ack).expect("ack should encode");
        server
            .send(&encoded_ack)
            .expect("server ack send should succeed");
    });

    let mut client = TcpTransport::connect(addr).expect("client should connect");
    let encoded = encode_packet(&packet).expect("packet should encode");
    client.send(&encoded).expect("client send should succeed");

    let ack_frame = wait_recv(&mut client);
    let ack = parse_packet(&ack_frame).expect("ack should parse");

    assert_eq!(ack.packet_type, PacketType::Ack);
    assert_eq!(ack.sequence_no, 12);
    server.join().expect("server thread should finish");
}

/// Regression: a single length-prefixed frame whose bytes arrive split across
/// multiple socket reads and nonblocking polls must still be reassembled intact.
#[test]
fn reassembles_frame_split_across_reads() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");

    let server = thread::spawn(move || {
        let mut server = TcpTransport::accept(&listener).expect("server should accept client");
        let frame = wait_recv(&mut server);
        assert_eq!(frame, b"hello world".to_vec());
    });

    // Connect a *raw* stream so we control exactly how bytes are flushed.
    let mut raw = TcpStream::connect(addr).expect("raw client should connect");
    let payload = b"hello world";
    let len = (payload.len() as u32).to_be_bytes();

    // Send the prefix and body in separate pieces with empty polls between them.
    raw.write_all(&len[..2]).unwrap();
    raw.flush().unwrap();
    thread::sleep(Duration::from_millis(20));
    raw.write_all(&len[2..]).unwrap();
    raw.flush().unwrap();
    thread::sleep(Duration::from_millis(20));
    raw.write_all(&payload[..4]).unwrap();
    raw.flush().unwrap();
    thread::sleep(Duration::from_millis(20));
    raw.write_all(&payload[4..]).unwrap();
    raw.flush().unwrap();

    server.join().expect("server thread should finish");
}

/// Two frames delivered back-to-back in a single write must both be read out,
/// with the second frame preserved in the buffer across `recv()` calls.
#[test]
fn reads_two_frames_from_one_segment() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");

    let server = thread::spawn(move || {
        let mut server = TcpTransport::accept(&listener).expect("server should accept client");
        let first = wait_recv(&mut server);
        let second = wait_recv(&mut server);
        assert_eq!(first, b"AAAA".to_vec());
        assert_eq!(second, b"BBBBBB".to_vec());
    });

    let mut raw = TcpStream::connect(addr).expect("raw client should connect");
    let mut buf = Vec::new();
    buf.extend_from_slice(&(4u32).to_be_bytes());
    buf.extend_from_slice(b"AAAA");
    buf.extend_from_slice(&(6u32).to_be_bytes());
    buf.extend_from_slice(b"BBBBBB");
    raw.write_all(&buf).unwrap();
    raw.flush().unwrap();

    server.join().expect("server thread should finish");
}

#[test]
fn rejects_frames_larger_than_configured_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");

    let server = thread::spawn(move || {
        let _server = TcpTransport::accept(&listener).expect("server should accept client");
    });

    let mut client = TcpTransport::connect(addr).expect("client should connect");
    client.set_max_frame_len(3);

    let err = client
        .send(b"four")
        .expect_err("oversized frame should fail");

    assert_eq!(err, TransportError::FrameTooLarge { len: 4, max: 3 });
    drop(client);
    server.join().expect("server thread should finish");
}

#[test]
fn rejects_oversized_inbound_length_prefix() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut server = TcpTransport::accept(&listener).unwrap();
        server.set_max_frame_len(3);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match server.recv() {
                Err(error) => return error,
                Ok(None) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(Some(_)) => panic!("oversized frame must not be returned"),
            }
        }
    });

    let mut raw = TcpStream::connect(address).unwrap();
    raw.write_all(&(4_u32).to_be_bytes()).unwrap();
    assert_eq!(
        server.join().unwrap(),
        TransportError::FrameTooLarge { len: 4, max: 3 }
    );
}

#[test]
fn returns_final_frame_before_clean_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut server = TcpTransport::accept(&listener).unwrap();
        assert_eq!(wait_recv(&mut server), b"final".to_vec());

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match server.recv() {
                Err(TransportError::CleanEof { operation: "read" }) => break,
                Ok(None) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(1));
                }
                result => panic!("unexpected result after final frame: {result:?}"),
            }
        }
    });

    let mut raw = TcpStream::connect(address).unwrap();
    raw.write_all(&(5_u32).to_be_bytes()).unwrap();
    raw.write_all(b"final").unwrap();
    drop(raw);
    server.join().unwrap();
}
