//! WebSocket signaling client: connects to the relay server, exchanges
//! `SignalMessage` JSON frames, and drives a single `RTCPeerConnection` through
//! offer/answer/ICE-candidate negotiation up to an open data channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::ice::candidate::CandidateType;
use webrtc::ice::network_type::NetworkType;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::stats::StatsReportType;

use crate::rtc::error::RtcError;
use crate::signaling::{IceServer, SignalMessage};

const SIGNALING_LINGER: Duration = Duration::from_secs(15);

const WS_CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_ws_with_fallback(
    relay_url: &str,
    diag_tx: &std::sync::mpsc::Sender<String>,
    role: &'static str,
) -> Result<(WsStream, bool), RtcError> {
    let direct_error = match tokio::time::timeout(
        WS_CONNECT_ATTEMPT_TIMEOUT,
        tokio_tungstenite::connect_async(relay_url),
    )
    .await
    {
        Ok(Ok((ws_stream, _response))) => return Ok((ws_stream, false)),
        Ok(Err(error)) => error.to_string(),
        Err(_) => format!(
            "timed out after {}s",
            WS_CONNECT_ATTEMPT_TIMEOUT.as_secs()
        ),
    };

    if !crate::rtc::resolve::fallback_applies(relay_url) {
        return Err(RtcError::WebSocket(direct_error));
    }

    let _ = diag_tx.send(format!(
        "DIAG {role}: relay connect via DNS failed ({direct_error}); retrying via fallback IP {}",
        crate::rtc::resolve::RELAY_FALLBACK_IP
    ));

    let parsed = url::Url::parse(relay_url)
        .map_err(|error| RtcError::WebSocket(format!("invalid relay url: {error}")))?;
    let port = parsed.port().unwrap_or(match parsed.scheme() {
        "wss" | "https" => 443,
        _ => 80,
    });

    let tcp = tokio::time::timeout(
        WS_CONNECT_ATTEMPT_TIMEOUT,
        tokio::net::TcpStream::connect((crate::rtc::resolve::RELAY_FALLBACK_IP, port)),
    )
    .await
    .map_err(|_| {
        RtcError::WebSocket(format!(
            "relay connect failed via DNS ({direct_error}) and fallback IP TCP connect timed out"
        ))
    })?
    .map_err(|error| {
        RtcError::WebSocket(format!(
            "relay connect failed via DNS ({direct_error}) and via fallback IP: {error}"
        ))
    })?;
    let _ = tcp.set_nodelay(true);

    let (ws_stream, _response) = tokio::time::timeout(
        WS_CONNECT_ATTEMPT_TIMEOUT,
        tokio_tungstenite::client_async_tls_with_config(relay_url, tcp, None, None),
    )
    .await
    .map_err(|_| RtcError::WebSocket("fallback IP TLS/WebSocket handshake timed out".into()))?
    .map_err(|error| {
        RtcError::WebSocket(format!("fallback IP TLS/WebSocket handshake failed: {error}"))
    })?;

    let _ = diag_tx.send(format!(
        "DIAG {role}: connected to relay via fallback IP (DNS sinkhole workaround active)"
    ));
    Ok((ws_stream, true))
}

pub struct ConnectedChannel {
    pub peer_connection: Arc<RTCPeerConnection>,
    pub data_channel: Arc<RTCDataChannel>,
    pub inbound_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    pub diag_tx: std::sync::mpsc::Sender<String>,
    pub diag_rx: std::sync::mpsc::Receiver<String>,
    /// True while the nominated ICE candidate pair goes through a TURN relay.
    /// Updated by the stats poller (ICE can re-nominate mid-transfer), read by
    /// `RtcTransport::is_relayed`.
    pub relayed: Arc<AtomicBool>,
    pub dead_pair: Arc<AtomicBool>,
}

fn build_api() -> Result<webrtc::api::API, RtcError> {
    let mut media_engine = MediaEngine::default();
    media_engine
        .register_default_codecs()
        .map_err(|error| RtcError::PeerConnection(error.to_string()))?;

    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut media_engine)
        .map_err(|error| RtcError::PeerConnection(error.to_string()))?;

    // IPv4 only. Mobile-carrier IPv6 (observed on Jio) passes the small
    // packets ICE and DTLS use for connectivity checks and handshakes but
    // blackholes full-size data packets (path-MTU/filtering). ICE then
    // nominates that poisoned host<->host IPv6 pair -- it "succeeded" -- and
    // the transfer stalls at 0% with the relay never consulted. Over IPv4 the
    // same phones fall back to srflx or the TURN relay, both of which carry
    // bulk data.
    let mut setting_engine = SettingEngine::default();
    setting_engine.set_network_types(vec![NetworkType::Udp4]);

    Ok(APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_setting_engine(setting_engine)
        .build())
}

/// Wire up the handlers shared by both roles once a data channel (local or
/// remote) exists: `on_open` signals readiness, `on_message` forwards bytes into
/// the std sync channel that `RtcTransport::recv()` polls.
fn wire_data_channel(
    data_channel: &Arc<RTCDataChannel>,
    inbound_tx: std::sync::mpsc::Sender<Vec<u8>>,
    open_tx: mpsc::UnboundedSender<()>,
) {
    let open_tx = open_tx.clone();
    data_channel.on_open(Box::new(move || {
        let _ = open_tx.send(());
        Box::pin(async {})
    }));

    data_channel.on_message(Box::new(move |msg: webrtc::data_channel::data_channel_message::DataChannelMessage| {
        let _ = inbound_tx.send(msg.data.to_vec());
        Box::pin(async {})
    }));
}

/// Flush ICE candidates that were received and buffered before the remote
/// description was set, applying them now that add_ice_candidate will accept
/// them. Drains the buffer.
async fn flush_pending_candidates(
    peer_connection: &Arc<RTCPeerConnection>,
    pending: &mut Vec<RTCIceCandidateInit>,
) -> Result<(), RtcError> {
    for init in pending.drain(..) {
        peer_connection
            .add_ice_candidate(init)
            .await
            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
    }
    Ok(())
}

async fn send_ws_message(
    ws_tx: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    message: &SignalMessage,
) -> Result<(), RtcError> {
    let json = message
        .to_json()
        .map_err(|error| RtcError::Signaling(error.to_string()))?;
    ws_tx
        .send(Message::Text(json))
        .await
        .map_err(|error| RtcError::WebSocket(error.to_string()))
}

fn spawn_outbound_task(
    mut ws_tx: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    mut outbound_rx: mpsc::UnboundedReceiver<SignalMessage>,
    mut ws_close_rx: oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                maybe_message = outbound_rx.recv() => {
                    match maybe_message {
                        Some(message) => {
                            if send_ws_message(&mut ws_tx, &message).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = &mut ws_close_rx => {
                    let _ = ws_tx.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });
}

fn spawn_signaling_linger(
    mut ws_rx: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    peer_connection: Arc<RTCPeerConnection>,
    diag_tx: std::sync::mpsc::Sender<String>,
    role: &'static str,
    ws_close_tx: oneshot::Sender<()>,
) {
    tokio::spawn(async move {
        let _ = diag_tx.send(format!(
            "DIAG {role}: data channel open; keeping signaling alive {}s for late ICE",
            SIGNALING_LINGER.as_secs()
        ));
        let deadline = tokio::time::Instant::now() + SIGNALING_LINGER;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                frame = ws_rx.next() => {
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(signal) = SignalMessage::from_json(&text) else {
                                continue;
                            };
                            if let SignalMessage::IceCandidate {
                                candidate,
                                sdp_mid,
                                sdp_mline_index,
                                ..
                            } = signal
                            {
                                let init = RTCIceCandidateInit {
                                    candidate,
                                    sdp_mid,
                                    sdp_mline_index,
                                    ..Default::default()
                                };
                                if let Err(error) = peer_connection.add_ice_candidate(init).await {
                                    let _ = diag_tx.send(format!(
                                        "DIAG {role}: linger add_ice_candidate failed: {error}"
                                    ));
                                }
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => break,
                    }
                }
            }
        }
        let _ = ws_close_tx.send(());
    });
}

fn wire_ice_candidate_outbound(
    peer_connection: &Arc<RTCPeerConnection>,
    outbound_tx: mpsc::UnboundedSender<SignalMessage>,
    session_id: String,
    my_peer_id: String,
    remote_peer_id: Arc<std::sync::Mutex<Option<String>>>,
) {
    peer_connection.on_ice_candidate(Box::new(move |candidate| {
        let outbound_tx = outbound_tx.clone();
        let session_id = session_id.clone();
        let my_peer_id = my_peer_id.clone();
        let remote_peer_id = Arc::clone(&remote_peer_id);
        Box::pin(async move {
            let Some(candidate) = candidate else {
                return;
            };
            let Some(to_peer_id) = remote_peer_id.lock().unwrap().clone() else {
                return;
            };
            let Ok(init) = candidate.to_json() else {
                return;
            };
            let message = SignalMessage::IceCandidate {
                session_id,
                from_peer_id: my_peer_id,
                to_peer_id,
                candidate: init.candidate,
                sdp_mid: init.sdp_mid,
                sdp_mline_index: init.sdp_mline_index,
            };
            let _ = outbound_tx.send(message);
        })
    }));
}

fn wire_state_logging(
    peer_connection: &Arc<RTCPeerConnection>,
    role: &'static str,
    diag_tx: std::sync::mpsc::Sender<String>,
) {
    let ice_diag_tx = diag_tx.clone();
    peer_connection.on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
        let _ = ice_diag_tx.send(format!("DIAG {role}: ice_connection_state -> {state:?}"));
        Box::pin(async {})
    }));
    peer_connection.on_peer_connection_state_change(Box::new(
        move |state: RTCPeerConnectionState| {
            let _ = diag_tx.send(format!("DIAG {role}: peer_connection_state -> {state:?}"));
            Box::pin(async {})
        },
    ));
}

/// Consecutive 2-second polls in which the nominated pair must show data
/// submitted but zero bytes received before the path is declared dead. Three
/// polls (~6s) is long enough to survive nomination churn right after
/// connect, and far faster than the 30-45s transfer stall watchdogs.
const DEAD_PAIR_CONSECUTIVE_POLLS: u32 = 3;

fn spawn_stats_poller(
    peer_connection: Arc<RTCPeerConnection>,
    diag_tx: std::sync::mpsc::Sender<String>,
    role: &'static str,
    relayed: Arc<AtomicBool>,
    dead_pair: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        let mut zero_recv_polls: u32 = 0;
        loop {
            interval.tick().await;

            let report = peer_connection.get_stats().await;
            let nominated = report.reports.values().find_map(|entry| match entry {
                StatsReportType::CandidatePair(pair) if pair.nominated => Some(pair),
                _ => None,
            });

            let Some(pair) = nominated else {
                let _ = diag_tx.send(format!("DIAG {role}: stats -> no nominated candidate pair yet"));
                continue;
            };

            let candidate_type = |id: &str| -> Option<CandidateType> {
                report.reports.values().find_map(|entry| match entry {
                    StatsReportType::LocalCandidate(candidate) if candidate.id == id => {
                        Some(candidate.candidate_type)
                    }
                    StatsReportType::RemoteCandidate(candidate) if candidate.id == id => {
                        Some(candidate.candidate_type)
                    }
                    _ => None,
                })
            };

            let describe_candidate = |id: &str| -> String {
                report
                    .reports
                    .values()
                    .find_map(|entry| match entry {
                        StatsReportType::LocalCandidate(candidate) if candidate.id == id => {
                            Some(format!("{:?}({})", candidate.candidate_type, candidate.ip))
                        }
                        StatsReportType::RemoteCandidate(candidate) if candidate.id == id => {
                            Some(format!("{:?}({})", candidate.candidate_type, candidate.ip))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| "unknown".to_string())
            };

            let is_relayed = candidate_type(&pair.local_candidate_id)
                == Some(CandidateType::Relay)
                || candidate_type(&pair.remote_candidate_id) == Some(CandidateType::Relay);
            relayed.store(is_relayed, Ordering::Relaxed);

            // Dead-path verdict: we are pushing bytes into the pair but have
            // never received a single byte back. A healthy pair always shows
            // inbound traffic within a poll or two (SCTP acks, DCEP
            // handshake), so several consecutive zero-received polls means
            // the pair blackholes data despite its Succeeded state.
            if pair.bytes_sent > 0 && pair.bytes_received == 0 {
                zero_recv_polls += 1;
                if zero_recv_polls >= DEAD_PAIR_CONSECUTIVE_POLLS
                    && !dead_pair.load(Ordering::Relaxed)
                {
                    dead_pair.store(true, Ordering::Relaxed);
                    let _ = diag_tx.send(format!(
                        "DIAG {role}: nominated pair is DEAD ({} polls with bytes_sent={} and zero bytes received) — flagging for relay retry",
                        zero_recv_polls, pair.bytes_sent
                    ));
                }
            } else {
                zero_recv_polls = 0;
            }

            let local = describe_candidate(&pair.local_candidate_id);
            let remote = describe_candidate(&pair.remote_candidate_id);

            let message = format!(
                "DIAG {role}: stats -> pair={local}<->{remote} state={:?} bytes_sent={} bytes_received={} rtt={:.3}s",
                pair.state, pair.bytes_sent, pair.bytes_received, pair.current_round_trip_time
            );

            if diag_tx.send(message).is_err() {
                // Receiver dropped (RtcTransport closed/dropped) -- stop polling.
                break;
            }
        }
    });
}

pub async fn run_offerer(
    relay_url: &str,
    session_id: &str,
    my_peer_id: &str,
    ice_servers: Vec<IceServer>,
    force_relay: bool,
) -> Result<ConnectedChannel, RtcError> {
    let (diag_tx, diag_rx) = std::sync::mpsc::channel::<String>();
    let (ws_stream, used_fallback_ip) =
        connect_ws_with_fallback(relay_url, &diag_tx, "offerer").await?;
    // Fallback active means DNS for the relay host is broken on this network;
    // the ICE agent resolves STUN/TURN hostnames through that same DNS, so
    // point them at the IP too.
    let ice_servers = if used_fallback_ip {
        crate::rtc::resolve::rewrite_ice_servers_to_fallback_ip(&ice_servers)
    } else {
        ice_servers
    };
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    send_ws_message(
        &mut ws_tx,
        &SignalMessage::JoinSession {
            peer_id: my_peer_id.to_string(),
            session_id: session_id.to_string(),
        },
    )
    .await?;

    let api = build_api()?;
    let configuration: RTCConfiguration =
        crate::rtc::config::to_rtc_configuration_with_policy(&ice_servers, force_relay);
    if force_relay {
        let _ = diag_tx.send(
            "DIAG rtc: ICE restricted to TURN relay candidates for this attempt".to_string(),
        );
    }
    let peer_connection = Arc::new(
        api.new_peer_connection(configuration)
            .await
            .map_err(|error| RtcError::PeerConnection(error.to_string()))?,
    );

    let (inbound_tx, inbound_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (open_tx, mut open_rx) = mpsc::unbounded_channel::<()>();
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<SignalMessage>();
    let remote_peer_id: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));

    wire_ice_candidate_outbound(
        &peer_connection,
        outbound_tx.clone(),
        session_id.to_string(),
        my_peer_id.to_string(),
        Arc::clone(&remote_peer_id),
    );
    wire_state_logging(&peer_connection, "offerer", diag_tx.clone());

    let (ws_close_tx, ws_close_rx) = oneshot::channel::<()>();
    spawn_outbound_task(ws_tx, outbound_rx, ws_close_rx);

    let mut data_channel_created = false;
    let mut pending_data_channel: Option<Arc<RTCDataChannel>> = None;
    let mut remote_description_set = false;
    let mut pending_remote_candidates: Vec<RTCIceCandidateInit> = Vec::new();

    loop {
        tokio::select! {
            _ = open_rx.recv() => {
                break;
            }
            frame = ws_rx.next() => {
                let Some(frame) = frame else {
                    return Err(RtcError::WebSocket("relay connection closed before negotiation completed".into()));
                };
                let frame = frame.map_err(|error| RtcError::WebSocket(error.to_string()))?;
                let Message::Text(text) = frame else {
                    continue;
                };
                let signal = SignalMessage::from_json(&text)
                    .map_err(|error| RtcError::Signaling(error.to_string()))?;

                match signal {
                    SignalMessage::PeerJoined { peer_id, .. } if !data_channel_created => {
                        *remote_peer_id.lock().unwrap() = Some(peer_id.clone());
                        data_channel_created = true;

                        let dc_init = RTCDataChannelInit {
                            ordered: Some(true),
                            ..Default::default()
                        };
                        let data_channel = peer_connection
                            .create_data_channel("plenum", Some(dc_init))
                            .await
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        wire_data_channel(&data_channel, inbound_tx.clone(), open_tx.clone());

                        let offer = peer_connection
                            .create_offer(None)
                            .await
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        let sdp = offer.sdp.clone();
                        peer_connection
                            .set_local_description(offer)
                            .await
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;

                        let _ = outbound_tx.send(SignalMessage::Offer {
                            session_id: session_id.to_string(),
                            from_peer_id: my_peer_id.to_string(),
                            to_peer_id: peer_id,
                            sdp,
                            nat: None,
                        });

                        // Stash the data channel handle for the caller once open.
                        pending_data_channel = Some(data_channel);
                    }
                    SignalMessage::PeerJoined { .. } => {
                        // Already negotiating/negotiated; ignore further notifications.
                    }
                    SignalMessage::Answer { sdp, .. } => {
                        let answer = RTCSessionDescription::answer(sdp)
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        peer_connection
                            .set_remote_description(answer)
                            .await
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        remote_description_set = true;
                        flush_pending_candidates(&peer_connection, &mut pending_remote_candidates).await?;
                    }
                    SignalMessage::IceCandidate { candidate, sdp_mid, sdp_mline_index, .. } => {
                        let init = RTCIceCandidateInit {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                            ..Default::default()
                        };
                        if remote_description_set {
                            peer_connection
                                .add_ice_candidate(init)
                                .await
                                .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        } else {
                            pending_remote_candidates.push(init);
                        }
                    }
                    SignalMessage::Error { message } => {
                        return Err(RtcError::Signaling(message));
                    }
                    _ => {}
                }
            }
        }
    }

    // Local data channel is open, but the remote peer may still be finishing
    // ICE — keep signaling alive so late trickle candidates aren't lost.
    spawn_signaling_linger(
        ws_rx,
        Arc::clone(&peer_connection),
        diag_tx.clone(),
        "offerer",
        ws_close_tx,
    );

    let data_channel = pending_data_channel
        .ok_or_else(|| RtcError::PeerConnection("data channel was never created".into()))?;

    let relayed = Arc::new(AtomicBool::new(false));
    let dead_pair = Arc::new(AtomicBool::new(false));
    spawn_stats_poller(
        Arc::clone(&peer_connection),
        diag_tx.clone(),
        "offerer",
        Arc::clone(&relayed),
        Arc::clone(&dead_pair),
    );

    Ok(ConnectedChannel {
        peer_connection,
        data_channel,
        inbound_rx,
        diag_tx,
        diag_rx,
        relayed,
        dead_pair,
    })
}

pub async fn run_answerer(
    relay_url: &str,
    session_id: &str,
    my_peer_id: &str,
    ice_servers: Vec<IceServer>,
    force_relay: bool,
) -> Result<ConnectedChannel, RtcError> {
    let (diag_tx, diag_rx) = std::sync::mpsc::channel::<String>();
    let (ws_stream, used_fallback_ip) =
        connect_ws_with_fallback(relay_url, &diag_tx, "answerer").await?;
    // Fallback active means DNS for the relay host is broken on this network;
    // the ICE agent resolves STUN/TURN hostnames through that same DNS, so
    // point them at the IP too.
    let ice_servers = if used_fallback_ip {
        crate::rtc::resolve::rewrite_ice_servers_to_fallback_ip(&ice_servers)
    } else {
        ice_servers
    };
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    send_ws_message(
        &mut ws_tx,
        &SignalMessage::JoinSession {
            peer_id: my_peer_id.to_string(),
            session_id: session_id.to_string(),
        },
    )
    .await?;

    let api = build_api()?;
    let configuration: RTCConfiguration =
        crate::rtc::config::to_rtc_configuration_with_policy(&ice_servers, force_relay);
    if force_relay {
        let _ = diag_tx.send(
            "DIAG rtc: ICE restricted to TURN relay candidates for this attempt".to_string(),
        );
    }
    let peer_connection = Arc::new(
        api.new_peer_connection(configuration)
            .await
            .map_err(|error| RtcError::PeerConnection(error.to_string()))?,
    );

    let (inbound_tx, inbound_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (open_tx, mut open_rx) = mpsc::unbounded_channel::<()>();
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<SignalMessage>();
    let remote_peer_id: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));

    wire_ice_candidate_outbound(
        &peer_connection,
        outbound_tx.clone(),
        session_id.to_string(),
        my_peer_id.to_string(),
        Arc::clone(&remote_peer_id),
    );
    wire_state_logging(&peer_connection, "answerer", diag_tx.clone());

    let inbound_tx_for_channel = inbound_tx.clone();
    let open_tx_for_channel = open_tx.clone();
    let pending_data_channel: Arc<std::sync::Mutex<Option<Arc<RTCDataChannel>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let pending_data_channel_setter = Arc::clone(&pending_data_channel);
    peer_connection.on_data_channel(Box::new(move |data_channel: Arc<RTCDataChannel>| {
        wire_data_channel(&data_channel, inbound_tx_for_channel.clone(), open_tx_for_channel.clone());
        if data_channel.ready_state() == RTCDataChannelState::Open {
            let _ = open_tx_for_channel.send(());
        }
        *pending_data_channel_setter.lock().unwrap() = Some(data_channel);
        Box::pin(async {})
    }));

    let (ws_close_tx, ws_close_rx) = oneshot::channel::<()>();
    spawn_outbound_task(ws_tx, outbound_rx, ws_close_rx);

    let mut answered = false;
    let mut remote_description_set = false;
    let mut pending_remote_candidates: Vec<RTCIceCandidateInit> = Vec::new();
    let mut open_poll = tokio::time::interval(Duration::from_millis(200));

    loop {
        tokio::select! {
            _ = open_rx.recv() => {
                break;
            }
            _ = open_poll.tick() => {
                let ready = pending_data_channel
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|data_channel| data_channel.ready_state());
                if ready == Some(RTCDataChannelState::Open) {
                    break;
                }
            }
            frame = ws_rx.next() => {
                let Some(frame) = frame else {
                    return Err(RtcError::WebSocket("relay connection closed before negotiation completed".into()));
                };
                let frame = frame.map_err(|error| RtcError::WebSocket(error.to_string()))?;
                let Message::Text(text) = frame else {
                    continue;
                };
                let signal = SignalMessage::from_json(&text)
                    .map_err(|error| RtcError::Signaling(error.to_string()))?;

                match signal {
                    SignalMessage::Offer { from_peer_id, sdp, .. } if !answered => {
                        *remote_peer_id.lock().unwrap() = Some(from_peer_id.clone());
                        answered = true;

                        let offer = RTCSessionDescription::offer(sdp)
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        peer_connection
                            .set_remote_description(offer)
                            .await
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        remote_description_set = true;
                        flush_pending_candidates(&peer_connection, &mut pending_remote_candidates).await?;

                        let answer = peer_connection
                            .create_answer(None)
                            .await
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        let answer_sdp = answer.sdp.clone();
                        peer_connection
                            .set_local_description(answer)
                            .await
                            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;

                        let _ = outbound_tx.send(SignalMessage::Answer {
                            session_id: session_id.to_string(),
                            from_peer_id: my_peer_id.to_string(),
                            to_peer_id: from_peer_id,
                            sdp: answer_sdp,
                        });
                    }
                    SignalMessage::Offer { .. } => {
                        // Already negotiating/negotiated; ignore further offers.
                    }
                    SignalMessage::IceCandidate { candidate, sdp_mid, sdp_mline_index, .. } => {
                        let init = RTCIceCandidateInit {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                            ..Default::default()
                        };
                        if remote_description_set {
                            peer_connection
                                .add_ice_candidate(init)
                                .await
                                .map_err(|error| RtcError::PeerConnection(error.to_string()))?;
                        } else {
                            pending_remote_candidates.push(init);
                        }
                    }
                    SignalMessage::Error { message } => {
                        return Err(RtcError::Signaling(message));
                    }
                    _ => {}
                }
            }
        }
    }

    // Local data channel is open, but the remote peer may still be finishing
    // ICE — keep signaling alive so late trickle candidates aren't lost.
    spawn_signaling_linger(
        ws_rx,
        Arc::clone(&peer_connection),
        diag_tx.clone(),
        "answerer",
        ws_close_tx,
    );

    let data_channel = pending_data_channel
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| RtcError::PeerConnection("data channel was never received".into()))?;

    let relayed = Arc::new(AtomicBool::new(false));
    let dead_pair = Arc::new(AtomicBool::new(false));
    spawn_stats_poller(
        Arc::clone(&peer_connection),
        diag_tx.clone(),
        "answerer",
        Arc::clone(&relayed),
        Arc::clone(&dead_pair),
    );

    Ok(ConnectedChannel {
        peer_connection,
        data_channel,
        inbound_rx,
        diag_tx,
        diag_rx,
        relayed,
        dead_pair,
    })
}
