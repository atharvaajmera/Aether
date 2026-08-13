// Authenticated encrypted transport used by LAN transfers.

use std::thread;
use std::time::{Duration, Instant};

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::security::{EncryptedFrame, SessionCipher};
use crate::transport::{Transport, TransportError, TransportResult};

const MAGIC: &[u8; 8] = b"PLNSEC01";
const VERSION: u8 = 1;
const HELLO_LEN: usize = 8 + 1 + 32 + 32 + 1;
const SERVER_HELLO_LEN: usize = HELLO_LEN + 32;
const PROOF_LEN: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const AAD_CLIENT_TO_SERVER: &[u8] = b"plenum-lan-v1:client-to-server";
const AAD_SERVER_TO_CLIENT: &[u8] = b"plenum-lan-v1:server-to-client";

type HmacSha256 = Hmac<Sha256>;

pub struct SecureTransport<T: Transport> {
    inner: T,
    sender: SessionCipher,
    receiver: SessionCipher,
    send_aad: &'static [u8],
    recv_aad: &'static [u8],
}

impl<T: Transport> SecureTransport<T> {
    pub fn connect(inner: T, pairing_secret: Option<&str>) -> TransportResult<Self> {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        let mut client_nonce = [0_u8; 32];
        OsRng.fill_bytes(&mut client_nonce);

        let mut hello = Vec::with_capacity(HELLO_LEN);
        hello.extend_from_slice(MAGIC);
        hello.push(VERSION);
        hello.extend_from_slice(public.as_bytes());
        hello.extend_from_slice(&client_nonce);
        hello.push(u8::from(pairing_secret.is_some()));

        let mut inner = inner;
        inner.send(&hello)?;
        let server_hello = recv_handshake(&mut inner)?;
        if server_hello.len() != SERVER_HELLO_LEN
            || &server_hello[..8] != MAGIC
            || server_hello[8] != VERSION
        {
            return Err(handshake_error("incompatible or malformed secure LAN peer"));
        }

        let server_requires_pin = server_hello[73] == 1;
        if server_requires_pin && pairing_secret.is_none() {
            return Err(handshake_error("receiver requires a pairing PIN"));
        }
        let proof_secret = if server_requires_pin { pairing_secret } else { None };

        let transcript = transcript(&hello, &server_hello[..HELLO_LEN]);
        let server_proof = &server_hello[HELLO_LEN..];
        verify_proof(proof_secret, b"server", &transcript, server_proof)?;
        inner.send(&make_proof(proof_secret, b"client", &transcript)?)?;

        let server_public = public_key(&server_hello[9..41])?;
        let shared = secret.diffie_hellman(&server_public);
        Self::from_shared(
            inner,
            shared.as_bytes(),
            &transcript,
            true,
        )
    }

    pub fn accept(inner: T, pairing_secret: Option<&str>) -> TransportResult<Self> {
        let mut inner = inner;
        let client_hello = recv_handshake(&mut inner)?;
        if client_hello.len() != HELLO_LEN
            || &client_hello[..8] != MAGIC
            || client_hello[8] != VERSION
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
        server_prefix.push(VERSION);
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
        Self::from_shared(
            inner,
            shared.as_bytes(),
            &transcript,
            false,
        )
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

        let (send_key, recv_key, send_aad, recv_aad) = if client {
            (client_key, server_key, AAD_CLIENT_TO_SERVER, AAD_SERVER_TO_CLIENT)
        } else {
            (server_key, client_key, AAD_SERVER_TO_CLIENT, AAD_CLIENT_TO_SERVER)
        };
        Ok(Self {
            inner,
            sender: SessionCipher::new(&send_key).map_err(security_error)?,
            receiver: SessionCipher::new(&recv_key).map_err(security_error)?,
            send_aad,
            recv_aad,
        })
    }
}

impl<T: Transport> Transport for SecureTransport<T> {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        let frame = self.sender.encrypt(bytes, self.send_aad).map_err(security_error)?;
        self.inner.send(&encode_envelope(&frame)?)
    }

    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        let Some(bytes) = self.inner.recv()? else { return Ok(None) };
        let frame = decode_envelope(&bytes)?;
        self.receiver
            .decrypt(&frame, self.recv_aad)
            .map(Some)
            .map_err(security_error)
    }

    fn close(&mut self) -> TransportResult<()> { self.inner.close() }
    fn is_closed(&self) -> bool { self.inner.is_closed() }
    fn poll_diagnostics(&mut self) -> Vec<String> { self.inner.poll_diagnostics() }
    fn is_relayed(&self) -> Option<bool> { self.inner.is_relayed() }
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
    hasher.update(b"plenum-secure-lan-transcript-v1");
    hasher.update(client);
    hasher.update(server);
    hasher.finalize().into()
}

fn make_proof(secret: Option<&str>, role: &[u8], transcript: &[u8; 32]) -> TransportResult<[u8; 32]> {
    let Some(secret) = secret else { return Ok([0_u8; 32]) };
    let mut mac = HmacSha256::new_from_slice(secret.trim().to_ascii_uppercase().as_bytes())
        .map_err(|_| handshake_error("invalid pairing secret"))?;
    mac.update(b"plenum-secure-lan-proof-v1");
    mac.update(role);
    mac.update(transcript);
    Ok(mac.finalize().into_bytes().into())
}

fn verify_proof(secret: Option<&str>, role: &[u8], transcript: &[u8; 32], proof: &[u8]) -> TransportResult<()> {
    let expected = make_proof(secret, role, transcript)?;
    if proof != expected {
        return Err(handshake_error("pairing authentication failed"));
    }
    Ok(())
}

fn public_key(bytes: &[u8]) -> TransportResult<PublicKey> {
    let array: [u8; 32] = bytes.try_into().map_err(|_| handshake_error("invalid public key"))?;
    Ok(PublicKey::from(array))
}

fn encode_envelope(frame: &EncryptedFrame) -> TransportResult<Vec<u8>> {
    let nonce = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, frame.nonce.as_bytes())
        .map_err(|_| handshake_error("invalid encrypted-frame nonce"))?;
    if nonce.len() != 12 { return Err(handshake_error("invalid encrypted-frame nonce")); }
    let mut bytes = Vec::with_capacity(12 + frame.ciphertext.len());
    bytes.extend_from_slice(&nonce);
    bytes.extend_from_slice(&frame.ciphertext);
    Ok(bytes)
}

fn decode_envelope(bytes: &[u8]) -> TransportResult<EncryptedFrame> {
    if bytes.len() < 28 { return Err(handshake_error("truncated encrypted LAN frame")); }
    Ok(EncryptedFrame {
        nonce: base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &bytes[..12]),
        ciphertext: bytes[12..].to_vec(),
    })
}

fn security_error(error: crate::security::SecurityError) -> TransportError {
    handshake_error(&format!("secure LAN transport error: {error}"))
}

fn handshake_error(message: &str) -> TransportError {
    TransportError::Io { message: message.to_string() }
}
