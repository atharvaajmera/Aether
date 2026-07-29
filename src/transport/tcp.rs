//! TCP transport implementation.
//!
//! `TcpTransport` preserves the byte-frame boundary expected by the `Transport`
//! trait by prefixing every frame with a four-byte big-endian length.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::transport::{Transport, TransportError, TransportResult};

const LENGTH_PREFIX_LEN: usize = 4;
const DEFAULT_MAX_FRAME_LEN: usize = 64 * 1024 * 1024;
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(5);
/// Bounded connect timeout so an unreachable peer (e.g. WiFi client/AP
/// isolation, or a receiver that never bound its listener) fails fast with a
/// clear error instead of leaving the UI stuck on "Connecting" for the OS
/// default (which can be ~1-2 minutes).
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct TcpTransport {
    stream: TcpStream,
    max_frame_len: usize,
    /// Bytes read from the socket but not yet consumed as a complete frame.
    /// Persisting this across `recv()` calls is essential: a single frame's
    /// bytes can straddle multiple read timeouts, and dropping the partial
    /// bytes would desync the length-prefixed framing.
    read_buf: Vec<u8>,
    closed: bool,
}

impl TcpTransport {
    pub fn connect(addr: impl ToSocketAddrs) -> TransportResult<Self> {
        // Resolve to concrete socket addresses so we can apply an explicit
        // connect timeout (`TcpStream::connect` itself has no timeout knob).
        let addrs = addr.to_socket_addrs()?;
        let mut last_err: Option<std::io::Error> = None;
        for socket_addr in addrs {
            match TcpStream::connect_timeout(&socket_addr, DEFAULT_CONNECT_TIMEOUT) {
                Ok(stream) => return Self::from_stream(stream),
                Err(error) => last_err = Some(error),
            }
        }
        Err(last_err
            .unwrap_or_else(|| {
                std::io::Error::new(ErrorKind::InvalidInput, "no socket addresses to connect to")
            })
            .into())
    }

    pub fn accept(listener: &TcpListener) -> TransportResult<Self> {
        let (stream, _) = listener.accept()?;
        Self::from_stream(stream)
    }

    pub fn from_stream(stream: TcpStream) -> TransportResult<Self> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(DEFAULT_READ_TIMEOUT))?;
        Ok(Self {
            stream,
            max_frame_len: DEFAULT_MAX_FRAME_LEN,
            read_buf: Vec::new(),
            closed: false,
        })
    }

    pub fn peer_addr(&self) -> TransportResult<SocketAddr> {
        Ok(self.stream.peer_addr()?)
    }

    pub fn max_frame_len(&self) -> usize {
        self.max_frame_len
    }

    pub fn set_max_frame_len(&mut self, max_frame_len: usize) {
        self.max_frame_len = max_frame_len;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Pulls whatever bytes are currently available on the socket into
    /// `read_buf`, without blocking beyond the socket's read timeout. Returns
    /// `Err(Closed)` on clean EOF. A timeout/would-block is not an error — it
    /// just means "no new bytes right now", so we return `Ok(())`.
    fn fill_from_socket(&mut self) -> TransportResult<()> {
        let mut chunk = [0_u8; 64 * 1024];
        match self.stream.read(&mut chunk) {
            Ok(0) => {
                // Clean EOF: peer closed the connection.
                self.closed = true;
                Err(TransportError::Closed)
            }
            Ok(n) => {
                self.read_buf.extend_from_slice(&chunk[..n]);
                Ok(())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                Ok(())
            }
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                self.closed = true;
                Err(TransportError::Closed)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Extracts one complete length-prefixed frame from `read_buf` if fully
    /// buffered, consuming its bytes and leaving any trailing bytes (the start
    /// of the next frame) in place. Returns `Ok(None)` if a full frame isn't
    /// available yet.
    fn take_buffered_frame(&mut self) -> TransportResult<Option<Vec<u8>>> {
        if self.read_buf.len() < LENGTH_PREFIX_LEN {
            return Ok(None);
        }

        let mut length_prefix = [0_u8; LENGTH_PREFIX_LEN];
        length_prefix.copy_from_slice(&self.read_buf[..LENGTH_PREFIX_LEN]);
        let frame_len = u32::from_be_bytes(length_prefix) as usize;

        if frame_len > self.max_frame_len {
            return Err(TransportError::FrameTooLarge {
                len: frame_len,
                max: self.max_frame_len,
            });
        }

        if self.read_buf.len() < LENGTH_PREFIX_LEN + frame_len {
            return Ok(None);
        }

        let frame = self.read_buf[LENGTH_PREFIX_LEN..LENGTH_PREFIX_LEN + frame_len].to_vec();
        self.read_buf.drain(..LENGTH_PREFIX_LEN + frame_len);
        Ok(Some(frame))
    }
}

impl Transport for TcpTransport {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        if self.closed {
            return Err(TransportError::Closed);
        }

        if bytes.len() > self.max_frame_len || bytes.len() > u32::MAX as usize {
            return Err(TransportError::FrameTooLarge {
                len: bytes.len(),
                max: self.max_frame_len.min(u32::MAX as usize),
            });
        }

        let len = bytes.len() as u32;
        self.stream.write_all(&len.to_be_bytes())?;
        self.stream.write_all(bytes)?;
        self.stream.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        if self.closed {
            // Even after close, serve any complete frame still buffered (the
            // peer's final segment may have carried the last frame together
            // with EOF).
            return Ok(self.take_buffered_frame()?);
        }

        // Try to pull any newly-available bytes into the accumulation buffer.
        // Defer a clean EOF until after we've drained any complete frame(s)
        // already buffered, so a final frame arriving alongside FIN isn't lost.
        let fill_result = self.fill_from_socket();

        if let Some(frame) = self.take_buffered_frame()? {
            return Ok(Some(frame));
        }

        // No complete frame buffered; now surface a close/error if the fill saw one.
        fill_result?;
        Ok(None)
    }

    fn close(&mut self) -> TransportResult<()> {
        self.closed = true;
        match self.stream.shutdown(std::net::Shutdown::Both) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotConnected
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::BrokenPipe
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}
