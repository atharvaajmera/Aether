use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

use plenum::transport::{SecureTransport, TcpTransport, Transport, TransportResult};

struct RecordingTransport<T> {
    inner: T,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl<T: Transport> Transport for RecordingTransport<T> {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        self.sent.lock().unwrap().push(bytes.to_vec());
        self.inner.send(bytes)
    }
    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        self.inner.recv()
    }
    fn close(&mut self) -> TransportResult<()> {
        self.inner.close()
    }
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

fn wait_recv<T: Transport>(transport: &mut T) -> TransportResult<Vec<u8>> {
    for _ in 0..2000 {
        if let Some(frame) = transport.recv()? {
            return Ok(frame);
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for frame")
}

#[test]
fn encrypted_transport_hides_plaintext_and_roundtrips_with_pin() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let server_capture = captured.clone();

    let server = thread::spawn(move || {
        let tcp = TcpTransport::accept(&listener).unwrap();
        let recording = RecordingTransport {
            inner: tcp,
            sent: server_capture,
        };
        let mut secure = SecureTransport::accept(recording, Some("ABC234")).unwrap();
        let message = wait_recv(&mut secure).unwrap();
        assert_eq!(message, b"private-file-name.txt::PRIVATE-FILE-CONTENTS");
        secure.send(b"authenticated response").unwrap();
    });

    let tcp = TcpTransport::connect(address).unwrap();
    let mut secure = SecureTransport::connect(tcp, Some("abc234")).unwrap();
    secure
        .send(b"private-file-name.txt::PRIVATE-FILE-CONTENTS")
        .unwrap();
    assert_eq!(wait_recv(&mut secure).unwrap(), b"authenticated response");
    server.join().unwrap();

    let wire = captured.lock().unwrap().concat();
    assert!(
        !wire
            .windows(b"private-file-name.txt".len())
            .any(|w| w == b"private-file-name.txt")
    );
    assert!(
        !wire
            .windows(b"PRIVATE-FILE-CONTENTS".len())
            .any(|w| w == b"PRIVATE-FILE-CONTENTS")
    );
}

#[test]
fn wrong_pin_rejects_the_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let tcp = TcpTransport::accept(&listener).unwrap();
        SecureTransport::accept(tcp, Some("RIGHT1"))
            .err()
            .expect("server must reject wrong PIN")
    });

    let tcp = TcpTransport::connect(address).unwrap();
    let error = SecureTransport::connect(tcp, Some("WRONG1"))
        .err()
        .expect("client must reject proof");
    assert!(error.to_string().contains("pairing authentication failed"));
    let _ = server.join().unwrap();
}

struct MutatingTransport<T> {
    inner: T,
    encrypted_sends: usize,
    replay: bool,
}

impl<T: Transport> Transport for MutatingTransport<T> {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        if bytes.len() >= 28 && !bytes.starts_with(b"PLNSEC01") && bytes.len() != 32 {
            self.encrypted_sends += 1;
            if self.replay && self.encrypted_sends == 1 {
                self.inner.send(bytes)?;
                return self.inner.send(bytes);
            }
            if !self.replay && self.encrypted_sends == 1 {
                let mut tampered = bytes.to_vec();
                tampered[21] ^= 1;
                return self.inner.send(&tampered);
            }
        }
        self.inner.send(bytes)
    }
    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        self.inner.recv()
    }
    fn close(&mut self) -> TransportResult<()> {
        self.inner.close()
    }
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

fn rejection_case(replay: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let tcp = TcpTransport::accept(&listener).unwrap();
        let mut secure = SecureTransport::accept(tcp, None).unwrap();
        if replay {
            let _ = wait_recv(&mut secure).unwrap();
            wait_recv(&mut secure).unwrap_err().to_string()
        } else {
            wait_recv(&mut secure).unwrap_err().to_string()
        }
    });
    let tcp = TcpTransport::connect(address).unwrap();
    let mut secure = SecureTransport::connect(
        MutatingTransport {
            inner: tcp,
            encrypted_sends: 0,
            replay,
        },
        None,
    )
    .unwrap();
    secure.send(b"secret").unwrap();
    server.join().unwrap()
}

#[test]
fn tampered_ciphertext_is_rejected() {
    assert!(rejection_case(false).contains("failed to decrypt payload"));
}

#[test]
fn replayed_ciphertext_is_rejected() {
    let error = rejection_case(true);
    assert!(error.contains("unexpected encrypted frame counter"));
    assert!(error.contains("expected_counter=1"));
    assert!(error.contains("received_counter=0"));
}

#[derive(Clone, Copy)]
enum EnvelopeMutation {
    ModifyCounter,
    ModifyTag,
    UnsupportedVersion,
    Truncate,
    OmitFirst,
    SwapFirstTwo,
}

struct EnvelopeMutatingTransport<T> {
    inner: T,
    mutation: EnvelopeMutation,
    encrypted_sends: usize,
    pending: Option<Vec<u8>>,
}

impl<T: Transport> Transport for EnvelopeMutatingTransport<T> {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        let is_envelope = bytes.len() >= 37 && bytes.first() == Some(&2);
        if !is_envelope {
            return self.inner.send(bytes);
        }
        self.encrypted_sends += 1;
        match (self.mutation, self.encrypted_sends) {
            (EnvelopeMutation::ModifyCounter, 1) => {
                let mut mutated = bytes.to_vec();
                mutated[8] ^= 1;
                self.inner.send(&mutated)
            }
            (EnvelopeMutation::ModifyTag, 1) => {
                let mut mutated = bytes.to_vec();
                *mutated.last_mut().unwrap() ^= 1;
                self.inner.send(&mutated)
            }
            (EnvelopeMutation::UnsupportedVersion, 1) => {
                let mut mutated = bytes.to_vec();
                mutated[0] = 3;
                self.inner.send(&mutated)
            }
            (EnvelopeMutation::Truncate, 1) => self.inner.send(&bytes[..20]),
            (EnvelopeMutation::OmitFirst, 1) => Ok(()),
            (EnvelopeMutation::SwapFirstTwo, 1) => {
                self.pending = Some(bytes.to_vec());
                Ok(())
            }
            (EnvelopeMutation::SwapFirstTwo, 2) => {
                self.inner.send(bytes)?;
                self.inner.send(self.pending.take().as_deref().unwrap())
            }
            _ => self.inner.send(bytes),
        }
    }

    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        self.inner.recv()
    }
    fn close(&mut self) -> TransportResult<()> {
        self.inner.close()
    }
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

fn envelope_rejection_case(mutation: EnvelopeMutation, sends: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let tcp = TcpTransport::accept(&listener).unwrap();
        let mut secure = SecureTransport::accept(tcp, None).unwrap();
        wait_recv(&mut secure).unwrap_err().to_string()
    });
    let tcp = TcpTransport::connect(address).unwrap();
    let mut secure = SecureTransport::connect(
        EnvelopeMutatingTransport {
            inner: tcp,
            mutation,
            encrypted_sends: 0,
            pending: None,
        },
        None,
    )
    .unwrap();
    for index in 0..sends {
        secure.send(&[index as u8]).unwrap();
    }
    server.join().unwrap()
}

#[test]
fn omitted_encrypted_frame_is_rejected() {
    let error = envelope_rejection_case(EnvelopeMutation::OmitFirst, 2);
    assert!(error.contains("unexpected encrypted frame counter"));
    assert!(error.contains("expected_counter=0"));
    assert!(error.contains("received_counter=1"));
}

#[test]
fn swapped_encrypted_frames_are_rejected() {
    let error = envelope_rejection_case(EnvelopeMutation::SwapFirstTwo, 2);
    assert!(error.contains("unexpected encrypted frame counter"));
    assert!(error.contains("expected_counter=0"));
    assert!(error.contains("received_counter=1"));
}

#[test]
fn modified_encrypted_frame_counter_is_rejected() {
    assert!(
        envelope_rejection_case(EnvelopeMutation::ModifyCounter, 1)
            .contains("unexpected encrypted frame counter")
    );
}

#[test]
fn modified_encrypted_frame_tag_is_rejected() {
    let error = envelope_rejection_case(EnvelopeMutation::ModifyTag, 1);
    assert!(error.contains("failed to authenticate encrypted frame"));
    assert!(error.contains("counter=0"));
}

#[test]
fn unsupported_encrypted_envelope_version_is_rejected() {
    assert!(
        envelope_rejection_case(EnvelopeMutation::UnsupportedVersion, 1)
            .contains("unsupported encrypted envelope version")
    );
}

#[test]
fn truncated_encrypted_envelope_is_rejected() {
    assert!(
        envelope_rejection_case(EnvelopeMutation::Truncate, 1)
            .contains("truncated encrypted frame")
    );
}

fn run_encrypted_tcp_transfer(frame_count: usize) {
    const CHUNK_SIZE: usize = 256 * 1024;
    const ACK_TIMEOUT: Duration = Duration::from_secs(10);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let tcp = TcpTransport::accept(&listener).unwrap();
        let mut secure = SecureTransport::accept(tcp, None).unwrap();
        let mut received_hash = Sha256::new();

        for index in 0..frame_count {
            let frame = wait_recv(&mut secure).unwrap();
            let expected = vec![(index as u8).wrapping_add(17); CHUNK_SIZE];
            assert_eq!(frame, expected);
            received_hash.update(&frame);
            secure.send(&(index as u64).to_be_bytes()).unwrap();
        }

        received_hash.finalize().to_vec()
    });

    let tcp = TcpTransport::connect(address).unwrap();
    let mut secure = SecureTransport::connect(tcp, None).unwrap();
    let mut sent_hash = Sha256::new();

    for index in 0..frame_count {
        let frame = vec![(index as u8).wrapping_add(17); CHUNK_SIZE];
        sent_hash.update(&frame);
        secure.send(&frame).unwrap();
        let deadline = std::time::Instant::now() + ACK_TIMEOUT;
        loop {
            if let Some(ack) = secure.recv().unwrap() {
                assert_eq!(ack, (index as u64).to_be_bytes());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for ACK"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    assert_eq!(server.join().unwrap(), sent_hash.finalize().to_vec());
}

#[test]
fn encrypted_tcp_sustains_production_sized_transfer() {
    run_encrypted_tcp_transfer(64);
}

#[test]
#[ignore = "large encrypted TCP transfer stress test"]
fn encrypted_tcp_sustains_large_transfer() {
    run_encrypted_tcp_transfer(512);
}
