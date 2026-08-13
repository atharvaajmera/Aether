use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> { self.inner.recv() }
    fn close(&mut self) -> TransportResult<()> { self.inner.close() }
    fn is_closed(&self) -> bool { self.inner.is_closed() }
}

fn wait_recv<T: Transport>(transport: &mut T) -> TransportResult<Vec<u8>> {
    for _ in 0..2000 {
        if let Some(frame) = transport.recv()? { return Ok(frame); }
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
        let recording = RecordingTransport { inner: tcp, sent: server_capture };
        let mut secure = SecureTransport::accept(recording, Some("ABC234")).unwrap();
        let message = wait_recv(&mut secure).unwrap();
        assert_eq!(message, b"private-file-name.txt::PRIVATE-FILE-CONTENTS");
        secure.send(b"authenticated response").unwrap();
    });

    let tcp = TcpTransport::connect(address).unwrap();
    let mut secure = SecureTransport::connect(tcp, Some("abc234")).unwrap();
    secure.send(b"private-file-name.txt::PRIVATE-FILE-CONTENTS").unwrap();
    assert_eq!(wait_recv(&mut secure).unwrap(), b"authenticated response");
    server.join().unwrap();

    let wire = captured.lock().unwrap().concat();
    assert!(!wire.windows(b"private-file-name.txt".len()).any(|w| w == b"private-file-name.txt"));
    assert!(!wire.windows(b"PRIVATE-FILE-CONTENTS".len()).any(|w| w == b"PRIVATE-FILE-CONTENTS"));
}

#[test]
fn wrong_pin_rejects_the_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let tcp = TcpTransport::accept(&listener).unwrap();
        SecureTransport::accept(tcp, Some("RIGHT1")).err().expect("server must reject wrong PIN")
    });

    let tcp = TcpTransport::connect(address).unwrap();
    let error = SecureTransport::connect(tcp, Some("WRONG1")).err().expect("client must reject proof");
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
                *tampered.last_mut().unwrap() ^= 1;
                return self.inner.send(&tampered);
            }
        }
        self.inner.send(bytes)
    }
    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> { self.inner.recv() }
    fn close(&mut self) -> TransportResult<()> { self.inner.close() }
    fn is_closed(&self) -> bool { self.inner.is_closed() }
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
    let mut secure = SecureTransport::connect(MutatingTransport { inner: tcp, encrypted_sends: 0, replay }, None).unwrap();
    secure.send(b"secret").unwrap();
    server.join().unwrap()
}

#[test]
fn tampered_ciphertext_is_rejected() {
    assert!(rejection_case(false).contains("failed to decrypt payload"));
}

#[test]
fn replayed_ciphertext_is_rejected() {
    assert!(rejection_case(true).contains("replayed frame"));
}
