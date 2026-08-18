// Authenticated encrypted transport used by LAN transfers.

use std::thread;
use std::time::{Duration, Instant};

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::security::{EncryptedFrame, SessionCipher};
use crate::transport::{Transport, TransportError, TransportResult};

const MAGIC: &[u8; 8] = b"PLNSEC01";
const SECURE_PROTOCOL_VERSION: u8 = 2;
const ENVELOPE_VERSION: u8 = 2;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const ENVELOPE_HEADER_LEN: usize = 1 + 8 + NONCE_LEN;
const MIN_ENVELOPE_LEN: usize = ENVELOPE_HEADER_LEN + TAG_LEN;
const HELLO_LEN: usize = 8 + 1 + 32 + 32 + 1;
const SERVER_HELLO_LEN: usize = HELLO_LEN + 32;
const PROOF_LEN: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const AAD_CLIENT_TO_SERVER: &[u8] = b"plenum-lan-v2:client-to-server";
const AAD_SERVER_TO_CLIENT: &[u8] = b"plenum-lan-v2:server-to-client";
const DIRECTION_CLIENT_TO_SERVER: &str = "client-to-server";
const DIRECTION_SERVER_TO_CLIENT: &str = "server-to-client";

type HmacSha256 = Hmac<Sha256>;

pub struct SecureTransport<T: Transport> {
    inner: T,
    sender: SessionCipher,
    receiver: SessionCipher,
    send_counter: u64,
    expected_recv_counter: u64,
    send_aad: &'static [u8],
    recv_aad: &'static [u8],
    send_direction: &'static str,
    recv_direction: &'static str,
}

impl<T: Transport> SecureTransport<T> {
    pub fn connect(inner: T, pairing_secret: Option<&str>) -> TransportResult<Self> {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        let mut client_nonce = [0_u8; 32];
        OsRng.fill_bytes(&mut client_nonce);

        let mut hello = Vec::with_capacity(HELLO_LEN);
        hello.extend_from_slice(MAGIC);
        hello.push(SECURE_PROTOCOL_VERSION);
        hello.extend_from_slice(public.as_bytes());
        hello.extend_from_slice(&client_nonce);
        hello.push(u8::from(pairing_secret.is_some()));

        let mut inner = inner;
        inner.send(&hello)?;
        let server_hello = recv_handshake(&mut inner)?;
        if server_hello.len() != SERVER_HELLO_LEN
            || &server_hello[..8] != MAGIC
            || server_hello[8] != SECURE_PROTOCOL_VERSION
        {
            return Err(handshake_error("incompatible or malformed secure LAN peer"));
        }

        let server_requires_pin = server_hello[73] == 1;
        if server_requires_pin && pairing_secret.is_none() {
            return Err(handshake_error("receiver requires a pairing PIN"));
        }
        let proof_secret = server_requires_pin.then_some(pairing_secret).flatten();

        let transcript = transcript(&hello, &server_hello[..HELLO_LEN]);
        verify_proof(
            proof_secret,
            b"server",
            &transcript,
            &server_hello[HELLO_LEN..],
        )?;
        inner.send(&make_proof(proof_secret, b"client", &transcript)?)?;

        let server_public = public_key(&server_hello[9..41])?;
        let shared = secret.diffie_hellman(&server_public);
        Self::from_shared(inner, shared.as_bytes(), &transcript, true)
    }

    pub fn accept(inner: T, pairing_secret: Option<&str>) -> TransportResult<Self> {
        let mut inner = inner;
        let client_hello = recv_handshake(&mut inner)?;
        if client_hello.len() != HELLO_LEN
            || &client_hello[..8] != MAGIC
            || client_hello[8] != SECURE_PROTOCOL_VERSION
        {
            return Err(handshake_error("incompatible or malformed secure LAN peer"));
        }
        if pairing_secret.is_some() && client_hello[73] != 1 {
            return Err(handshake_error("pairing PIN required"));
        }

        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        let mut server_nonce = [0_u8; 32];
        OsRng.fill_bytes(&mut server_nonce);

        let mut server_prefix = Vec::with_capacity(HELLO_LEN);
        server_prefix.extend_from_slice(MAGIC);
        server_prefix.push(SECURE_PROTOCOL_VERSION);
        server_prefix.extend_from_slice(public.as_bytes());
        server_prefix.extend_from_slice(&server_nonce);
        server_prefix.push(u8::from(pairing_secret.is_some()));
        let transcript = transcript(&client_hello, &server_prefix);

        let mut response = server_prefix;
        response.extend_from_slice(&make_proof(pairing_secret, b"server", &transcript)?);
        inner.send(&response)?;
        let client_proof = recv_handshake(&mut inner)?;
        if client_proof.len() != PROOF_LEN {
            return Err(handshake_error("malformed client handshake proof"));
        }
        verify_proof(pairing_secret, b"client", &transcript, &client_proof)?;

        let client_public = public_key(&client_hello[9..41])?;
        let shared = secret.diffie_hellman(&client_public);
        Self::from_shared(inner, shared.as_bytes(), &transcript, false)
    }

    fn from_shared(
        inner: T,
        shared: &[u8; 32],
        transcript: &[u8; 32],
        client: bool,
    ) -> TransportResult<Self> {
        let hkdf = Hkdf::<Sha256>::new(Some(transcript), shared);
        let mut client_key = [0_u8; 32];
        let mut server_key = [0_u8; 32];
        hkdf.expand(AAD_CLIENT_TO_SERVER, &mut client_key)
            .map_err(|_| handshake_error("could not derive client encryption key"))?;
        hkdf.expand(AAD_SERVER_TO_CLIENT, &mut server_key)
            .map_err(|_| handshake_error("could not derive server encryption key"))?;

        let (send_key, recv_key, send_aad, recv_aad, send_direction, recv_direction) = if client {
            (
                client_key,
                server_key,
                AAD_CLIENT_TO_SERVER,
                AAD_SERVER_TO_CLIENT,
                DIRECTION_CLIENT_TO_SERVER,
                DIRECTION_SERVER_TO_CLIENT,
            )
        } else {
            (
                server_key,
                client_key,
                AAD_SERVER_TO_CLIENT,
                AAD_CLIENT_TO_SERVER,
                DIRECTION_SERVER_TO_CLIENT,
                DIRECTION_CLIENT_TO_SERVER,
            )
        };
        Ok(Self {
            inner,
            sender: SessionCipher::new(&send_key).map_err(security_error)?,
            receiver: SessionCipher::new(&recv_key).map_err(security_error)?,
            send_counter: 0,
            expected_recv_counter: 0,
            send_aad,
            recv_aad,
            send_direction,
            recv_direction,
        })
    }
}

impl<T: Transport> Transport for SecureTransport<T> {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        let counter = self.send_counter;
        let Some(next_counter) = counter.checked_add(1) else {
            let _ = self.inner.close();
            return Err(secure_frame_error(format!(
                "secure frame counter exhausted: direction={} counter={counter}",
                self.send_direction
            )));
        };
        let aad = build_frame_aad(self.send_aad, counter);
        let frame = self.sender.encrypt(bytes, &aad).map_err(|error| {
            secure_frame_error(format!(
                "failed to encrypt secure frame: direction={} counter={} plaintext_len={} error={error}",
                self.send_direction,
                counter,
                bytes.len()
            ))
        })?;
        let envelope = encode_envelope(counter, &frame)?;
        self.inner.send(&envelope)?;
        self.send_counter = next_counter;
        Ok(())
    }

    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        let Some(bytes) = self.inner.recv()? else {
            return Ok(None);
        };
        let envelope = decode_envelope(&bytes)?;
        if envelope.counter != self.expected_recv_counter {
            return Err(secure_frame_error(format!(
                "unexpected encrypted frame counter: direction={} expected_counter={} received_counter={} envelope_len={} ciphertext_len={}",
                self.recv_direction,
                self.expected_recv_counter,
                envelope.counter,
                bytes.len(),
                envelope.frame.ciphertext.len()
            )));
        }

        let aad = build_frame_aad(self.recv_aad, envelope.counter);
        let plaintext = self
            .receiver
            .decrypt(&envelope.frame, &aad)
            .map_err(|error| {
                secure_frame_error(format!(
                    "failed to authenticate encrypted frame: direction={} counter={} expected_counter={} envelope_len={} ciphertext_len={} error={error}",
                    self.recv_direction,
                    envelope.counter,
                    self.expected_recv_counter,
                    bytes.len(),
                    envelope.frame.ciphertext.len()
                ))
            })?;
        let Some(next_counter) = self.expected_recv_counter.checked_add(1) else {
            let _ = self.inner.close();
            return Err(secure_frame_error(format!(
                "secure frame counter exhausted: direction={} counter={}",
                self.recv_direction, self.expected_recv_counter
            )));
        };
        self.expected_recv_counter = next_counter;
        Ok(Some(plaintext))
    }

    fn close(&mut self) -> TransportResult<()> {
        self.inner.close()
    }
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
    fn poll_diagnostics(&mut self) -> Vec<String> {
        self.inner.poll_diagnostics()
    }
    fn is_relayed(&self) -> Option<bool> {
        self.inner.is_relayed()
    }
}

struct DecodedEnvelope {
    counter: u64,
    frame: EncryptedFrame,
}

fn build_frame_aad(direction_aad: &[u8], counter: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(direction_aad.len() + 1 + 8);
    aad.extend_from_slice(direction_aad);
    aad.push(ENVELOPE_VERSION);
    aad.extend_from_slice(&counter.to_be_bytes());
    aad
}

fn encode_envelope(counter: u64, frame: &EncryptedFrame) -> TransportResult<Vec<u8>> {
    let nonce = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        frame.nonce.as_bytes(),
    )
    .map_err(|_| secure_frame_error("invalid encrypted-frame nonce"))?;
    if nonce.len() != NONCE_LEN {
        return Err(secure_frame_error("invalid encrypted-frame nonce"));
    }
    let mut bytes = Vec::with_capacity(ENVELOPE_HEADER_LEN + frame.ciphertext.len());
    bytes.push(ENVELOPE_VERSION);
    bytes.extend_from_slice(&counter.to_be_bytes());
    bytes.extend_from_slice(&nonce);
    bytes.extend_from_slice(&frame.ciphertext);
    Ok(bytes)
}

fn decode_envelope(bytes: &[u8]) -> TransportResult<DecodedEnvelope> {
    if bytes.len() < MIN_ENVELOPE_LEN {
        return Err(secure_frame_error(format!(
            "truncated encrypted frame: envelope_len={} minimum_len={MIN_ENVELOPE_LEN}",
            bytes.len()
        )));
    }
    if bytes[0] != ENVELOPE_VERSION {
        return Err(secure_frame_error(format!(
            "unsupported encrypted envelope version: expected={} received={} envelope_len={}",
            ENVELOPE_VERSION,
            bytes[0],
            bytes.len()
        )));
    }
    let counter = u64::from_be_bytes(
        bytes[1..9]
            .try_into()
            .map_err(|_| secure_frame_error("malformed encrypted frame counter"))?,
    );
    Ok(DecodedEnvelope {
        counter,
        frame: EncryptedFrame {
            nonce: base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                &bytes[9..ENVELOPE_HEADER_LEN],
            ),
            ciphertext: bytes[ENVELOPE_HEADER_LEN..].to_vec(),
        },
    })
}

fn recv_handshake<T: Transport>(transport: &mut T) -> TransportResult<Vec<u8>> {
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        match transport.recv()? {
            Some(frame) => return Ok(frame),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            None => return Err(handshake_error("secure LAN handshake timed out")),
        }
    }
}

fn transcript(client: &[u8], server: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"plenum-secure-lan-transcript-v2");
    hasher.update(client);
    hasher.update(server);
    hasher.finalize().into()
}

fn make_proof(
    secret: Option<&str>,
    role: &[u8],
    transcript: &[u8; 32],
) -> TransportResult<[u8; 32]> {
    let Some(secret) = secret else {
        return Ok([0_u8; 32]);
    };
    let mut mac = HmacSha256::new_from_slice(secret.trim().to_ascii_uppercase().as_bytes())
        .map_err(|_| handshake_error("invalid pairing secret"))?;
    mac.update(b"plenum-secure-lan-proof-v2");
    mac.update(role);
    mac.update(transcript);
    Ok(mac.finalize().into_bytes().into())
}

fn verify_proof(
    secret: Option<&str>,
    role: &[u8],
    transcript: &[u8; 32],
    proof: &[u8],
) -> TransportResult<()> {
    let expected = make_proof(secret, role, transcript)?;
    if proof != expected {
        return Err(handshake_error("pairing authentication failed"));
    }
    Ok(())
}

fn public_key(bytes: &[u8]) -> TransportResult<PublicKey> {
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| handshake_error("invalid public key"))?;
    Ok(PublicKey::from(array))
}

fn security_error(error: crate::security::SecurityError) -> TransportError {
    secure_frame_error(format!("secure LAN transport error: {error}"))
}

fn handshake_error(message: impl Into<String>) -> TransportError {
    TransportError::Io {
        operation: "secure handshake",
        kind: std::io::ErrorKind::InvalidData,
        message: message.into(),
    }
}

fn secure_frame_error(message: impl Into<String>) -> TransportError {
    TransportError::Io {
        operation: "secure frame",
        kind: std::io::ErrorKind::InvalidData,
        message: message.into(),
    }
}
