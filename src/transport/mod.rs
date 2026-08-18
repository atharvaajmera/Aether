//! Transport abstraction and implementations.

pub mod error;
pub mod memory;
pub mod multipath;
pub mod secure;
pub mod tcp;

pub use error::TransportError;
pub use memory::{MemoryTransport, MemoryTransportConfig};
pub use multipath::MultipathTransport;
pub use secure::SecureTransport;
pub use tcp::TcpTransport;

pub type TransportResult<T> = Result<T, TransportError>;

/// Minimal byte transport abstraction used by higher-level transfer logic.
pub trait Transport {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()>;
    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>>;
    fn close(&mut self) -> TransportResult<()>;
    fn is_closed(&self) -> bool;

    /// Drains any out-of-band diagnostic messages the transport has queued
    /// since the last call (e.g. ICE connection state changes, candidate-pair
    /// stats). Most transports have nothing to report; `RtcTransport` is the
    /// only implementor that overrides this.
    fn poll_diagnostics(&mut self) -> Vec<String> {
        Vec::new()
    }

    fn is_relayed(&self) -> Option<bool> {
        None
    }
}
