//! TCP transport implementation.
//!
//! `TcpTransport` preserves the byte-frame boundary expected by the `Transport`
//! trait by prefixing every frame with a four-byte big-endian length.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::transport::{Transport, TransportError, TransportResult};

const LENGTH_PREFIX_LEN: usize = 4;
const DEFAULT_MAX_FRAME_LEN: usize = 64 * 1024 * 1024;
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(1);
/// Bounded connect timeout so an unreachable peer fails fast instead of
/// leaving the UI stuck on the operating system's default timeout.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn is_retryable_io(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted)
}

fn write_all_nonblocking(
    writer: &mut impl Write,
    operation: &'static str,
    bytes: &[u8],
    timeout: Duration,
) -> TransportResult<()> {
    let deadline = Instant::now() + timeout;
    let mut offset = 0;

    while offset < bytes.len() {
        match writer.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(TransportError::from_io(
                    operation,
                    std::io::Error::new(ErrorKind::WriteZero, "socket write returned zero bytes"),
                ));
            }
            Ok(written) => offset += written,
            Err(error) if is_retryable_io(&error) => match error.kind() {
                ErrorKind::Interrupted => continue,
                ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(TransportError::WriteTimedOut { timeout, operation });
                    }
                    std::thread::sleep(IDLE_POLL_INTERVAL);
                }
                _ => unreachable!("retryable I/O classifier returned an unexpected error kind"),
            },
            Err(error) => return Err(TransportError::from_write_io(operation, timeout, error)),
        }
    }

    Ok(())
}

#[derive(Debug)]
pub struct TcpTransport {
    stream: TcpStream,
    max_frame_len: usize,
    /// Bytes read from the socket but not yet consumed as a complete frame.
    /// Persisting this across `recv()` calls is essential: a single frame can
    /// straddle multiple nonblocking polls.
    read_buf: Vec<u8>,
    closed: bool,
}

impl TcpTransport {
    pub fn connect(addr: impl ToSocketAddrs) -> TransportResult<Self> {
        let addrs = addr
            .to_socket_addrs()
            .map_err(|error| TransportError::from_io("resolve address", error))?;
        let mut last_err: Option<std::io::Error> = None;
        for socket_addr in addrs {
            match TcpStream::connect_timeout(&socket_addr, DEFAULT_CONNECT_TIMEOUT) {
                Ok(stream) => return Self::from_stream(stream),
                Err(error) => last_err = Some(error),
            }
        }
        Err(TransportError::from_io(
            "connect",
            last_err.unwrap_or_else(|| {
                std::io::Error::new(ErrorKind::InvalidInput, "no socket addresses to connect to")
            }),
        ))
    }

    pub fn accept(listener: &TcpListener) -> TransportResult<Self> {
        let (stream, _) = listener
            .accept()
            .map_err(|error| TransportError::from_io("accept", error))?;
        Self::from_stream(stream)
    }

    pub fn from_stream(stream: TcpStream) -> TransportResult<Self> {
        stream
            .set_nodelay(true)
            .map_err(|error| TransportError::from_io("configure TCP_NODELAY", error))?;
        stream
            .set_nonblocking(true)
            .map_err(|error| TransportError::from_io("configure nonblocking mode", error))?;
        Ok(Self {
            stream,
            max_frame_len: DEFAULT_MAX_FRAME_LEN,
            read_buf: Vec::new(),
            closed: false,
        })
    }

    pub fn peer_addr(&self) -> TransportResult<SocketAddr> {
        self.stream
            .peer_addr()
            .map_err(|error| TransportError::from_io("get peer address", error))
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

    /// Pulls currently available socket bytes into `read_buf`. `WouldBlock`
    /// means there is no more data to drain during this nonblocking poll.
    fn fill_from_socket(&mut self) -> TransportResult<()> {
        let mut chunk = [0_u8; 64 * 1024];

        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.closed = true;
                    return Err(TransportError::CleanEof { operation: "read" });
                }
                Ok(n) => {
                    self.read_buf.extend_from_slice(&chunk[..n]);
                    if n < chunk.len() {
                        return Ok(());
                    }
                }
                Err(error) if is_retryable_io(&error) => match error.kind() {
                    ErrorKind::Interrupted => continue,
                    ErrorKind::WouldBlock => return Ok(()),
                    _ => unreachable!("retryable I/O classifier returned an unexpected error kind"),
                },
                Err(error) => {
                    self.closed = true;
                    return Err(TransportError::from_io("read", error));
                }
            }
        }
    }

    /// Extracts one complete length-prefixed frame from `read_buf` if fully
    /// buffered, consuming its bytes and leaving any trailing bytes in place.
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

    /// Writes every byte, advancing only after the socket reports `Ok(n)`.
    /// Any failure invalidates the length-prefixed stream permanently.
    fn write_part(&mut self, operation: &'static str, bytes: &[u8]) -> TransportResult<()> {
        if let Err(error) =
            write_all_nonblocking(&mut self.stream, operation, bytes, DEFAULT_WRITE_TIMEOUT)
        {
            self.closed = true;
            return Err(error);
        }
        Ok(())
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

        self.write_part("write prefix", &(bytes.len() as u32).to_be_bytes())?;
        self.write_part("write payload", bytes)?;
        if let Err(error) = self.stream.flush() {
            self.closed = true;
            return Err(TransportError::from_write_io(
                "flush",
                DEFAULT_WRITE_TIMEOUT,
                error,
            ));
        }
        Ok(())
    }

    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        if self.closed {
            // A final frame can arrive in the same segment as FIN.
            return self.take_buffered_frame();
        }

        let fill_result = self.fill_from_socket();
        if let Some(frame) = self.take_buffered_frame()? {
            return Ok(Some(frame));
        }
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
            Err(error) => Err(TransportError::from_io("shutdown", error)),
        }
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
mod tests {
    use super::{is_retryable_io, write_all_nonblocking};
    use std::collections::VecDeque;
    use std::io::{self, ErrorKind, Write};
    use std::time::Duration;

    use crate::transport::TransportError;

    struct ScriptedWriter {
        script: VecDeque<io::Result<usize>>,
        output: Vec<u8>,
        attempts: Vec<Vec<u8>>,
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.attempts.push(bytes.to_vec());
            let result = self.script.pop_front().expect("script exhausted");
            match result {
                Ok(written) => {
                    self.output.extend_from_slice(&bytes[..written]);
                    Ok(written)
                }
                Err(error) => Err(error),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn windows_io_pending_is_not_retryable() {
        let error = std::io::Error::from_raw_os_error(997);
        assert_eq!(error.raw_os_error(), Some(997));
        assert!(!is_retryable_io(&error));
    }

    #[test]
    fn scripted_partial_writes_preserve_exact_byte_stream() {
        let mut writer = ScriptedWriter {
            script: VecDeque::from([
                Ok(2),
                Err(io::Error::from(io::ErrorKind::WouldBlock)),
                Err(io::Error::from(io::ErrorKind::Interrupted)),
                Ok(3),
            ]),
            output: Vec::new(),
            attempts: Vec::new(),
        };

        write_all_nonblocking(&mut writer, "test write", b"abcde", Duration::from_secs(1))
            .expect("scripted write should complete");
        assert_eq!(writer.output, b"abcde");
        assert_eq!(
            writer.attempts,
            [
                b"abcde".to_vec(),
                b"cde".to_vec(),
                b"cde".to_vec(),
                b"cde".to_vec()
            ]
        );
    }

    #[test]
    fn scripted_zero_write_is_fatal() {
        let mut writer = ScriptedWriter {
            script: VecDeque::from([Ok(0)]),
            output: Vec::new(),
            attempts: Vec::new(),
        };

        let error =
            write_all_nonblocking(&mut writer, "test write", b"data", Duration::from_secs(1))
                .expect_err("zero write must fail");
        assert!(matches!(
            error,
            TransportError::Io {
                kind: ErrorKind::WriteZero,
                ..
            }
        ));
    }

    #[test]
    fn scripted_windows_997_error_is_not_retried() {
        let mut writer = ScriptedWriter {
            script: VecDeque::from([Err(io::Error::from_raw_os_error(997))]),
            output: Vec::new(),
            attempts: Vec::new(),
        };

        let error =
            write_all_nonblocking(&mut writer, "test write", b"data", Duration::from_secs(1))
                .expect_err("error 997 must fail");
        assert!(matches!(error, TransportError::Io { .. }));
        assert!(error.to_string().contains("997"));
    }

    #[test]
    fn scripted_would_block_obeys_deadline() {
        let mut writer = ScriptedWriter {
            script: VecDeque::from([Err(io::Error::from(io::ErrorKind::WouldBlock))]),
            output: Vec::new(),
            attempts: Vec::new(),
        };

        let error = write_all_nonblocking(&mut writer, "test write", b"data", Duration::ZERO)
            .expect_err("expired deadline must fail");
        assert_eq!(
            error,
            TransportError::WriteTimedOut {
                timeout: Duration::ZERO,
                operation: "test write",
            }
        );
    }

    #[test]
    fn scripted_permanent_error_is_not_retried() {
        let mut writer = ScriptedWriter {
            script: VecDeque::from([Err(io::Error::from(io::ErrorKind::BrokenPipe))]),
            output: Vec::new(),
            attempts: Vec::new(),
        };

        let error =
            write_all_nonblocking(&mut writer, "test write", b"data", Duration::from_secs(1))
                .expect_err("permanent error must fail");
        assert_eq!(
            error,
            TransportError::BrokenPipe {
                operation: "test write"
            }
        );
    }
}
