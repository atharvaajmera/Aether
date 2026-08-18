//! Transport-level errors.

use std::fmt;
use std::io::ErrorKind;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    Closed,
    CleanEof {
        operation: &'static str,
    },
    ConnectionReset {
        operation: &'static str,
    },
    BrokenPipe {
        operation: &'static str,
    },
    WriteTimedOut {
        timeout: Duration,
        operation: &'static str,
    },
    BufferFull {
        capacity: usize,
        requested: usize,
    },
    FrameTooLarge {
        len: usize,
        max: usize,
    },
    Io {
        operation: &'static str,
        kind: ErrorKind,
        message: String,
    },
    DeadPath,
}

impl TransportError {
    pub fn from_io(operation: &'static str, error: std::io::Error) -> Self {
        match error.kind() {
            ErrorKind::UnexpectedEof => Self::CleanEof { operation },
            ErrorKind::ConnectionReset => Self::ConnectionReset { operation },
            ErrorKind::BrokenPipe => Self::BrokenPipe { operation },
            kind => Self::Io {
                operation,
                kind,
                message: error.to_string(),
            },
        }
    }

    pub fn from_write_io(
        operation: &'static str,
        timeout: Duration,
        error: std::io::Error,
    ) -> Self {
        match error.kind() {
            ErrorKind::TimedOut | ErrorKind::WouldBlock => {
                Self::WriteTimedOut { timeout, operation }
            }
            _ => Self::from_io(operation, error),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "transport is closed"),
            Self::CleanEof { operation } => {
                write!(f, "transport reached clean EOF during {operation}")
            }
            Self::ConnectionReset { operation } => write!(f, "connection reset during {operation}"),
            Self::BrokenPipe { operation } => write!(f, "broken pipe during {operation}"),
            Self::WriteTimedOut { timeout, operation } => write!(
                f,
                "write timed out during {operation} after {:.3}s",
                timeout.as_secs_f64()
            ),
            Self::BufferFull {
                capacity,
                requested,
            } => write!(
                f,
                "transport buffer is full: capacity {capacity}, requested {requested}"
            ),
            Self::FrameTooLarge { len, max } => {
                write!(f, "transport frame is too large: {len} bytes, max {max}")
            }
            Self::Io {
                operation,
                kind,
                message,
            } => write!(
                f,
                "transport I/O error during {operation} ({kind:?}): {message}"
            ),
            Self::DeadPath => write!(
                f,
                "network path is dead: data submitted but nothing ever received back"
            ),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> Self {
        Self::from_io("I/O operation", error)
    }
}
