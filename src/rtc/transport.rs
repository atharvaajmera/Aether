//! `RtcTransport`: a `crate::transport::Transport` implementation backed by a
//! WebRTC data channel, negotiated over a relay/signaling WebSocket.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use crate::rtc::error::RtcError;
use crate::rtc::runtime::BackgroundRuntime;
use crate::rtc::signaling_client::{run_answerer, run_offerer, ConnectedChannel};
use crate::signaling::IceServer;
use crate::transport::{Transport, TransportError, TransportResult};

const OUTBOUND_QUEUE_PACKETS: usize = 64;

const BUFFERED_HIGH_WATERMARK: usize = 1024 * 1024;
const BUFFERED_LOW_WATERMARK: usize = 256 * 1024;

pub struct RtcTransport {
    inbound_rx: std_mpsc::Receiver<Vec<u8>>,
    outbound_tx: mpsc::Sender<Vec<u8>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    diag_rx: std_mpsc::Receiver<String>,
    runtime: Option<BackgroundRuntime>,
    relayed: Arc<AtomicBool>,
    dead_pair: Arc<AtomicBool>,
    closed: bool,
}

/// Outcome of the background thread's connect-and-negotiate phase, sent back to
/// the constructing thread over a std (non-tokio) channel.
enum ConnectOutcome {
    Connected {
        outbound_tx: mpsc::Sender<Vec<u8>>,
        shutdown_tx: oneshot::Sender<()>,
        inbound_rx: std_mpsc::Receiver<Vec<u8>>,
        diag_rx: std_mpsc::Receiver<String>,
        relayed: Arc<AtomicBool>,
        dead_pair: Arc<AtomicBool>,
    },
    Failed(RtcError),
}

impl RtcTransport {
    pub fn connect_as_offerer(
        relay_url: &str,
        session_id: &str,
        my_peer_id: &str,
        ice_servers: Vec<IceServer>,
        connect_timeout: Duration,
    ) -> Result<Self, RtcError> {
        Self::connect(
            relay_url,
            session_id,
            my_peer_id,
            ice_servers,
            connect_timeout,
            true,
            false,
            None,
        )
    }

    pub fn connect_as_answerer(
        relay_url: &str,
        session_id: &str,
        my_peer_id: &str,
        ice_servers: Vec<IceServer>,
        connect_timeout: Duration,
    ) -> Result<Self, RtcError> {
        Self::connect(
            relay_url,
            session_id,
            my_peer_id,
            ice_servers,
            connect_timeout,
            false,
            false,
            None,
        )
    }

    pub fn connect_as_offerer_cancellable(
        relay_url: &str,
        session_id: &str,
        my_peer_id: &str,
        ice_servers: Vec<IceServer>,
        connect_timeout: Duration,
        force_relay: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, RtcError> {
        Self::connect(
            relay_url,
            session_id,
            my_peer_id,
            ice_servers,
            connect_timeout,
            true,
            force_relay,
            Some(cancel),
        )
    }

    /// See [`Self::connect_as_offerer_cancellable`].
    pub fn connect_as_answerer_cancellable(
        relay_url: &str,
        session_id: &str,
        my_peer_id: &str,
        ice_servers: Vec<IceServer>,
        connect_timeout: Duration,
        force_relay: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, RtcError> {
        Self::connect(
            relay_url,
            session_id,
            my_peer_id,
            ice_servers,
            connect_timeout,
            false,
            force_relay,
            Some(cancel),
        )
    }

    fn connect(
        relay_url: &str,
        session_id: &str,
        my_peer_id: &str,
        ice_servers: Vec<IceServer>,
        connect_timeout: Duration,
        is_offerer: bool,
        force_relay: bool,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<Self, RtcError> {
        let relay_url = relay_url.to_string();
        let session_id = session_id.to_string();
        let my_peer_id = my_peer_id.to_string();

        let (outcome_tx, outcome_rx) = std_mpsc::channel::<ConnectOutcome>();

        // Shared flag
        let offer_exchanged = Arc::new(AtomicBool::new(false));
        let offer_exchanged_for_task = Arc::clone(&offer_exchanged);

        let runtime = BackgroundRuntime::spawn("plenum-rtc", move || async move {
            let connected = if is_offerer {
                run_offerer(&relay_url, &session_id, &my_peer_id, ice_servers, force_relay, offer_exchanged_for_task).await
            } else {
                run_answerer(&relay_url, &session_id, &my_peer_id, ice_servers, force_relay, offer_exchanged_for_task).await
            };

            let ConnectedChannel {
                peer_connection,
                data_channel,
                inbound_rx,
                diag_tx,
                diag_rx,
                relayed,
                dead_pair,
            } = match connected {
                Ok(connected) => connected,
                Err(error) => {
                    let _ = outcome_tx.send(ConnectOutcome::Failed(error));
                    return;
                }
            };

            let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE_PACKETS);
            let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

            if outcome_tx
                .send(ConnectOutcome::Connected {
                    outbound_tx,
                    shutdown_tx,
                    inbound_rx,
                    diag_rx,
                    relayed,
                    dead_pair,
                })
                .is_err()
            {
                // Constructing thread gave up (e.g. timed out); close down.
                let _ = peer_connection.close().await;
                return;
            }
            let mut shutdown_requested = false;
            loop {
                tokio::select! {
                    biased;
                    maybe_bytes = outbound_rx.recv() => {
                        match maybe_bytes {
                            Some(bytes) => {
                                if data_channel.buffered_amount().await >= BUFFERED_HIGH_WATERMARK {
                                    while !shutdown_requested
                                        && data_channel.buffered_amount().await > BUFFERED_LOW_WATERMARK
                                    {
                                        tokio::select! {
                                            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
                                            _ = &mut shutdown_rx => {
                                                shutdown_requested = true;
                                            }
                                        }
                                    }
                                }
                                if let Err(error) = data_channel.send(&Bytes::from(bytes)).await {
                                    let _ = diag_tx.send(format!(
                                        "DIAG transport: data_channel.send failed mid-transfer: {error}"
                                    ));
                                    break;
                                }
                                if shutdown_requested {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    _ = &mut shutdown_rx, if !shutdown_requested => {
                        break;
                    }
                }
            }

            while let Ok(bytes) = outbound_rx.try_recv() {
                if let Err(error) = data_channel.send(&Bytes::from(bytes)).await {
                    let _ = diag_tx.send(format!(
                        "DIAG transport: data_channel.send failed during close-flush: {error}"
                    ));
                    break;
                }
            }
            // Bounded wait (~5s) for the send buffer to empty. 
            for _ in 0..500 {
                if data_channel.buffered_amount().await == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            let _ = peer_connection.close().await;
        });

        let deadline = Instant::now() + connect_timeout;
        let outcome = loop {
            match outcome_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(outcome) => break outcome,
                Err(std_mpsc::RecvTimeoutError::Timeout) => {
                    let cancelled = cancel
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::Relaxed));
                    if cancelled || Instant::now() >= deadline {
                        drop(outcome_rx);
                        std::thread::spawn(move || runtime.join());
                        return Err(if cancelled {
                            RtcError::Cancelled
                        } else if offer_exchanged.load(Ordering::Relaxed) {
                            // Offer/answer exchanged but ICE/DTLS never completed —
                            // a forced-relay retry may unblock a blackholed path.
                            RtcError::Timeout
                        } else {
                            // Remote peer never joined the signaling session; forcing
                            // relay cannot help, retry is useless
                            RtcError::PeerNeverArrived
                        });
                    }
                }
                Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(RtcError::PeerConnection(
                        "rtc background thread exited before producing a connection".into(),
                    ));
                }
            }
        };

        match outcome {
            ConnectOutcome::Connected {
                outbound_tx,
                shutdown_tx,
                inbound_rx,
                diag_rx,
                relayed,
                dead_pair,
            } => Ok(Self {
                inbound_rx,
                outbound_tx,
                shutdown_tx: Some(shutdown_tx),
                diag_rx,
                runtime: Some(runtime),
                relayed,
                dead_pair,
                closed: false,
            }),
            ConnectOutcome::Failed(error) => {
                runtime.join();
                Err(error)
            }
        }
    }
}

impl Transport for RtcTransport {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        if self.dead_pair.load(Ordering::Relaxed) {
            return Err(TransportError::DeadPath);
        }
        self.outbound_tx
            .blocking_send(bytes.to_vec())
            .map_err(|_| TransportError::Closed)
    }

    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        if self.dead_pair.load(Ordering::Relaxed) {
            return Err(TransportError::DeadPath);
        }
        match self
            .inbound_rx
            .recv_timeout(Duration::from_millis(1))
        {
            Ok(bytes) => Ok(Some(bytes)),
            Err(std_mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                self.closed = true;
                Err(TransportError::Closed)
            }
        }
    }

    fn close(&mut self) -> TransportResult<()> {
        self.closed = true;
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.join();
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed
    }

    fn poll_diagnostics(&mut self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        while let Ok(message) = self.diag_rx.try_recv() {
            diagnostics.push(message);
        }
        diagnostics
    }

    fn is_relayed(&self) -> Option<bool> {
        Some(self.relayed.load(Ordering::Relaxed))
    }
}

impl Drop for RtcTransport {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.close();
        }
    }
}
