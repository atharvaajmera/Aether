//! High-level app integration errors.

use std::fmt;

use crate::app::types::PermissionKind;
use crate::discovery::DiscoveryError;
use crate::flow::FlowError;
use crate::protocol::ProtocolError;
use crate::rtc::RtcError;
use crate::security::SecurityError;
use crate::signaling::SignalingError;
use crate::stream::StreamError;
use crate::transport::TransportError;

#[derive(Debug)]
pub enum AppError {
    PermissionDenied {
        permission: PermissionKind,
        operation: &'static str,
    },
    InvalidRequest(String),
    /// The session was cancelled locally via `SessionControl::cancel`.
    Cancelled,
    /// The transfer was refused by the peer or the local user. The message is
    /// already user-presentable ("the receiver declined the transfer", ...).
    Rejected(String),
    /// The transfer made no observable progress for longer than the watchdog
    /// allows (e.g. the peer went half-open and packets stopped flowing).
    Stalled(String),
    Discovery(DiscoveryError),
    Flow(FlowError),
    Protocol(ProtocolError),
    Rtc(RtcError),
    Security(SecurityError),
    Signaling(SignalingError),
    Stream(StreamError),
    Transport(TransportError),
    Io(std::io::Error),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionDenied {
                permission,
                operation,
            } => write!(f, "permission {permission:?} is required for {operation}"),
            Self::InvalidRequest(message) => write!(f, "invalid request: {message}"),
            Self::Cancelled => write!(f, "transfer cancelled"),
            Self::Rejected(message) => write!(f, "{message}"),
            Self::Stalled(message) => write!(f, "transfer stalled: {message}"),
            Self::Discovery(error) => write!(f, "discovery error: {error}"),
            Self::Flow(error) => write!(f, "flow error: {error}"),
            Self::Protocol(error) => write!(f, "protocol error: {error}"),
            Self::Rtc(error) => write!(f, "rtc error: {error}"),
            Self::Security(error) => write!(f, "security error: {error}"),
            Self::Signaling(error) => write!(f, "signaling error: {error}"),
            Self::Stream(error) => write!(f, "stream error: {error}"),
            Self::Transport(error) => write!(f, "transport error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Discovery(error) => Some(error),
            Self::Flow(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Rtc(error) => Some(error),
            Self::Security(error) => Some(error),
            Self::Signaling(error) => Some(error),
            Self::Stream(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DiscoveryError> for AppError {
    fn from(error: DiscoveryError) -> Self {
        Self::Discovery(error)
    }
}

impl From<FlowError> for AppError {
    fn from(error: FlowError) -> Self {
        Self::Flow(error)
    }
}

impl From<ProtocolError> for AppError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<RtcError> for AppError {
    fn from(error: RtcError) -> Self {
        Self::Rtc(error)
    }
}

impl From<SecurityError> for AppError {
    fn from(error: SecurityError) -> Self {
        Self::Security(error)
    }
}

impl From<SignalingError> for AppError {
    fn from(error: SignalingError) -> Self {
        Self::Signaling(error)
    }
}

impl From<StreamError> for AppError {
    fn from(error: StreamError) -> Self {
        Self::Stream(error)
    }
}

impl From<TransportError> for AppError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
