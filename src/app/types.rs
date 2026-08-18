//! Stable app-facing request, result, and event types.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptDecision {
    Pending,
    Accepted,
    Declined,
}

#[derive(Debug, Clone, Default)]
pub struct SessionControl {
    cancelled: Arc<AtomicBool>,
    decision: Arc<AtomicU8>,
}

impl SessionControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn accept(&self) {
        self.decision.store(1, Ordering::Relaxed);
    }

    pub fn decline(&self) {
        self.decision.store(2, Ordering::Relaxed);
    }

    pub fn decision(&self) -> AcceptDecision {
        match self.decision.load(Ordering::Relaxed) {
            1 => AcceptDecision::Accepted,
            2 => AcceptDecision::Declined,
            _ => AcceptDecision::Pending,
        }
    }

    pub fn reset_decision(&self) {
        self.decision.store(0, Ordering::Relaxed);
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionKind {
    LocalNetwork,
    FileSystemRead,
    FileSystemWrite,
    BackgroundTransfer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorePermissions {
    pub local_network: bool,
    pub file_system_read: bool,
    pub file_system_write: bool,
    pub background_transfer: bool,
}

impl CorePermissions {
    pub fn full() -> Self {
        Self {
            local_network: true,
            file_system_read: true,
            file_system_write: true,
            background_transfer: true,
        }
    }

    pub fn desktop_defaults() -> Self {
        Self::full()
    }

    pub fn mobile_defaults() -> Self {
        Self {
            local_network: true,
            file_system_read: true,
            file_system_write: true,
            background_transfer: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferOptions {
    pub chunk_size: usize,
    pub window_size: usize,
    pub timeout_ticks: u64,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            chunk_size: 256 * 1024,
            window_size: 64,
            timeout_ticks: 15_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequest {
    pub file_path: PathBuf,
    pub address: Option<String>,
    pub discovery_token: Option<String>,
    #[serde(default)]
    pub device_name: Option<String>,
    pub permissions: CorePermissions,
    pub options: TransferOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiveRequest {
    pub port: u16,
    pub output_dir: PathBuf,
    pub announce_on_lan: bool,
    pub device_name: Option<String>,
    #[serde(default)]
    pub require_pin: bool,
    #[serde(default = "default_true")]
    pub auto_accept: bool,
    pub permissions: CorePermissions,
    pub options: TransferOptions,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverRequest {
    pub token: Option<String>,
    pub timeout_secs: u64,
    pub permissions: CorePermissions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRemoteRequest {
    pub file_path: PathBuf,
    pub relay_server_url: String,
    pub session_id: String,
    pub my_peer_id: String,
    pub ice_servers: Vec<crate::signaling::IceServer>,
    pub connect_timeout_secs: u64,
    /// See [`SendRequest::device_name`].
    #[serde(default)]
    pub device_name: Option<String>,
    pub permissions: CorePermissions,
    pub options: TransferOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiveRemoteRequest {
    pub output_dir: PathBuf,
    pub relay_server_url: String,
    pub session_id: String,
    pub my_peer_id: String,
    pub ice_servers: Vec<crate::signaling::IceServer>,
    pub connect_timeout_secs: u64,
    /// See [`ReceiveRequest::auto_accept`].
    #[serde(default = "default_true")]
    pub auto_accept: bool,
    #[serde(default)]
    pub device_name: Option<String>,
    pub permissions: CorePermissions,
    pub options: TransferOptions,
}

pub fn generate_room_code() -> String {
    crate::discovery::PairingToken::generate_with_len(9)
        .code()
        .to_string()
}

pub fn generate_peer_id() -> String {
    crate::security::SessionId::generate().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkRequest {
    pub size_mb: usize,
    pub iterations: usize,
    pub latency_ticks: u64,
    pub options: TransferOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferMode {
    #[default]
    Lan,
    Direct,
    Relay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    Discovering,
    Listening,
    Connecting,
    SignalingConnected,
    NegotiatingIce,
    Connected,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferSummary {
    pub direction: TransferDirection,
    pub file_name: String,
    pub peer: Option<String>,
    #[serde(default)]
    pub peer_name: Option<String>,
    #[serde(default)]
    pub mode: TransferMode,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub resumed_bytes: u64,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySummary {
    pub hostname: String,
    pub address: String,
    pub token: String,
    #[serde(default)]
    pub pin_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkIterationSummary {
    pub iteration: usize,
    pub throughput_mib_s: f64,
    pub peak_sender_buffered_bytes: usize,
    pub peak_receiver_buffered_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSummary {
    pub size_mb: usize,
    pub iterations: Vec<BenchmarkIterationSummary>,
    pub average_throughput_mib_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferEvent {
    StateChanged {
        direction: TransferDirection,
        state: ConnectionState,
        peer: Option<String>,
    },
    IncomingRequest {
        direction: TransferDirection,
        file_name: String,
        total_bytes: u64,
        peer: Option<String>,
        sender_name: Option<String>,
    },
    ConnectionEstablished {
        direction: TransferDirection,
        mode: TransferMode,
    },
    AwaitingApproval {
        direction: TransferDirection,
        file_name: String,
    },
    Cancelled {
        direction: TransferDirection,
    },
    Declined {
        direction: TransferDirection,
        reason: String,
    },
    Failed {
        direction: TransferDirection,
        message: String,
        recoverable: bool,
    },
    Started {
        direction: TransferDirection,
        file_name: String,
        total_bytes: u64,
        resumed_bytes: u64,
    },
    Resumed {
        direction: TransferDirection,
        next_sequence: u32,
        resumed_bytes: u64,
    },
    Progress {
        direction: TransferDirection,
        transferred_bytes: u64,
        total_bytes: u64,
    },
    CheckpointUpdated {
        checkpoint_path: PathBuf,
        next_sequence: u32,
        bytes_written: u64,
    },
    Completed(TransferSummary),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiscoveryEvent {
    SearchStarted {
        token: Option<String>,
        timeout_secs: u64,
    },
    BroadcastStarted {
        token: String,
        port: u16,
    },
    PeerFound(DiscoverySummary),
    PeerNotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BenchmarkEvent {
    Started {
        size_mb: usize,
        iterations: usize,
        latency_ticks: u64,
    },
    IterationCompleted(BenchmarkIterationSummary),
    Completed(BenchmarkSummary),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlenumEvent {
    Log { level: LogLevel, message: String },
    Transfer(TransferEvent),
    Discovery(DiscoveryEvent),
    Benchmark(BenchmarkEvent),
}

pub trait EventSink {
    fn emit(&mut self, event: PlenumEvent);
}

impl<F> EventSink for F
where
    F: FnMut(PlenumEvent),
{
    fn emit(&mut self, event: PlenumEvent) {
        self(event);
    }
}
