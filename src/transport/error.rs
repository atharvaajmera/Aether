//! Transport-level errors.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    Closed,
    BufferFull { capacity: usize, requested: usize },
    FrameTooLarge { len: usize, max: usize },
    Io { message: String },
    DeadPath,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "transport is closed"),
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
            Self::Io { message } => write!(f, "transport I/O error: {message}"),
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
        Self::Io {
            message: error.to_string(),
        }
    }
}
