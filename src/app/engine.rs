//! High-level app integration engine.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app::error::AppError;
use crate::app::types::{
    AcceptDecision, BenchmarkEvent, BenchmarkIterationSummary, BenchmarkRequest, BenchmarkSummary,
    ConnectionState, CorePermissions, DiscoverRequest, DiscoveryEvent, DiscoverySummary, EventSink,
    LogLevel, PermissionKind, PlenumEvent, ReceiveRemoteRequest, ReceiveRequest, SendRemoteRequest,
    SendRequest, SessionControl, TransferDirection, TransferEvent, TransferMode, TransferSummary,
};
use crate::discovery::{Beacon, PairingToken};
use crate::flow::{ReceiverWindow, SenderWindow};
use crate::protocol::{Packet, PacketType, encode_packet, parse_packet};
use crate::rtc::RtcTransport;
use crate::signaling::{IceServer, RoutedSignal, SignalMessage, SignalingState};
use crate::stream::{ResumeCheckpoint, chunk_bytes};
use crate::transport::{
    MemoryTransport, MemoryTransportConfig, SecureTransport, TcpTransport, Transport,
    TransportError,
};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

const ACCEPT_WAIT_TIMEOUT: Duration = Duration::from_secs(150);

const CLOSE_REASON_DECLINED: &str = "declined";
const CLOSE_REASON_PIN_REJECTED: &str = "pin_rejected";
const CLOSE_REASON_CANCELLED: &str = "cancelled";

const SEND_STALL_TIMEOUT: Duration = Duration::from_secs(30);

const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(100);

const CHECKPOINT_SAVE_INTERVAL: Duration = Duration::from_secs(2);
const CHECKPOINT_SAVE_BYTES: u64 = 4 * 1024 * 1024;

const RECEIVE_STALL_TIMEOUT: Duration = Duration::from_secs(45);

const FINISH_RETRY_INTERVAL: Duration = Duration::from_secs(5);

const STREAM_MODE_MAGIC: &[u8] = b"PLENUM_STREAM_V1";

const STREAM_ACK_INTERVAL_BYTES: u64 = 2 * 1024 * 1024;
const STREAM_MAX_IN_FLIGHT_BYTES: u64 = 8 * 1024 * 1024;
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(1);

const RTC_MAX_CHUNK_SIZE: usize = 32 * 1024;

/// Delays before the single forced-TURN-relay retry after a dead path or
/// connect timeout. Staggered by role on purpose: the receiver (answerer)
/// rejoins the session first and is ready to answer by the time the sender
/// (offerer) rejoins and sends its offer. Both delays also give the relay
/// server time to process the previous connections' disconnects.
const RELAY_RETRY_DELAY_SEND: Duration = Duration::from_secs(5);
const RELAY_RETRY_DELAY_RECEIVE: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub struct PlenumCore {
    signaling: SignalingState,
    control: SessionControl,
}

impl PlenumCore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn control(&self) -> SessionControl {
        self.control.clone()
    }

    pub fn send_file<S: EventSink>(
        &mut self,
        request: SendRequest,
        sink: &mut S,
    ) -> Result<crate::app::types::TransferSummary, AppError> {
        validate_send_request(&request)?;
        let mut file = File::open(&request.file_path)?;
        let file_size = file.metadata()?.len();
        let file_name = request
            .file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let address = match request.address.clone() {
            Some(addr) => addr,
            None => {
                sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
                    direction: TransferDirection::Send,
                    state: ConnectionState::Discovering,
                    peer: None,
                }));
                let discovery = self.discover_peer(
                    DiscoverRequest {
                        token: request.discovery_token.clone(),
                        timeout_secs: 10,
                        permissions: request.permissions.clone(),
                    },
                    sink,
                )?;
                discovery.address
            }
        };

        sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
            direction: TransferDirection::Send,
            state: ConnectionState::Connecting,
            peer: Some(address.clone()),
        }));
        let transport = TcpTransport::connect(&address)?;
        let mut transport = SecureTransport::connect(
            transport,
            request
                .discovery_token
                .as_deref()
                .filter(|token| !token.trim().is_empty()),
        )?;

        sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
            direction: TransferDirection::Send,
            state: ConnectionState::Connected,
            peer: Some(address.clone()),
        }));

        run_send_transfer(
            &mut transport,
            sink,
            &mut file,
            file_size,
            &file_name,
            &request.options,
            Some(address),
            &self.control,
            request.device_name.as_deref(),
        )
    }

    pub fn send_file_remote<S: EventSink>(
        &mut self,
        mut request: SendRemoteRequest,
        sink: &mut S,
    ) -> Result<TransferSummary, AppError> {
        validate_send_remote_request(&request)?;
        request.options.chunk_size = request.options.chunk_size.min(RTC_MAX_CHUNK_SIZE);
        ensure_turn_server(
            &mut request.ice_servers,
            &request.relay_server_url,
            &request.my_peer_id,
            sink,
        );

        let first = self.send_file_remote_attempt(&request, sink, false);
        let Err(error) = &first else {
            return first;
        };
        if !should_retry_via_relay(error, &request.ice_servers) || self.control.is_cancelled() {
            return first;
        }
        sink.emit(PlenumEvent::Log {
            level: LogLevel::Warn,
            message: format!(
                "DIAG rtc: send failed on dead/timed-out path ({error}); retrying once via forced TURN relay"
            ),
        });
        thread::sleep(RELAY_RETRY_DELAY_SEND);
        if self.control.is_cancelled() {
            return first;
        }
        self.send_file_remote_attempt(&request, sink, true)
    }

    fn send_file_remote_attempt<S: EventSink>(
        &mut self,
        request: &SendRemoteRequest,
        sink: &mut S,
        force_relay: bool,
    ) -> Result<TransferSummary, AppError> {
        let mut file = File::open(&request.file_path)?;
        let file_size = file.metadata()?.len();
        let file_name = request
            .file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
            direction: TransferDirection::Send,
            state: ConnectionState::SignalingConnected,
            peer: Some(request.session_id.clone()),
        }));

        let mut transport = RtcTransport::connect_as_offerer_cancellable(
            &request.relay_server_url,
            &request.session_id,
            &request.my_peer_id,
            request.ice_servers.clone(),
            Duration::from_secs(request.connect_timeout_secs),
            force_relay,
            self.control.cancel_flag(),
        )
        .map_err(|error| rtc_connect_error(error, TransferDirection::Send, sink))?;

        sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
            direction: TransferDirection::Send,
            state: ConnectionState::Connected,
            peer: Some(request.session_id.clone()),
        }));

        run_send_transfer(
            &mut transport,
            sink,
            &mut file,
            file_size,
            &file_name,
            &request.options,
            Some(request.session_id.clone()),
            &self.control,
            request.device_name.as_deref(),
        )
    }

    pub fn receive_file<S: EventSink>(
        &mut self,
        request: ReceiveRequest,
        sink: &mut S,
    ) -> Result<crate::app::types::TransferSummary, AppError> {
        validate_receive_request(&request)?;
        create_dir_all(&request.output_dir)?;
        let control = self.control.clone();
        control.reset_decision();

        let listener = TcpListener::bind(format!("0.0.0.0:{}", request.port))?;
        listener.set_nonblocking(true)?;
        let actual_port = listener.local_addr()?.port();

        sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
            direction: TransferDirection::Receive,
            state: ConnectionState::Listening,
            peer: Some(format!("0.0.0.0:{}", actual_port)),
        }));

        let token = PairingToken::generate();
        let broadcast_handle = if request.announce_on_lan {
            let beacon = Beacon::new();
            let handle = beacon.broadcast(
                &token,
                actual_port,
                request.device_name.clone(),
                request.require_pin,
            )?;
            sink.emit(PlenumEvent::Discovery(DiscoveryEvent::BroadcastStarted {
                token: token.code().to_string(),
                port: actual_port,
            }));
            Some(handle)
        } else {
            None
        };

        let stop_flag = Arc::new(AtomicBool::new(false));
        let broadcast_thread = if let Some(handle) = broadcast_handle {
            let flag = stop_flag.clone();
            Some(thread::spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    let _ = handle.send_once();
                    thread::sleep(handle.interval());
                }
            }))
        } else {
            None
        };

        let result =
            self.accept_and_receive(&request, &listener, &token, &control, &stop_flag, sink);

        stop_flag.store(true, Ordering::Relaxed);
        if let Some(thread) = broadcast_thread {
            let _ = thread.join();
        }

        result
    }

    fn accept_and_receive<S: EventSink>(
        &mut self,
        request: &ReceiveRequest,
        listener: &TcpListener,
        token: &PairingToken,
        control: &SessionControl,
        announce_stop: &AtomicBool,
        sink: &mut S,
    ) -> Result<crate::app::types::TransferSummary, AppError> {
        loop {
            let stream = loop {
                if control.is_cancelled() {
                    sink.emit(PlenumEvent::Transfer(TransferEvent::Cancelled {
                        direction: TransferDirection::Receive,
                    }));
                    return Err(AppError::Cancelled);
                }
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_nonblocking(false)?;

            let peer = stream
                .peer_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            let transport = TcpTransport::from_stream(stream)?;
            let mut transport = match SecureTransport::accept(
                transport,
                request.require_pin.then_some(token.code()),
            ) {
                Ok(transport) => transport,
                Err(error) if request.require_pin => {
                    sink.emit(PlenumEvent::Log {
                        level: LogLevel::Warn,
                        message: format!("rejected secure connection from {peer}: {error}"),
                    });
                    continue;
                }
                Err(error) => return Err(error.into()),
            };

            sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
                direction: TransferDirection::Receive,
                state: ConnectionState::Connected,
                peer: Some(peer.clone()),
            }));

            announce_stop.store(true, Ordering::Relaxed);

            return run_receive_transfer(
                &mut transport,
                sink,
                &request.output_dir,
                &request.options,
                peer,
                control,
                request.auto_accept,
                request.device_name.as_deref(),
            );
        }
    }

    pub fn receive_file_remote<S: EventSink>(
        &mut self,
        mut request: ReceiveRemoteRequest,
        sink: &mut S,
    ) -> Result<TransferSummary, AppError> {
        validate_receive_remote_request(&request)?;
        request.options.chunk_size = request.options.chunk_size.min(RTC_MAX_CHUNK_SIZE);
        ensure_turn_server(
            &mut request.ice_servers,
            &request.relay_server_url,
            &request.my_peer_id,
            sink,
        );

        let first = self.receive_file_remote_attempt(&request, sink, false);
        let Err(error) = &first else {
            return first;
        };
        if !should_retry_via_relay(error, &request.ice_servers) || self.control.is_cancelled() {
            return first;
        }
        sink.emit(PlenumEvent::Log {
            level: LogLevel::Warn,
            message: format!(
                "DIAG rtc: receive failed on dead/timed-out path ({error}); retrying once via forced TURN relay"
            ),
        });
        thread::sleep(RELAY_RETRY_DELAY_RECEIVE);
        if self.control.is_cancelled() {
            return first;
        }
        self.receive_file_remote_attempt(&request, sink, true)
    }

    fn receive_file_remote_attempt<S: EventSink>(
        &mut self,
        request: &ReceiveRemoteRequest,
        sink: &mut S,
        force_relay: bool,
    ) -> Result<TransferSummary, AppError> {
        create_dir_all(&request.output_dir)?;
        let control = self.control.clone();
        control.reset_decision();

        sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
            direction: TransferDirection::Receive,
            state: ConnectionState::SignalingConnected,
            peer: Some(request.session_id.clone()),
        }));

        let mut transport = RtcTransport::connect_as_answerer_cancellable(
            &request.relay_server_url,
            &request.session_id,
            &request.my_peer_id,
            request.ice_servers.clone(),
            Duration::from_secs(request.connect_timeout_secs),
            force_relay,
            control.cancel_flag(),
        )
        .map_err(|error| rtc_connect_error(error, TransferDirection::Receive, sink))?;

        sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
            direction: TransferDirection::Receive,
            state: ConnectionState::Connected,
            peer: Some(request.session_id.clone()),
        }));

        run_receive_transfer(
            &mut transport,
            sink,
            &request.output_dir,
            &request.options,
            request.session_id.clone(),
            &control,
            request.auto_accept,
            request.device_name.as_deref(),
        )
    }

    pub fn discover_peer<S: EventSink>(
        &mut self,
        request: DiscoverRequest,
        sink: &mut S,
    ) -> Result<DiscoverySummary, AppError> {
        validate_discover_request(&request)?;
        sink.emit(PlenumEvent::Discovery(DiscoveryEvent::SearchStarted {
            token: request.token.clone(),
            timeout_secs: request.timeout_secs,
        }));

        let beacon = Beacon::with_config(crate::discovery::beacon::BeaconConfig {
            discover_timeout: Duration::from_secs(request.timeout_secs),
            ..Default::default()
        });

        let result = match request.token.as_deref() {
            Some(token) => beacon.discover_with_token(token),
            None => beacon.discover(),
        };

        match result {
            Ok(announcement) => {
                let address = announcement.tcp_addr().to_string();
                let summary = DiscoverySummary {
                    hostname: announcement.hostname,
                    address,
                    token: announcement.token,
                    pin_required: announcement.pin_required,
                };
                sink.emit(PlenumEvent::Discovery(DiscoveryEvent::PeerFound(
                    summary.clone(),
                )));
                Ok(summary)
            }
            Err(crate::discovery::DiscoveryError::NoPeersFound) => {
                sink.emit(PlenumEvent::Discovery(DiscoveryEvent::PeerNotFound));
                Err(AppError::from(
                    crate::discovery::DiscoveryError::NoPeersFound,
                ))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn benchmark<S: EventSink>(
        &mut self,
        request: BenchmarkRequest,
        sink: &mut S,
    ) -> Result<BenchmarkSummary, AppError> {
        if request.size_mb == 0 {
            return Err(AppError::InvalidRequest(
                "benchmark size must be greater than zero".into(),
            ));
        }
        if request.iterations == 0 {
            return Err(AppError::InvalidRequest(
                "benchmark iterations must be greater than zero".into(),
            ));
        }
        if request.options.chunk_size == 0 || request.options.window_size == 0 {
            return Err(AppError::InvalidRequest(
                "transfer tuning values must be greater than zero".into(),
            ));
        }

        sink.emit(PlenumEvent::Benchmark(BenchmarkEvent::Started {
            size_mb: request.size_mb,
            iterations: request.iterations,
            latency_ticks: request.latency_ticks,
        }));

        let size_bytes = request.size_mb * 1024 * 1024;
        let payload: Vec<u8> = (0..size_bytes).map(|idx| (idx % 251) as u8).collect();
        let mut total_secs = 0.0;
        let mut iterations = Vec::with_capacity(request.iterations);

        for iteration in 0..request.iterations {
            let started = Instant::now();
            let packets = chunk_bytes(&payload, request.options.chunk_size)?;
            let mut sender =
                SenderWindow::new(request.options.window_size, request.options.timeout_ticks)?;
            for packet in packets {
                sender.enqueue(packet)?;
            }
            let mut receiver = ReceiverWindow::new();
            let mut data_transport = MemoryTransport::new(MemoryTransportConfig {
                latency_ticks: request.latency_ticks,
                reorder_every: Some(3),
                ..MemoryTransportConfig::default()
            });
            let mut control_transport = MemoryTransport::new(MemoryTransportConfig {
                latency_ticks: request.latency_ticks,
                ..MemoryTransportConfig::default()
            });
            let mut restored = Vec::with_capacity(payload.len());
            let mut peak_sender_buffered_bytes = 0usize;
            let mut peak_receiver_buffered_bytes = 0usize;

            for tick in 0..200_000_u64 {
                peak_sender_buffered_bytes =
                    peak_sender_buffered_bytes.max(sender.buffered_payload_bytes());
                peak_receiver_buffered_bytes =
                    peak_receiver_buffered_bytes.max(receiver.buffered_payload_bytes());

                sender.retransmit_due(&mut data_transport, tick)?;
                sender.send_available(&mut data_transport, tick)?;

                while let Some(frame) = data_transport.recv()? {
                    let packet = parse_packet(&frame)?;
                    let controls = receiver.receive_data_packet(packet)?;
                    for (_, payload) in receiver.drain_ordered_packets() {
                        restored.extend_from_slice(&payload);
                    }
                    for control in controls {
                        control_transport.send(&encode_packet(&control)?)?;
                    }
                }

                while let Some(frame) = control_transport.recv()? {
                    let control = parse_packet(&frame)?;
                    sender.handle_control_packet(&control)?;
                }

                if sender.is_empty() && restored == payload {
                    break;
                }

                data_transport.tick();
                control_transport.tick();
            }

            let secs = started.elapsed().as_secs_f64();
            total_secs += secs;
            let iteration_summary = BenchmarkIterationSummary {
                iteration: iteration + 1,
                throughput_mib_s: if secs > 0.0 {
                    (size_bytes as f64 / (1024.0 * 1024.0)) / secs
                } else {
                    0.0
                },
                peak_sender_buffered_bytes,
                peak_receiver_buffered_bytes,
            };
            sink.emit(PlenumEvent::Benchmark(BenchmarkEvent::IterationCompleted(
                iteration_summary.clone(),
            )));
            iterations.push(iteration_summary);
        }

        let summary = BenchmarkSummary {
            size_mb: request.size_mb,
            average_throughput_mib_s: total_secs
                .is_normal()
                .then_some(
                    (size_bytes as f64 / (1024.0 * 1024.0))
                        / (total_secs / request.iterations as f64),
                )
                .unwrap_or(0.0),
            iterations,
        };
        sink.emit(PlenumEvent::Benchmark(BenchmarkEvent::Completed(
            summary.clone(),
        )));
        Ok(summary)
    }

    pub fn handle_signal(&mut self, message: SignalMessage) -> Result<Vec<RoutedSignal>, AppError> {
        Ok(self.signaling.handle(message)?)
    }

    pub fn session_of(&self, peer_id: &str) -> Option<String> {
        self.signaling.session_of(peer_id).map(str::to_string)
    }

    pub fn peers_in_session(&self, session_id: &str) -> Option<Vec<String>> {
        self.signaling.peers_in_session(session_id)
    }
}

fn validate_send_request(request: &SendRequest) -> Result<(), AppError> {
    require_permission(
        &request.permissions,
        PermissionKind::FileSystemRead,
        "send_file",
    )?;
    require_permission(
        &request.permissions,
        PermissionKind::LocalNetwork,
        "send_file",
    )?;
    validate_transfer_options(&request.options)?;
    Ok(())
}

fn validate_receive_request(request: &ReceiveRequest) -> Result<(), AppError> {
    require_permission(
        &request.permissions,
        PermissionKind::FileSystemWrite,
        "receive_file",
    )?;
    require_permission(
        &request.permissions,
        PermissionKind::LocalNetwork,
        "receive_file",
    )?;
    validate_transfer_options(&request.options)?;
    Ok(())
}

fn validate_discover_request(request: &DiscoverRequest) -> Result<(), AppError> {
    require_permission(
        &request.permissions,
        PermissionKind::LocalNetwork,
        "discover_peer",
    )?;
    if request.timeout_secs == 0 {
        return Err(AppError::InvalidRequest(
            "discover timeout must be greater than zero".into(),
        ));
    }
    Ok(())
}

fn validate_send_remote_request(request: &SendRemoteRequest) -> Result<(), AppError> {
    require_permission(
        &request.permissions,
        PermissionKind::FileSystemRead,
        "send_file_remote",
    )?;
    validate_remote_request_fields(
        &request.relay_server_url,
        &request.session_id,
        request.connect_timeout_secs,
    )?;
    validate_transfer_options(&request.options)?;
    Ok(())
}

fn validate_receive_remote_request(request: &ReceiveRemoteRequest) -> Result<(), AppError> {
    require_permission(
        &request.permissions,
        PermissionKind::FileSystemWrite,
        "receive_file_remote",
    )?;
    validate_remote_request_fields(
        &request.relay_server_url,
        &request.session_id,
        request.connect_timeout_secs,
    )?;
    validate_transfer_options(&request.options)?;
    Ok(())
}

fn validate_remote_request_fields(
    relay_server_url: &str,
    session_id: &str,
    connect_timeout_secs: u64,
) -> Result<(), AppError> {
    if relay_server_url.trim().is_empty() {
        return Err(AppError::InvalidRequest(
            "relay server url must not be empty".into(),
        ));
    }
    if session_id.trim().is_empty() {
        return Err(AppError::InvalidRequest(
            "session id must not be empty".into(),
        ));
    }
    if connect_timeout_secs == 0 {
        return Err(AppError::InvalidRequest(
            "connect timeout must be greater than zero".into(),
        ));
    }
    Ok(())
}

fn validate_transfer_options(options: &crate::app::types::TransferOptions) -> Result<(), AppError> {
    if options.chunk_size == 0 {
        return Err(AppError::InvalidRequest(
            "chunk size must be greater than zero".into(),
        ));
    }
    if options.window_size == 0 {
        return Err(AppError::InvalidRequest(
            "window size must be greater than zero".into(),
        ));
    }
    Ok(())
}

fn require_permission(
    permissions: &CorePermissions,
    kind: PermissionKind,
    operation: &'static str,
) -> Result<(), AppError> {
    let granted = match kind {
        PermissionKind::LocalNetwork => permissions.local_network,
        PermissionKind::FileSystemRead => permissions.file_system_read,
        PermissionKind::FileSystemWrite => permissions.file_system_write,
        PermissionKind::BackgroundTransfer => permissions.background_transfer,
    };

    if granted {
        Ok(())
    } else {
        Err(AppError::PermissionDenied {
            permission: kind,
            operation,
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

const START_PAYLOAD_V2_SENTINEL: u8 = 0xFF;
fn encode_start_payload(file_size: u64, file_name: &str, device_name: Option<&str>) -> Vec<u8> {
    let name_bytes = file_name.as_bytes();
    match device_name {
        None => {
            let mut payload = Vec::with_capacity(8 + name_bytes.len());
            payload.extend_from_slice(&file_size.to_be_bytes());
            payload.extend_from_slice(name_bytes);
            payload
        }
        Some(device_name) => {
            let mut payload = Vec::with_capacity(11 + name_bytes.len() + device_name.len());
            payload.push(START_PAYLOAD_V2_SENTINEL);
            payload.extend_from_slice(&file_size.to_be_bytes());
            payload.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
            payload.extend_from_slice(name_bytes);
            payload.extend_from_slice(device_name.as_bytes());
            payload
        }
    }
}

fn parse_start_payload(payload: &[u8]) -> Result<(u64, String, Option<String>), AppError> {
    if payload.first() == Some(&START_PAYLOAD_V2_SENTINEL) {
        if payload.len() < 11 {
            return Err(AppError::InvalidRequest(
                "versioned start packet payload is too short".into(),
            ));
        }
        let mut size_bytes = [0u8; 8];
        size_bytes.copy_from_slice(&payload[1..9]);
        let file_size = u64::from_be_bytes(size_bytes);
        let name_len = u16::from_be_bytes([payload[9], payload[10]]) as usize;
        if payload.len() < 11 + name_len {
            return Err(AppError::InvalidRequest(
                "start packet file name is truncated".into(),
            ));
        }
        let file_name = String::from_utf8_lossy(&payload[11..11 + name_len]).into_owned();
        let device_bytes = &payload[11 + name_len..];
        let sender_name = if device_bytes.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(device_bytes).into_owned())
        };
        Ok((file_size, file_name, sender_name))
    } else {
        if payload.len() < 8 {
            return Err(AppError::InvalidRequest(
                "start packet payload must contain file size".into(),
            ));
        }
        let mut size_bytes = [0u8; 8];
        size_bytes.copy_from_slice(&payload[0..8]);
        let file_size = u64::from_be_bytes(size_bytes);
        let file_name = String::from_utf8_lossy(&payload[8..]).into_owned();
        Ok((file_size, file_name, None))
    }
}

fn parse_accept_payload(payload: &[u8]) -> Option<String> {
    if payload.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(payload).into_owned())
    }
}

fn transfer_mode<T: Transport + ?Sized>(transport: &T) -> TransferMode {
    match transport.is_relayed() {
        None => TransferMode::Lan,
        Some(false) => TransferMode::Direct,
        Some(true) => TransferMode::Relay,
    }
}

fn error_category(error: &AppError) -> String {
    match error {
        AppError::Transport(error) => format!("transport::{error:?}"),
        AppError::Stalled(_) => "stalled".to_string(),
        AppError::Cancelled => "cancelled".to_string(),
        AppError::Rejected(_) => "rejected".to_string(),
        AppError::Io(error) => format!("io::{:?}", error.kind()),
        _ => error.to_string(),
    }
}

fn emit_failed<S: EventSink>(
    sink: &mut S,
    direction: TransferDirection,
    error: &AppError,
    partial_saved: bool,
) {
    if matches!(error, AppError::Cancelled | AppError::Rejected(_)) {
        return;
    }
    let (message, recoverable) = match direction {
        TransferDirection::Receive if partial_saved => (
            "Connection lost. Your partial transfer was saved and can be resumed.".to_string(),
            true,
        ),
        TransferDirection::Receive => ("The sender stopped responding.".to_string(), false),
        TransferDirection::Send => ("The receiver stopped responding.".to_string(), false),
    };
    sink.emit(PlenumEvent::Transfer(TransferEvent::Failed {
        direction,
        message,
        recoverable,
    }));
}

fn emit_sender_failure<S: EventSink>(
    sink: &mut S,
    error: &AppError,
    bytes_sent: u64,
    bytes_acked: u64,
    sequence_no: u32,
    last_ack: Instant,
) {
    if matches!(error, AppError::Cancelled | AppError::Rejected(_)) {
        return;
    }
    sink.emit(PlenumEvent::Log {
        level: LogLevel::Error,
        message: format!(
            "DIAG send failure: bytes_sent={bytes_sent} bytes_acked={bytes_acked} outstanding={} sequence={sequence_no} last_ack_ms={} category={} error={error}",
            bytes_sent.saturating_sub(bytes_acked),
            last_ack.elapsed().as_millis(),
            error_category(error),
        ),
    });
    emit_failed(sink, TransferDirection::Send, error, false);
}

fn save_receive_failure_checkpoint(
    file: &mut Option<BufWriter<File>>,
    checkpoint: &mut Option<ResumeCheckpoint>,
    checkpoint_path: Option<&Path>,
    next_sequence: u32,
    bytes_received: u64,
) -> bool {
    let Some(checkpoint) = checkpoint.as_mut() else {
        return false;
    };
    if file.as_mut().is_some_and(|file| file.flush().is_err()) {
        return false;
    }
    checkpoint.update(next_sequence, bytes_received);
    checkpoint_path.is_some_and(|path| checkpoint.save(path).is_ok())
}

fn run_send_transfer<T: Transport, S: EventSink>(
    transport: &mut T,
    sink: &mut S,
    file: &mut File,
    file_size: u64,
    file_name: &str,
    options: &crate::app::types::TransferOptions,
    peer_label: Option<String>,
    control: &SessionControl,
    device_name: Option<&str>,
) -> Result<TransferSummary, AppError> {
    let file_name = file_name.to_string();

    sink.emit(PlenumEvent::Transfer(
        TransferEvent::ConnectionEstablished {
            direction: TransferDirection::Send,
            mode: transfer_mode(transport),
        },
    ));

    transport.send(&encode_packet(&Packet::new(
        PacketType::Start,
        0,
        encode_start_payload(file_size, &file_name, device_name),
    ))?)?;

    sink.emit(PlenumEvent::Transfer(TransferEvent::AwaitingApproval {
        direction: TransferDirection::Send,
        file_name: file_name.clone(),
    }));
    let (mut sequence_no, resume_bytes, receiver_name, receiver_streams) =
        wait_for_accept(transport, control, sink, TransferDirection::Send)?;

    let started_at = Instant::now();

    if resume_bytes > 0 {
        sink.emit(PlenumEvent::Transfer(TransferEvent::Resumed {
            direction: TransferDirection::Send,
            next_sequence: sequence_no,
            resumed_bytes: resume_bytes,
        }));
        file.seek(SeekFrom::Start(resume_bytes))?;
    }

    sink.emit(PlenumEvent::Transfer(TransferEvent::Started {
        direction: TransferDirection::Send,
        file_name: file_name.clone(),
        total_bytes: file_size,
        resumed_bytes: resume_bytes,
    }));

    sink.emit(PlenumEvent::Log {
        level: LogLevel::Info,
        message: format!(
            "DIAG send: transfer loop start, file={file_name} size={file_size} chunk={} window={}",
            options.chunk_size, options.window_size
        ),
    });

    // Streaming mode: only over TCP (is_relayed() is None on the LAN path) and
    // only when the receiver offered it during the accept handshake. The
    // confirmation Resume must precede the first Data packet; TCP ordering
    // guarantees the receiver switches modes before any data arrives.
    let streaming = receiver_streams && transport.is_relayed().is_none();
    if streaming {
        transport.send(&encode_packet(&Packet::new(
            PacketType::Resume,
            0,
            STREAM_MODE_MAGIC.to_vec(),
        ))?)?;
        sink.emit(PlenumEvent::Log {
            level: LogLevel::Info,
            message: "DIAG send: streaming mode enabled (cumulative ACKs over TCP)".to_string(),
        });
        run_streaming_send_loop(
            transport,
            sink,
            file,
            file_size,
            options,
            sequence_no,
            resume_bytes,
            control,
        )?;
    } else {
        run_windowed_send_loop(
            transport,
            sink,
            file,
            file_size,
            options,
            &mut sequence_no,
            resume_bytes,
            control,
        )?;

        transport.send(&encode_packet(&Packet::new(
            PacketType::Finish,
            sequence_no,
            Vec::new(),
        ))?)?;
    }

    let mode = transfer_mode(transport);
    transport.close()?;

    let summary = TransferSummary {
        direction: TransferDirection::Send,
        file_name,
        peer: peer_label,
        peer_name: receiver_name,
        mode,
        total_bytes: file_size,
        transferred_bytes: file_size,
        resumed_bytes: resume_bytes,
        elapsed_ms: started_at.elapsed().as_millis(),
    };
    sink.emit(PlenumEvent::Transfer(TransferEvent::Completed(
        summary.clone(),
    )));
    sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
        direction: TransferDirection::Send,
        state: ConnectionState::Closed,
        peer: summary.peer.clone(),
    }));
    Ok(summary)
}

// Legacy send path: sliding window with per-packet ACKs and retransmits.
fn run_windowed_send_loop<T: Transport, S: EventSink>(
    transport: &mut T,
    sink: &mut S,
    file: &mut File,
    file_size: u64,
    options: &crate::app::types::TransferOptions,
    sequence_no: &mut u32,
    resume_bytes: u64,
    control: &SessionControl,
) -> Result<(), AppError> {
    let mut sender = SenderWindow::new(options.window_size, options.timeout_ticks)?;
    let mut ack_sizes = BTreeMap::<u32, usize>::new();
    let mut file_done = resume_bytes >= file_size;
    let mut buffer = vec![0u8; options.chunk_size];
    let mut bytes_acked = resume_bytes;
    let mut bytes_sent = resume_bytes;
    let mut last_inbound = Instant::now();
    let mut last_progress_emit = Instant::now() - PROGRESS_EMIT_INTERVAL;
    let mut progress_dirty = false;

    let mut diag_acks_recv: u64 = 0;
    let mut diag_data_sent: u64 = 0;
    let mut diag_last = now_ms();

    loop {
        let mut made_progress = false;

        if control.is_cancelled() {
            let _ = transport.send(&encode_packet(&Packet::new(
                PacketType::Close,
                *sequence_no,
                CLOSE_REASON_CANCELLED.as_bytes().to_vec(),
            ))?);
            let _ = transport.close();
            sink.emit(PlenumEvent::Transfer(TransferEvent::Cancelled {
                direction: TransferDirection::Send,
            }));
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: format!(
                    "DIAG send cancelled: bytes_sent={bytes_sent} bytes_acked={bytes_acked} sequence={}",
                    *sequence_no
                ),
            });
            return Err(AppError::Cancelled);
        }

        let now = now_ms();
        while !file_done && sender.pending_len() < options.window_size * 2 {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                file_done = true;
                break;
            }

            let packet = Packet::new(PacketType::Data, *sequence_no, buffer[..n].to_vec());
            sender.enqueue(packet)?;
            ack_sizes.insert(*sequence_no, n);
            bytes_sent = bytes_sent.saturating_add(n as u64);
            *sequence_no = sequence_no.saturating_add(1);
            made_progress = true;
        }

        loop {
            let Some(frame) = transport.recv().map_err(|error| {
                let error = AppError::from(error);
                emit_sender_failure(
                    sink,
                    &error,
                    bytes_sent,
                    bytes_acked,
                    *sequence_no,
                    last_inbound,
                );
                error
            })?
            else {
                break;
            };
            made_progress = true;
            last_inbound = Instant::now();
            let ctrl_packet = parse_packet(&frame)?;
            match ctrl_packet.packet_type {
                PacketType::Close => {
                    let reason = String::from_utf8_lossy(&ctrl_packet.payload).into_owned();
                    sink.emit(PlenumEvent::Transfer(TransferEvent::Declined {
                        direction: TransferDirection::Send,
                        reason: reason.clone(),
                    }));
                    sink.emit(PlenumEvent::Log {
                        level: LogLevel::Info,
                        message: format!("DIAG send declined: reason={reason}"),
                    });
                    let error = AppError::Rejected(rejection_message(&reason));
                    return Err(error);
                }
                PacketType::Accept | PacketType::Resume | PacketType::Auth => continue,
                _ => {}
            }
            if ctrl_packet.packet_type == PacketType::Ack {
                diag_acks_recv = diag_acks_recv.saturating_add(1);
                if let Some(size) = ack_sizes.remove(&ctrl_packet.sequence_no) {
                    bytes_acked = bytes_acked.saturating_add(size as u64);
                    progress_dirty = true;
                }
            }
            sender.handle_control_packet(&ctrl_packet)?;
        }

        if progress_dirty
            && (last_progress_emit.elapsed() >= PROGRESS_EMIT_INTERVAL || bytes_acked >= file_size)
        {
            progress_dirty = false;
            last_progress_emit = Instant::now();
            sink.emit(PlenumEvent::Transfer(TransferEvent::Progress {
                direction: TransferDirection::Send,
                transferred_bytes: bytes_acked.min(file_size),
                total_bytes: file_size,
            }));
        }

        let retransmitted = sender.retransmit_due(transport, now).map_err(|error| {
            let error = AppError::from(error);
            emit_sender_failure(
                sink,
                &error,
                bytes_sent,
                bytes_acked,
                *sequence_no,
                last_inbound,
            );
            error
        })?;
        made_progress |= retransmitted > 0;
        let just_sent = sender.send_available(transport, now).map_err(|error| {
            let error = AppError::from(error);
            emit_sender_failure(
                sink,
                &error,
                bytes_sent,
                bytes_acked,
                *sequence_no,
                last_inbound,
            );
            error
        })?;
        made_progress |= just_sent > 0;
        diag_data_sent = diag_data_sent.saturating_add(just_sent as u64);

        if now.saturating_sub(diag_last) >= 1000 {
            diag_last = now;
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: format!(
                    "DIAG send: data_sent={diag_data_sent} acks_recv={diag_acks_recv} bytes_acked={bytes_acked} in_flight={} pending={} seq_next={sequence_no}",
                    sender.in_flight_len(),
                    sender.pending_len()
                ),
            });
        }

        for diag in transport.poll_diagnostics() {
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: diag,
            });
        }

        if file_done && sender.is_empty() {
            break;
        }

        if !sender.is_empty() && last_inbound.elapsed() >= SEND_STALL_TIMEOUT {
            let error = AppError::Stalled(format!(
                "no packets from receiver for {}s ({} in flight, {} pending)",
                SEND_STALL_TIMEOUT.as_secs(),
                sender.in_flight_len(),
                sender.pending_len()
            ));
            emit_sender_failure(
                sink,
                &error,
                bytes_sent,
                bytes_acked,
                *sequence_no,
                last_inbound,
            );
            return Err(error);
        }

        if !made_progress {
            thread::sleep(IDLE_POLL_INTERVAL);
        }
    }

    if progress_dirty {
        sink.emit(PlenumEvent::Transfer(TransferEvent::Progress {
            direction: TransferDirection::Send,
            transferred_bytes: bytes_acked.min(file_size),
            total_bytes: file_size,
        }));
    }

    sink.emit(PlenumEvent::Log {
        level: LogLevel::Info,
        message: format!(
            "DIAG send: transfer loop END data_sent={diag_data_sent} acks_recv={diag_acks_recv} bytes_acked={bytes_acked}"
        ),
    });
    Ok(())
}

/// Streaming send path (TCP only, negotiated via `STREAM_MODE_MAGIC`): Data
/// packets are written back-to-back while the transport's bounded nonblocking
/// write loop provides backpressure, and sparse cumulative ACKs drive progress
/// reporting. No retransmits.
#[allow(clippy::too_many_arguments)]
fn run_streaming_send_loop<T: Transport, S: EventSink>(
    transport: &mut T,
    sink: &mut S,
    file: &mut File,
    file_size: u64,
    options: &crate::app::types::TransferOptions,
    mut sequence_no: u32,
    resume_bytes: u64,
    control: &SessionControl,
) -> Result<(), AppError> {
    let chunk_size = options.chunk_size;
    let configured_window = (chunk_size as u64).saturating_mul(options.window_size as u64);
    let max_in_flight = configured_window
        .min(STREAM_MAX_IN_FLIGHT_BYTES)
        .max(chunk_size as u64);
    let mut buffer = vec![0u8; chunk_size];
    let mut ack_sizes = BTreeMap::<u32, usize>::new();
    let mut bytes_acked = resume_bytes;
    let mut bytes_sent = resume_bytes;
    let mut file_done = resume_bytes >= file_size;
    let mut finish_sent = false;
    let mut last_finish_sent = Instant::now();
    let mut last_inbound = Instant::now();
    let mut last_progress_emit = Instant::now() - PROGRESS_EMIT_INTERVAL;
    let mut diag_data_sent: u64 = 0;
    let mut diag_acks_recv: u64 = 0;
    let mut diag_last = now_ms();

    sink.emit(PlenumEvent::Log {
        level: LogLevel::Info,
        message: format!("DIAG send: streaming backpressure max_in_flight={max_in_flight} bytes"),
    });

    loop {
        let mut made_progress = false;

        if control.is_cancelled() {
            let _ = transport.send(&encode_packet(&Packet::new(
                PacketType::Close,
                sequence_no,
                CLOSE_REASON_CANCELLED.as_bytes().to_vec(),
            ))?);
            let _ = transport.close();
            sink.emit(PlenumEvent::Transfer(TransferEvent::Cancelled {
                direction: TransferDirection::Send,
            }));
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: format!(
                    "DIAG send cancelled: bytes_sent={bytes_sent} bytes_acked={bytes_acked} sequence={sequence_no}"
                ),
            });
            return Err(AppError::Cancelled);
        }

        let remaining = file_size.saturating_sub(bytes_sent);
        if remaining == 0 {
            file_done = true;
        }

        let next_chunk_len = remaining.min(chunk_size as u64) as usize;
        let in_flight = bytes_sent.saturating_sub(bytes_acked);
        let window_full =
            !file_done && in_flight.saturating_add(next_chunk_len as u64) > max_in_flight;

        // Empty nonblocking polls are cheap, so drain all currently available
        // control frames on every iteration.
        loop {
            // Fully acknowledged: stop draining. The receiver tears its side
            // down after the final cumulative ACK, so another recv could turn
            // the expected EOF into a spurious error.
            if file_done && bytes_acked >= file_size {
                break;
            }
            let frame = match transport.recv() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    if file_done && bytes_acked >= file_size {
                        break;
                    }
                    let error = AppError::from(error);
                    emit_sender_failure(
                        sink,
                        &error,
                        bytes_sent,
                        bytes_acked,
                        sequence_no,
                        last_inbound,
                    );
                    return Err(error);
                }
            };
            made_progress = true;
            last_inbound = Instant::now();
            let ctrl_packet = parse_packet(&frame)?;
            match ctrl_packet.packet_type {
                PacketType::Ack => {
                    diag_acks_recv = diag_acks_recv.saturating_add(1);
                    // Cumulative: one ACK covers every outstanding
                    // sequence up to and including its sequence number.
                    let acked: Vec<u32> = ack_sizes
                        .range(..=ctrl_packet.sequence_no)
                        .map(|(&seq, _)| seq)
                        .collect();
                    for seq in acked {
                        if let Some(size) = ack_sizes.remove(&seq) {
                            bytes_acked = bytes_acked.saturating_add(size as u64);
                        }
                    }
                    if last_progress_emit.elapsed() >= PROGRESS_EMIT_INTERVAL
                        || bytes_acked >= file_size
                    {
                        last_progress_emit = Instant::now();
                        sink.emit(PlenumEvent::Transfer(TransferEvent::Progress {
                            direction: TransferDirection::Send,
                            transferred_bytes: bytes_acked.min(file_size),
                            total_bytes: file_size,
                        }));
                    }
                }
                PacketType::Close => {
                    let reason = String::from_utf8_lossy(&ctrl_packet.payload).into_owned();
                    sink.emit(PlenumEvent::Transfer(TransferEvent::Declined {
                        direction: TransferDirection::Send,
                        reason: reason.clone(),
                    }));
                    sink.emit(PlenumEvent::Log {
                        level: LogLevel::Info,
                        message: format!("DIAG send declined: reason={reason}"),
                    });
                    let error = AppError::Rejected(rejection_message(&reason));
                    return Err(error);
                }
                _ => {}
            }
        }

        // Recompute on the next iteration after draining ACKs. Avoid reading
        // another file chunk until there is room for that entire sequence,
        // preserving fixed chunk boundaries used by resume checkpoints.
        if window_full {
            if last_inbound.elapsed() >= SEND_STALL_TIMEOUT {
                let error = AppError::Stalled(format!(
                    "no packets from receiver for {}s ({} bytes unacknowledged; {} byte limit)",
                    SEND_STALL_TIMEOUT.as_secs(),
                    bytes_sent.saturating_sub(bytes_acked),
                    max_in_flight
                ));
                emit_sender_failure(
                    sink,
                    &error,
                    bytes_sent,
                    bytes_acked,
                    sequence_no,
                    last_inbound,
                );
                return Err(error);
            }
            if !made_progress {
                thread::sleep(IDLE_POLL_INTERVAL);
            }
            continue;
        }

        if !file_done {
            let n = file.read(&mut buffer[..next_chunk_len])?;
            if n == 0 {
                file_done = true;
            } else {
                let packet = Packet::new(PacketType::Data, sequence_no, buffer[..n].to_vec());
                transport.send(&encode_packet(&packet)?).map_err(|error| {
                    let error = AppError::from(error);
                    emit_sender_failure(
                        sink,
                        &error,
                        bytes_sent,
                        bytes_acked,
                        sequence_no,
                        last_inbound,
                    );
                    error
                })?;
                ack_sizes.insert(sequence_no, n);
                sequence_no = sequence_no.saturating_add(1);
                bytes_sent = bytes_sent.saturating_add(n as u64);
                diag_data_sent = diag_data_sent.saturating_add(1);
                file_done = bytes_sent >= file_size;
                made_progress = true;
            }
        }

        // The Finish goes out immediately after the last Data packet so the
        // receiver knows to emit its final cumulative ACK: the tail of the
        // file is usually smaller than STREAM_ACK_INTERVAL_BYTES and would
        // otherwise never be acknowledged.
        if file_done && !finish_sent {
            finish_sent = true;
            transport
                .send(&encode_packet(&Packet::new(
                    PacketType::Finish,
                    sequence_no,
                    Vec::new(),
                ))?)
                .map_err(|error| {
                    let error = AppError::from(error);
                    emit_sender_failure(
                        sink,
                        &error,
                        bytes_sent,
                        bytes_acked,
                        sequence_no,
                        last_inbound,
                    );
                    error
                })?;
            made_progress = true;
        }

        let now = now_ms();
        if now.saturating_sub(diag_last) >= 1000 {
            diag_last = now;
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: format!(
                    "DIAG send: streaming data_sent={diag_data_sent} acks_recv={diag_acks_recv} bytes_sent={bytes_sent} bytes_acked={bytes_acked} in_flight={} max_in_flight={max_in_flight} tracked_chunks={} seq_next={sequence_no}",
                    bytes_sent.saturating_sub(bytes_acked),
                    ack_sizes.len()
                ),
            });
        }

        for diag in transport.poll_diagnostics() {
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: diag,
            });
        }

        if file_done {
            if bytes_acked >= file_size {
                break;
            }
            if last_finish_sent.elapsed() >= FINISH_RETRY_INTERVAL {
                last_finish_sent = Instant::now();
                if let Ok(bytes) =
                    encode_packet(&Packet::new(PacketType::Finish, sequence_no, Vec::new()))
                {
                    if transport.send(&bytes).is_ok() {
                        made_progress = true;
                    }
                }
            }
            if last_inbound.elapsed() >= SEND_STALL_TIMEOUT {
                let error = AppError::Stalled(format!(
                    "no cumulative ACK from receiver for {}s ({} of {} bytes acknowledged)",
                    SEND_STALL_TIMEOUT.as_secs(),
                    bytes_acked,
                    file_size
                ));
                emit_sender_failure(
                    sink,
                    &error,
                    bytes_sent,
                    bytes_acked,
                    sequence_no,
                    last_inbound,
                );
                return Err(error);
            }
        } else if bytes_acked < bytes_sent && last_inbound.elapsed() >= SEND_STALL_TIMEOUT {
            let error = AppError::Stalled(format!(
                "no packets from receiver for {}s ({} bytes unacknowledged)",
                SEND_STALL_TIMEOUT.as_secs(),
                bytes_sent - bytes_acked
            ));
            emit_sender_failure(
                sink,
                &error,
                bytes_sent,
                bytes_acked,
                sequence_no,
                last_inbound,
            );
            return Err(error);
        }

        if !made_progress {
            thread::sleep(IDLE_POLL_INTERVAL);
        }
    }

    sink.emit(PlenumEvent::Log {
        level: LogLevel::Info,
        message: format!(
            "DIAG send: streaming loop END data_sent={diag_data_sent} acks_recv={diag_acks_recv} bytes_acked={bytes_acked}"
        ),
    });

    sink.emit(PlenumEvent::Transfer(TransferEvent::Progress {
        direction: TransferDirection::Send,
        transferred_bytes: bytes_acked.min(file_size),
        total_bytes: file_size,
    }));
    Ok(())
}

fn run_receive_transfer<T: Transport, S: EventSink>(
    transport: &mut T,
    sink: &mut S,
    output_dir: &Path,
    options: &crate::app::types::TransferOptions,
    peer_label: String,
    control: &SessionControl,
    auto_accept: bool,
    device_name: Option<&str>,
) -> Result<TransferSummary, AppError> {
    let mut receiver = ReceiverWindow::new();
    let mut started_at = Instant::now();
    let mut file: Option<BufWriter<File>> = None;
    let mut file_name = String::from("received_file");
    let mut file_size = 0u64;
    let mut sender_name: Option<String> = None;
    let mut bytes_received = 0u64;
    let mut resumed_bytes_at_start = 0u64;
    let mut checkpoint: Option<ResumeCheckpoint> = None;
    let mut checkpoint_path: Option<PathBuf> = None;
    let mut peak_receiver_buffered = 0usize;
    let mut last_progress_emit = Instant::now() - PROGRESS_EMIT_INTERVAL;
    let mut last_checkpoint_save = Instant::now() - CHECKPOINT_SAVE_INTERVAL;
    let mut bytes_since_checkpoint = 0u64;
    let mut progress_dirty = false;

    // Streaming mode handshake state: `stream_offered` is set once we advertise
    // the capability (LAN/TCP only, during Start handling); `streaming` flips
    // when the sender's confirmation Resume arrives, which TCP ordering
    // guarantees happens before the first Data packet of the transfer.
    let mut stream_offered = false;
    let mut streaming = false;
    let mut bytes_since_stream_ack = 0u64;
    let mut finish_received = false;

    let mut diag_data_recv: u64 = 0;
    let mut diag_acks_sent: u64 = 0;
    let mut diag_frames: u64 = 0;
    let mut diag_last = now_ms();
    let mut last_frame_at = Instant::now();
    macro_rules! receive_try {
        ($operation:expr) => {
            match $operation {
                Ok(value) => value,
                Err(error) => {
                    let error: AppError = error.into();
                    let checkpoint_saved = save_receive_failure_checkpoint(
                        &mut file,
                        &mut checkpoint,
                        checkpoint_path.as_deref(),
                        receiver.next_expected(),
                        bytes_received,
                    );
                    sink.emit(PlenumEvent::Log {
                        level: LogLevel::Error,
                        message: format!(
                            "DIAG recv failure: bytes_received={bytes_received} total_bytes={file_size} last_sequence={} elapsed_since_frame_ms={} checkpoint_saved={checkpoint_saved} category={} error={error}",
                            receiver.next_expected().saturating_sub(1),
                            last_frame_at.elapsed().as_millis(),
                            error_category(&error),
                        ),
                    });
                    emit_failed(sink, TransferDirection::Receive, &error, checkpoint_saved);
                    return Err(error);
                }
            }
        };
    }
    macro_rules! receive_send {
        ($packet:expr) => {{
            let bytes = receive_try!(encode_packet(&$packet));
            receive_try!(transport.send(&bytes));
        }};
    }
    sink.emit(PlenumEvent::Transfer(
        TransferEvent::ConnectionEstablished {
            direction: TransferDirection::Receive,
            mode: transfer_mode(transport),
        },
    ));
    sink.emit(PlenumEvent::Log {
        level: LogLevel::Info,
        message: "DIAG recv: transfer loop start, waiting for packets".to_string(),
    });

    loop {
        if control.is_cancelled() {
            let _ = save_receive_failure_checkpoint(
                &mut file,
                &mut checkpoint,
                checkpoint_path.as_deref(),
                receiver.next_expected(),
                bytes_received,
            );
            if let Ok(bytes) = encode_packet(&Packet::new(
                PacketType::Close,
                receiver.next_expected(),
                CLOSE_REASON_CANCELLED.as_bytes().to_vec(),
            )) {
                let _ = transport.send(&bytes);
            }
            let _ = transport.close();
            sink.emit(PlenumEvent::Transfer(TransferEvent::Cancelled {
                direction: TransferDirection::Receive,
            }));
            return Err(AppError::Cancelled);
        }

        let diag_now = now_ms();
        if diag_now.saturating_sub(diag_last) >= 1000 {
            diag_last = diag_now;
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: format!(
                    "DIAG recv: frames={diag_frames} data_recv={diag_data_recv} acks_sent={diag_acks_sent} bytes_recv={bytes_received} next_expected={}",
                    receiver.next_expected()
                ),
            });
        }

        for diag in transport.poll_diagnostics() {
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: diag,
            });
        }

        let frame = match transport.recv() {
            Ok(Some(frame)) => {
                last_frame_at = Instant::now();
                frame
            }
            Ok(None) => {
                if last_frame_at.elapsed() >= RECEIVE_STALL_TIMEOUT {
                    let checkpoint_saved = save_receive_failure_checkpoint(
                        &mut file,
                        &mut checkpoint,
                        checkpoint_path.as_deref(),
                        receiver.next_expected(),
                        bytes_received,
                    );
                    let error = AppError::Stalled(format!(
                        "no packets from sender for {}s",
                        RECEIVE_STALL_TIMEOUT.as_secs()
                    ));
                    emit_failed(sink, TransferDirection::Receive, &error, checkpoint_saved);
                    return Err(error);
                }
                thread::sleep(IDLE_POLL_INTERVAL);
                continue;
            }
            Err(error) => {
                let error = AppError::from(error);
                let checkpoint_saved = save_receive_failure_checkpoint(
                    &mut file,
                    &mut checkpoint,
                    checkpoint_path.as_deref(),
                    receiver.next_expected(),
                    bytes_received,
                );
                sink.emit(PlenumEvent::Log {
                    level: LogLevel::Error,
                    message: format!(
                        "DIAG recv failure: bytes_received={bytes_received} total_bytes={file_size} last_sequence={} elapsed_since_frame_ms={} checkpoint_saved={checkpoint_saved} category={} error={error}",
                        receiver.next_expected().saturating_sub(1),
                        last_frame_at.elapsed().as_millis(),
                        error_category(&error),
                    ),
                });
                emit_failed(sink, TransferDirection::Receive, &error, checkpoint_saved);
                return Err(error);
            }
        };
        diag_frames = diag_frames.saturating_add(1);

        let packet = receive_try!(parse_packet(&frame));
        match packet.packet_type {
            PacketType::Start => {
                let (parsed_size, parsed_name, parsed_sender) =
                    receive_try!(parse_start_payload(&packet.payload));
                file_size = parsed_size;
                file_name = parsed_name;
                sender_name = parsed_sender;
                let clean_name = Path::new(&file_name)
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("received_file"))
                    .to_string_lossy()
                    .to_string();
                let out_path = output_dir.join(&clean_name);
                let cp_path = resume_checkpoint_path(&out_path);
                let (resume_sequence, resume_bytes, open_file, cp) =
                    receive_try!(prepare_resume_state(
                        &out_path,
                        &cp_path,
                        &clean_name,
                        file_size,
                        options.chunk_size,
                    ));
                resumed_bytes_at_start = resume_bytes;

                file = Some(BufWriter::with_capacity(1024 * 1024, open_file));
                checkpoint = Some(cp);
                checkpoint_path = Some(cp_path.clone());
                receiver = ReceiverWindow::with_next_expected(resume_sequence);
                bytes_received = resume_bytes;
                bytes_since_checkpoint = 0;
                last_checkpoint_save = Instant::now();
                file_name = clean_name;

                sink.emit(PlenumEvent::Log {
                    level: LogLevel::Info,
                    message: format!(
                        "DIAG recv: START received file={file_name} size={file_size} resume={resume_bytes}"
                    ),
                });

                sink.emit(PlenumEvent::Transfer(TransferEvent::IncomingRequest {
                    direction: TransferDirection::Receive,
                    file_name: file_name.clone(),
                    total_bytes: file_size,
                    peer: Some(peer_label.clone()),
                    sender_name: sender_name.clone(),
                }));
                if !auto_accept {
                    let deadline = Instant::now() + APPROVAL_TIMEOUT;
                    loop {
                        if control.is_cancelled() {
                            let _ = save_receive_failure_checkpoint(
                                &mut file,
                                &mut checkpoint,
                                checkpoint_path.as_deref(),
                                receiver.next_expected(),
                                bytes_received,
                            );
                            let _ = transport.send(&encode_packet(&Packet::new(
                                PacketType::Close,
                                resume_sequence,
                                CLOSE_REASON_CANCELLED.as_bytes().to_vec(),
                            ))?);
                            let _ = transport.close();
                            sink.emit(PlenumEvent::Transfer(TransferEvent::Cancelled {
                                direction: TransferDirection::Receive,
                            }));
                            return Err(AppError::Cancelled);
                        }
                        match control.decision() {
                            AcceptDecision::Accepted => break,
                            AcceptDecision::Declined => {
                                let _ = save_receive_failure_checkpoint(
                                    &mut file,
                                    &mut checkpoint,
                                    checkpoint_path.as_deref(),
                                    receiver.next_expected(),
                                    bytes_received,
                                );
                                let _ = transport.send(&encode_packet(&Packet::new(
                                    PacketType::Close,
                                    resume_sequence,
                                    CLOSE_REASON_DECLINED.as_bytes().to_vec(),
                                ))?);
                                let _ = transport.close();
                                sink.emit(PlenumEvent::Transfer(TransferEvent::Declined {
                                    direction: TransferDirection::Receive,
                                    reason: CLOSE_REASON_DECLINED.to_string(),
                                }));
                                return Err(AppError::Rejected("transfer declined".to_string()));
                            }
                            AcceptDecision::Pending => {
                                if Instant::now() >= deadline {
                                    let _ = save_receive_failure_checkpoint(
                                        &mut file,
                                        &mut checkpoint,
                                        checkpoint_path.as_deref(),
                                        receiver.next_expected(),
                                        bytes_received,
                                    );
                                    let _ = transport.send(&encode_packet(&Packet::new(
                                        PacketType::Close,
                                        resume_sequence,
                                        CLOSE_REASON_DECLINED.as_bytes().to_vec(),
                                    ))?);
                                    let _ = transport.close();
                                    sink.emit(PlenumEvent::Transfer(TransferEvent::Declined {
                                        direction: TransferDirection::Receive,
                                        reason: CLOSE_REASON_DECLINED.to_string(),
                                    }));
                                    return Err(AppError::Rejected(format!(
                                        "no decision within {}s; transfer declined",
                                        APPROVAL_TIMEOUT.as_secs()
                                    )));
                                }
                                thread::sleep(Duration::from_millis(50));
                            }
                        }
                    }
                    control.reset_decision();
                }

                // Offer streaming mode on the TCP/LAN path. Must be sent
                // before any real Resume packet: an old sender zeroes its
                // resume offset on any non-8-byte Resume payload, so the real
                // 8-byte Resume below has to arrive after this one to win.
                if transport.is_relayed().is_none() {
                    stream_offered = true;
                    receive_send!(Packet::new(
                        PacketType::Resume,
                        0,
                        STREAM_MODE_MAGIC.to_vec(),
                    ));
                }

                if resume_bytes > 0 {
                    sink.emit(PlenumEvent::Transfer(TransferEvent::Resumed {
                        direction: TransferDirection::Receive,
                        next_sequence: resume_sequence,
                        resumed_bytes: resume_bytes,
                    }));
                    receive_send!(Packet::new(
                        PacketType::Resume,
                        resume_sequence,
                        resume_bytes.to_be_bytes().to_vec(),
                    ));
                }

                receive_send!(Packet::new(
                    PacketType::Accept,
                    resume_sequence,
                    device_name
                        .map(|name| name.as_bytes().to_vec())
                        .unwrap_or_default(),
                ));

                started_at = Instant::now();

                sink.emit(PlenumEvent::Transfer(TransferEvent::Started {
                    direction: TransferDirection::Receive,
                    file_name: file_name.clone(),
                    total_bytes: file_size,
                    resumed_bytes: resume_bytes,
                }));

                last_frame_at = Instant::now();
            }
            PacketType::Data => {
                diag_data_recv = diag_data_recv.saturating_add(1);
                let controls = receive_try!(receiver.receive_data_packet(packet));
                if !streaming {
                    for control in controls {
                        if control.packet_type == PacketType::Ack {
                            diag_acks_sent = diag_acks_sent.saturating_add(1);
                        }
                        receive_send!(control);
                    }
                }

                peak_receiver_buffered =
                    peak_receiver_buffered.max(receiver.buffered_payload_bytes());
                let drained = receiver.drain_ordered_packets();
                if !drained.is_empty() {
                    let mut batch_bytes = 0u64;
                    for (_, payload) in drained {
                        batch_bytes = batch_bytes.saturating_add(payload.len() as u64);
                        if let Some(file) = file.as_mut() {
                            receive_try!(file.write_all(&payload));
                        }
                    }
                    bytes_received = bytes_received.saturating_add(batch_bytes);
                    bytes_since_checkpoint = bytes_since_checkpoint.saturating_add(batch_bytes);
                    progress_dirty = true;

                    // Streaming mode: one cumulative ACK per interval. Its
                    // sequence number acknowledges everything delivered so
                    // far (next_expected - 1 is the last contiguous packet).
                    if streaming {
                        bytes_since_stream_ack = bytes_since_stream_ack.saturating_add(batch_bytes);
                        if bytes_since_stream_ack >= STREAM_ACK_INTERVAL_BYTES {
                            bytes_since_stream_ack = 0;
                            diag_acks_sent = diag_acks_sent.saturating_add(1);
                            receive_send!(Packet::new(
                                PacketType::Ack,
                                receiver.next_expected().saturating_sub(1),
                                Vec::new(),
                            ));
                        }
                    }

                    if let Some(cp) = checkpoint.as_mut() {
                        cp.update(receiver.next_expected(), bytes_received);
                        let due = bytes_since_checkpoint >= CHECKPOINT_SAVE_BYTES
                            || last_checkpoint_save.elapsed() >= CHECKPOINT_SAVE_INTERVAL;
                        if due {
                            if let Some(file) = file.as_mut() {
                                receive_try!(file.flush());
                            }
                            if let Some(path) = checkpoint_path.as_ref() {
                                match cp.save(path) {
                                    Ok(()) => sink.emit(PlenumEvent::Transfer(
                                        TransferEvent::CheckpointUpdated {
                                            checkpoint_path: path.clone(),
                                            next_sequence: cp.next_sequence,
                                            bytes_written: cp.bytes_written,
                                        },
                                    )),
                                    Err(error) => sink.emit(PlenumEvent::Log {
                                        level: LogLevel::Warn,
                                        message: format!(
                                            "DIAG recv checkpoint warning: bytes_received={bytes_received} next_sequence={} error={error}; transfer continues without updating resume metadata",
                                            receiver.next_expected()
                                        ),
                                    }),
                                }
                            }
                            bytes_since_checkpoint = 0;
                            last_checkpoint_save = Instant::now();
                        }
                    }

                    if last_progress_emit.elapsed() >= PROGRESS_EMIT_INTERVAL
                        || bytes_received >= file_size
                    {
                        progress_dirty = false;
                        last_progress_emit = Instant::now();
                        sink.emit(PlenumEvent::Transfer(TransferEvent::Progress {
                            direction: TransferDirection::Receive,
                            transferred_bytes: bytes_received.min(file_size),
                            total_bytes: file_size,
                        }));
                    }
                }
            }
            PacketType::Finish => {
                if let Some(file) = file.as_mut() {
                    receive_try!(file.flush());
                }
                // Streaming mode: the sender holds its connection open until
                // every byte is acknowledged, and the file tail is usually
                // shorter than the ACK interval — always emit the final
                // cumulative ACK here.
                if streaming && receiver.next_expected() > 0 {
                    diag_acks_sent = diag_acks_sent.saturating_add(1);
                    receive_send!(Packet::new(
                        PacketType::Ack,
                        receiver.next_expected() - 1,
                        Vec::new(),
                    ));
                }
                if progress_dirty {
                    sink.emit(PlenumEvent::Transfer(TransferEvent::Progress {
                        direction: TransferDirection::Receive,
                        transferred_bytes: bytes_received.min(file_size),
                        total_bytes: file_size,
                    }));
                }
                if let Some(path) = checkpoint_path.as_ref() {
                    receive_try!(ResumeCheckpoint::clear(path));
                }
                finish_received = true;
                break;
            }
            PacketType::Close => {
                let reason = String::from_utf8_lossy(&packet.payload).into_owned();
                if reason.is_empty() {
                    break;
                }
                let _ = save_receive_failure_checkpoint(
                    &mut file,
                    &mut checkpoint,
                    checkpoint_path.as_deref(),
                    receiver.next_expected(),
                    bytes_received,
                );
                if reason == CLOSE_REASON_CANCELLED {
                    sink.emit(PlenumEvent::Transfer(TransferEvent::Cancelled {
                        direction: TransferDirection::Receive,
                    }));
                    sink.emit(PlenumEvent::Log {
                        level: LogLevel::Info,
                        message: format!(
                            "DIAG recv cancelled by sender: bytes_received={bytes_received}"
                        ),
                    });
                    return Err(AppError::Cancelled);
                } else {
                    sink.emit(PlenumEvent::Transfer(TransferEvent::Declined {
                        direction: TransferDirection::Receive,
                        reason: reason.clone(),
                    }));
                    sink.emit(PlenumEvent::Log {
                        level: LogLevel::Info,
                        message: format!("DIAG recv declined by sender: reason={reason}"),
                    });
                    return Err(AppError::Rejected(rejection_message(&reason)));
                }
            }
            PacketType::Resume => {
                // Sender's confirmation of the streaming-mode offer. Anything
                // else in a mid-transfer Resume is ignored, as it always was.
                if stream_offered && !streaming && packet.payload == STREAM_MODE_MAGIC {
                    streaming = true;
                    sink.emit(PlenumEvent::Log {
                        level: LogLevel::Info,
                        message: "DIAG recv: streaming mode enabled (cumulative ACKs over TCP)"
                            .to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    sink.emit(PlenumEvent::Log {
        level: LogLevel::Info,
        message: format!(
            "DIAG recv: transfer loop END frames={diag_frames} data_recv={diag_data_recv} acks_sent={diag_acks_sent} bytes_recv={bytes_received}"
        ),
    });

    if !finish_received && bytes_received < file_size {
        let _ = transport.close();
        let checkpoint_saved = save_receive_failure_checkpoint(
            &mut file,
            &mut checkpoint,
            checkpoint_path.as_deref(),
            receiver.next_expected(),
            bytes_received,
        );
        let error = AppError::Stalled(format!(
            "connection closed before transfer completed ({} of {} bytes received)",
            bytes_received, file_size
        ));
        sink.emit(PlenumEvent::Log {
            level: LogLevel::Error,
            message: format!(
                "DIAG recv EOF: bytes_received={bytes_received} total_bytes={file_size} last_sequence={} elapsed_since_frame_ms={} checkpoint_saved={checkpoint_saved} category={}",
                receiver.next_expected().saturating_sub(1),
                last_frame_at.elapsed().as_millis(),
                error_category(&error),
            ),
        });
        emit_failed(sink, TransferDirection::Receive, &error, checkpoint_saved);
        return Err(error);
    }

    let mode = transfer_mode(transport);
    let _ = transport.close();
    let summary = TransferSummary {
        direction: TransferDirection::Receive,
        file_name,
        peer: Some(peer_label.clone()),
        peer_name: sender_name,
        mode,
        total_bytes: file_size,
        transferred_bytes: bytes_received,
        resumed_bytes: resumed_bytes_at_start.min(bytes_received),
        elapsed_ms: started_at.elapsed().as_millis(),
    };
    let _ = peak_receiver_buffered;
    sink.emit(PlenumEvent::Transfer(TransferEvent::Completed(
        summary.clone(),
    )));
    sink.emit(PlenumEvent::Transfer(TransferEvent::StateChanged {
        direction: TransferDirection::Receive,
        state: ConnectionState::Closed,
        peer: Some(peer_label),
    }));
    Ok(summary)
}

/// Why the PIN gate turned a connection away (or failed outright).
fn wait_for_accept<T: Transport, S: EventSink>(
    transport: &mut T,
    control: &SessionControl,
    sink: &mut S,
    direction: TransferDirection,
) -> Result<(u32, u64, Option<String>, bool), AppError> {
    let deadline = Instant::now() + ACCEPT_WAIT_TIMEOUT;
    let mut resume_bytes = 0u64;
    let mut receiver_streams = false;
    loop {
        if control.is_cancelled() {
            let _ = transport.send(&encode_packet(&Packet::new(
                PacketType::Close,
                0,
                CLOSE_REASON_CANCELLED.as_bytes().to_vec(),
            ))?);
            let _ = transport.close();
            sink.emit(PlenumEvent::Transfer(TransferEvent::Cancelled {
                direction,
            }));
            return Err(AppError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(AppError::Stalled(format!(
                "receiver did not answer the transfer offer within {}s",
                ACCEPT_WAIT_TIMEOUT.as_secs()
            )));
        }
        match transport.recv()? {
            Some(frame) => {
                let packet = parse_packet(&frame)?;
                match packet.packet_type {
                    PacketType::Accept => {
                        return Ok((
                            packet.sequence_no,
                            resume_bytes,
                            parse_accept_payload(&packet.payload),
                            receiver_streams,
                        ));
                    }
                    PacketType::Resume => {
                        if packet.payload == STREAM_MODE_MAGIC {
                            receiver_streams = true;
                        } else {
                            resume_bytes = if packet.payload.len() == 8 {
                                let mut bytes = [0u8; 8];
                                bytes.copy_from_slice(&packet.payload);
                                u64::from_be_bytes(bytes)
                            } else {
                                0
                            };
                        }
                    }
                    // The receiver declined, rejected the PIN, or cancelled.
                    PacketType::Close => {
                        let reason = String::from_utf8_lossy(&packet.payload).into_owned();
                        sink.emit(PlenumEvent::Transfer(TransferEvent::Declined {
                            direction,
                            reason: reason.clone(),
                        }));
                        return Err(AppError::Rejected(rejection_message(&reason)));
                    }
                    _ => {}
                }
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Turns a machine-readable `Close` reason into a human-readable message.
fn rejection_message(reason: &str) -> String {
    match reason {
        CLOSE_REASON_DECLINED => "the receiver declined the transfer".to_string(),
        CLOSE_REASON_PIN_REJECTED => "the receiver rejected the pairing code".to_string(),
        CLOSE_REASON_CANCELLED => "the peer cancelled the transfer".to_string(),
        other => format!("the peer closed the connection ({other})"),
    }
}

/// Ensures the ICE server list carries a TURN entry before a remote transfer.
fn has_turn_server(ice_servers: &[IceServer]) -> bool {
    ice_servers.iter().any(|server| {
        server
            .urls
            .iter()
            .any(|u| u.starts_with("turn:") || u.starts_with("turns:"))
    })
}

fn should_retry_via_relay(error: &AppError, ice_servers: &[IceServer]) -> bool {
    let retryable = matches!(
        error,
        // ICE/DTLS timed out after offer/answer — a forced-relay retry may
        // unblock an asymmetrically-blocked path.
        AppError::Rtc(crate::rtc::RtcError::Timeout)
            // Stats poller detected the active pair went dead mid-transfer.
            | AppError::Transport(TransportError::DeadPath) // RtcError::PeerNeverArrived is intentionally excluded: the remote
                                                            // peer never joined the signaling session so the ICE handshake never
                                                            // started; forcing relay-only candidates cannot help.
    );
    retryable && has_turn_server(ice_servers)
}

fn ensure_turn_server<S: EventSink>(
    ice_servers: &mut Vec<IceServer>,
    relay_server_url: &str,
    my_peer_id: &str,
    sink: &mut S,
) {
    if has_turn_server(ice_servers) {
        return;
    }
    match crate::rtc::turn::fetch_turn_credentials_blocking(relay_server_url, my_peer_id) {
        Some(server) => {
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Info,
                message: "DIAG rtc: no TURN server in request; fetched credentials via core fallback path"
                    .to_string(),
            });
            ice_servers.push(server);
        }
        None => {
            sink.emit(PlenumEvent::Log {
                level: LogLevel::Warn,
                message: "DIAG rtc: no TURN server available (fetch failed); proceeding STUN-only"
                    .to_string(),
            });
        }
    }
}

/// Maps an RTC connect failure to an app error, emitting the cancelled state
fn rtc_connect_error<S: EventSink>(
    error: crate::rtc::RtcError,
    direction: TransferDirection,
    sink: &mut S,
) -> AppError {
    if matches!(error, crate::rtc::RtcError::Cancelled) {
        sink.emit(PlenumEvent::Transfer(TransferEvent::Cancelled {
            direction,
        }));
        AppError::Cancelled
    } else {
        AppError::Rtc(error)
    }
}

fn resume_checkpoint_path(out_path: &Path) -> PathBuf {
    let file_name = out_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("received_file"))
        .to_string_lossy();
    out_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{}.plenum.resume.json", file_name))
}

fn prepare_resume_state(
    out_path: &Path,
    checkpoint_path: &Path,
    file_name: &str,
    file_size: u64,
    chunk_size: usize,
) -> Result<(u32, u64, File, ResumeCheckpoint), AppError> {
    if checkpoint_path.exists() {
        let checkpoint = ResumeCheckpoint::load(checkpoint_path)?;
        if checkpoint.matches(file_name, file_size, chunk_size)
            && checkpoint.bytes_written <= file_size
            && checkpoint.next_sequence
                <= file_size
                    .saturating_add(chunk_size.saturating_sub(1) as u64)
                    .checked_div(chunk_size as u64)
                    .unwrap_or(u64::MAX) as u32
        {
            let mut file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(out_path)?;
            let existing_len = file.metadata()?.len();
            if existing_len >= checkpoint.bytes_written {
                // Data written after the last durable checkpoint is not covered
                // by the sender's resume offset; remove it before appending.
                file.set_len(checkpoint.bytes_written)?;
                file.seek(SeekFrom::Start(checkpoint.bytes_written))?;
                return Ok((
                    checkpoint.next_sequence,
                    checkpoint.bytes_written,
                    file,
                    checkpoint,
                ));
            }
        }

        ResumeCheckpoint::clear(checkpoint_path)?;
    }

    let file = File::create(out_path)?;
    let checkpoint = ResumeCheckpoint::new(file_name.to_string(), file_size, chunk_size);
    checkpoint.save(checkpoint_path)?;
    Ok((0, 0, file, checkpoint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportResult;
    use std::collections::VecDeque;

    struct AckingBackpressureTransport {
        acknowledgements: VecDeque<Vec<u8>>,
        ack_threshold: u64,
        pending_bytes: u64,
        pending_frames: usize,
        max_pending_bytes: u64,
        max_pending_frames: usize,
        last_data_sequence: u32,
        data_frames: usize,
    }

    impl AckingBackpressureTransport {
        fn new(ack_threshold: u64) -> Self {
            Self {
                acknowledgements: VecDeque::new(),
                ack_threshold,
                pending_bytes: 0,
                pending_frames: 0,
                max_pending_bytes: 0,
                max_pending_frames: 0,
                last_data_sequence: 0,
                data_frames: 0,
            }
        }

        fn queue_ack(&mut self) -> TransportResult<()> {
            self.acknowledgements.push_back(
                encode_packet(&Packet::new(
                    PacketType::Ack,
                    self.last_data_sequence,
                    Vec::new(),
                ))
                .map_err(|error| TransportError::Io {
                    operation: "encode test ACK",
                    kind: std::io::ErrorKind::InvalidData,
                    message: error.to_string(),
                })?,
            );
            Ok(())
        }
    }

    impl Transport for AckingBackpressureTransport {
        fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
            let packet = parse_packet(bytes).map_err(|error| TransportError::Io {
                operation: "parse test packet",
                kind: std::io::ErrorKind::InvalidData,
                message: error.to_string(),
            })?;
            match packet.packet_type {
                PacketType::Data => {
                    self.last_data_sequence = packet.sequence_no;
                    self.data_frames += 1;
                    self.pending_bytes = self
                        .pending_bytes
                        .saturating_add(packet.payload.len() as u64);
                    self.pending_frames += 1;
                    self.max_pending_bytes = self.max_pending_bytes.max(self.pending_bytes);
                    self.max_pending_frames = self.max_pending_frames.max(self.pending_frames);
                    if self.pending_bytes >= self.ack_threshold && self.acknowledgements.is_empty()
                    {
                        self.queue_ack()?;
                    }
                }
                PacketType::Finish if self.pending_bytes > 0 => self.queue_ack()?,
                _ => {}
            }
            Ok(())
        }

        fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
            let acknowledgement = self.acknowledgements.pop_front();
            if acknowledgement.is_some() {
                self.pending_bytes = 0;
                self.pending_frames = 0;
            }
            Ok(acknowledgement)
        }

        fn close(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn is_closed(&self) -> bool {
            false
        }
    }

    #[test]
    fn streaming_sender_bounds_unacknowledged_bytes_and_tracking() {
        let chunk_size = 256 * 1024;
        let window_size = 4;
        let max_in_flight = (chunk_size * window_size) as u64;
        let file_size = 5 * 1024 * 1024;
        let path = std::env::temp_dir().join(format!(
            "plenum-backpressure-{}-{}.bin",
            std::process::id(),
            now_ms()
        ));
        let source = File::create(&path).unwrap();
        source.set_len(file_size).unwrap();
        drop(source);
        let mut file = File::open(&path).unwrap();

        let mut transport = AckingBackpressureTransport::new(max_in_flight);
        let mut events = Vec::new();
        let mut sink = |event| events.push(event);
        let options = crate::app::types::TransferOptions {
            chunk_size,
            window_size,
            timeout_ticks: 15_000,
        };

        run_streaming_send_loop(
            &mut transport,
            &mut sink,
            &mut file,
            file_size,
            &options,
            0,
            0,
            &SessionControl::default(),
        )
        .unwrap();

        assert_eq!(transport.data_frames, file_size as usize / chunk_size);
        assert!(transport.max_pending_bytes <= max_in_flight);
        assert!(transport.max_pending_frames <= window_size);
        assert!(events.iter().any(|event| matches!(
            event,
            PlenumEvent::Transfer(TransferEvent::Progress {
                transferred_bytes,
                total_bytes,
                ..
            }) if *transferred_bytes == file_size && *total_bytes == file_size
        )));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resume_truncates_data_after_last_checkpoint() {
        let unique = format!("{}-{}", std::process::id(), now_ms());
        let output = std::env::temp_dir().join(format!("plenum-resume-{unique}.bin"));
        let checkpoint_path = std::env::temp_dir().join(format!("plenum-resume-{unique}.json"));
        std::fs::write(&output, vec![0_u8; 12]).unwrap();
        let mut checkpoint = ResumeCheckpoint::new("file.bin", 32, 4);
        checkpoint.update(2, 8);
        checkpoint.save(&checkpoint_path).unwrap();

        let (sequence, bytes, file, _) =
            prepare_resume_state(&output, &checkpoint_path, "file.bin", 32, 4).unwrap();
        assert_eq!((sequence, bytes), (2, 8));
        assert_eq!(file.metadata().unwrap().len(), 8);

        let _ = std::fs::remove_file(output);
        let _ = std::fs::remove_file(checkpoint_path);
    }

    #[test]
    fn start_payload_legacy_format_parses() {
        // Old sender: [8B size][fname], no sentinel.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1234u64.to_be_bytes());
        payload.extend_from_slice(b"photo.jpg");

        let (size, name, sender) = parse_start_payload(&payload).unwrap();
        assert_eq!(size, 1234);
        assert_eq!(name, "photo.jpg");
        assert_eq!(sender, None);
    }

    #[test]
    fn start_payload_versioned_roundtrip() {
        let payload = encode_start_payload(987_654_321, "video.mp4", Some("Atharva's Pixel"));
        assert_eq!(payload[0], START_PAYLOAD_V2_SENTINEL);

        let (size, name, sender) = parse_start_payload(&payload).unwrap();
        assert_eq!(size, 987_654_321);
        assert_eq!(name, "video.mp4");
        assert_eq!(sender.as_deref(), Some("Atharva's Pixel"));
    }

    #[test]
    fn start_payload_without_device_name_uses_legacy_format() {
        // No device name -> legacy layout, so old receivers keep working.
        let payload = encode_start_payload(42, "doc.pdf", None);
        assert_ne!(payload[0], START_PAYLOAD_V2_SENTINEL);

        let (size, name, sender) = parse_start_payload(&payload).unwrap();
        assert_eq!(size, 42);
        assert_eq!(name, "doc.pdf");
        assert_eq!(sender, None);
    }

    #[test]
    fn start_payload_versioned_empty_device_name_is_none() {
        let payload = encode_start_payload(42, "doc.pdf", Some(""));
        let (_, _, sender) = parse_start_payload(&payload).unwrap();
        assert_eq!(sender, None);
    }

    #[test]
    fn start_payload_truncated_versioned_is_rejected() {
        let mut payload = encode_start_payload(42, "doc.pdf", Some("Laptop"));
        payload.truncate(10);
        assert!(parse_start_payload(&payload).is_err());

        // Name length claims more bytes than present.
        let mut bad = vec![START_PAYLOAD_V2_SENTINEL];
        bad.extend_from_slice(&42u64.to_be_bytes());
        bad.extend_from_slice(&100u16.to_be_bytes());
        bad.extend_from_slice(b"short");
        assert!(parse_start_payload(&bad).is_err());
    }

    #[test]
    fn stream_magic_never_parses_as_resume_offset() {
        // Old senders treat any 8-byte Resume payload as a resume offset and
        // zero it otherwise; the capability marker must never be 8 bytes or a
        // legacy peer would seek mid-file.
        assert_ne!(STREAM_MODE_MAGIC.len(), 8);
        // And it must differ from every real resume payload by construction:
        // real payloads are exactly 8 bytes (u64::to_be_bytes).
        assert!(STREAM_MODE_MAGIC.len() > 8);
    }

    #[test]
    fn accept_payload_roundtrip() {
        assert_eq!(parse_accept_payload(b""), None);
        assert_eq!(
            parse_accept_payload("MacBook Pro".as_bytes()),
            Some("MacBook Pro".to_string())
        );
    }

    #[test]
    fn relay_retry_only_for_dead_path_or_timeout_with_turn_available() {
        let stun_only = vec![IceServer::new(vec!["stun:stun.example.com:3478".into()])];
        let with_turn = vec![
            IceServer::new(vec!["stun:stun.example.com:3478".into()]),
            IceServer::with_credentials(
                vec!["turn:turn.example.com:3478?transport=udp".into()],
                "user",
                "secret",
            ),
        ];

        let dead_path = AppError::Transport(TransportError::DeadPath);
        let timeout = AppError::Rtc(crate::rtc::RtcError::Timeout);
        let peer_never_arrived = AppError::Rtc(crate::rtc::RtcError::PeerNeverArrived);
        let cancelled = AppError::Cancelled;
        let rejected = AppError::Rejected("declined".into());
        let closed = AppError::Transport(TransportError::Closed);

        assert!(should_retry_via_relay(&dead_path, &with_turn));
        assert!(should_retry_via_relay(&timeout, &with_turn));

        // No TURN server: forcing relay-only ICE cannot produce candidates.
        assert!(!should_retry_via_relay(&dead_path, &stun_only));
        assert!(!should_retry_via_relay(&timeout, &stun_only));

        // Non-path failures must never trigger a retry.
        assert!(!should_retry_via_relay(&cancelled, &with_turn));
        assert!(!should_retry_via_relay(&rejected, &with_turn));
        // PeerNeverArrived: remote never joined signaling; forcing relay again is useless
        assert!(!should_retry_via_relay(&peer_never_arrived, &with_turn));
        assert!(!should_retry_via_relay(&closed, &with_turn));
    }

    #[test]
    fn failure_events_preserve_terminal_outcomes_and_resume_message() {
        let mut events = Vec::new();
        let network_error =
            AppError::Transport(TransportError::ConnectionReset { operation: "read" });

        emit_failed(
            &mut |event| events.push(event),
            TransferDirection::Receive,
            &network_error,
            true,
        );
        emit_failed(
            &mut |event| events.push(event),
            TransferDirection::Receive,
            &AppError::Cancelled,
            true,
        );
        emit_failed(
            &mut |event| events.push(event),
            TransferDirection::Receive,
            &AppError::Rejected("declined".to_string()),
            true,
        );

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            PlenumEvent::Transfer(TransferEvent::Failed {
                message,
                recoverable: true,
                ..
            }) if message == "Connection lost. Your partial transfer was saved and can be resumed."
        ));
    }
}
