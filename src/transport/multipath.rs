use std::time::{Duration, Instant};

use crate::transport::{Transport, TransportResult};

pub struct MultipathTransport {
    local_path: Box<dyn Transport + Send + Sync>,
    control_path: Box<dyn Transport + Send + Sync>,
    failover_enabled: bool,

    local_active: bool,

    // Weighted scheduling (e.g., 95% local, 5% relay)
    // Defaults to 100% local until a real relay transport is wired in.
    local_weight: f64,
    control_weight: f64,

    chunks_sent_local: u64,
    chunks_sent_control: u64,

    // Active latency and congestion monitoring
    last_local_recv: Option<Instant>,
    last_control_recv: Option<Instant>,

    pub local_inter_arrival_avg: Duration,
    pub control_inter_arrival_avg: Duration,

    closed: bool,
}

impl MultipathTransport {
    pub fn new(
        local_path: Box<dyn Transport + Send + Sync>,
        control_path: Box<dyn Transport + Send + Sync>,
    ) -> Self {
        Self {
            local_path,
            control_path,
            failover_enabled: false,
            local_active: true,
            // Route 100% through local until a real relay transport is connected.
            // When a real control path is available, call set_weights(0.95, 0.05).
            local_weight: 1.0,
            control_weight: 0.0,
            chunks_sent_local: 0,
            chunks_sent_control: 0,
            last_local_recv: None,
            last_control_recv: None,
            local_inter_arrival_avg: Duration::from_millis(0),
            control_inter_arrival_avg: Duration::from_millis(0),
            closed: false,
        }
    }

    /// Activate multipath scheduling with custom weights once a real relay
    /// transport is available.
    pub fn set_weights(&mut self, local: f64, control: f64) {
        self.local_weight = local;
        self.control_weight = control;
    }

    /// Enables automatic failover after the caller has configured a genuine
    /// end-to-end secondary path. It is deliberately off by default: merely
    /// having another local transport object does not mean its frames can
    /// reach the remote peer.
    pub fn enable_failover(&mut self) {
        self.failover_enabled = true;
    }

    fn update_metrics(&mut self, is_local: bool) {
        let now = Instant::now();
        if is_local {
            if let Some(last) = self.last_local_recv {
                let diff = now.duration_since(last);
                if self.local_inter_arrival_avg.as_millis() == 0 {
                    self.local_inter_arrival_avg = diff;
                } else {
                    // Moving average
                    self.local_inter_arrival_avg = (self.local_inter_arrival_avg * 9 + diff) / 10;
                }
            }
            self.last_local_recv = Some(now);
        } else {
            if let Some(last) = self.last_control_recv {
                let diff = now.duration_since(last);
                if self.control_inter_arrival_avg.as_millis() == 0 {
                    self.control_inter_arrival_avg = diff;
                } else {
                    self.control_inter_arrival_avg =
                        (self.control_inter_arrival_avg * 9 + diff) / 10;
                }
            }
            self.last_control_recv = Some(now);
        }
    }

    pub fn failover_to_control(&mut self) {
        self.local_active = false;
        self.local_weight = 0.0;
        self.control_weight = 1.0;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

impl Transport for MultipathTransport {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        let total_sent = self.chunks_sent_local + self.chunks_sent_control;
        let expected_local = (total_sent as f64 * self.local_weight) as u64;

        let target_local = self.local_active && self.chunks_sent_local <= expected_local;

        if target_local {
            match self.local_path.send(bytes) {
                Ok(_) => {
                    self.chunks_sent_local += 1;
                    Ok(())
                }
                Err(error) => {
                    if !self.failover_enabled {
                        self.closed = self.local_path.is_closed();
                        return Err(error);
                    }
                    self.failover_to_control();
                    self.control_path.send(bytes).map(|_| {
                        self.chunks_sent_control += 1;
                    })
                }
            }
        } else {
            match self.control_path.send(bytes) {
                Ok(_) => {
                    self.chunks_sent_control += 1;
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
    }

    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        if self.local_active {
            match self.local_path.recv() {
                Ok(Some(data)) => {
                    self.update_metrics(true);
                    return Ok(Some(data));
                }
                Ok(None) => {
                    // No data available on local right now; fall through to
                    // try control path below.
                }
                Err(e) => {
                    // Propagate the error so callers (e.g., engine receive loop)
                    // can detect a closed connection and break cleanly, instead
                    // of silently failing over to a dummy control path.
                    self.closed = true;
                    return Err(e);
                }
            }
        }

        match self.control_path.recv() {
            Ok(Some(data)) => {
                self.update_metrics(false);
                Ok(Some(data))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn close(&mut self) -> TransportResult<()> {
        self.closed = true;
        let mut err = None;
        if let Err(e) = self.local_path.close() {
            err = Some(e);
        }
        if let Err(e) = self.control_path.close() {
            err = Some(e);
        }
        if let Some(e) = err {
            return Err(e);
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed
    }

    fn is_relayed(&self) -> Option<bool> {
        if self.local_active {
            self.local_path.is_relayed()
        } else {
            self.control_path.is_relayed()
        }
    }

    fn poll_diagnostics(&mut self) -> Vec<String> {
        let mut diagnostics: Vec<String> = self
            .local_path
            .poll_diagnostics()
            .into_iter()
            .map(|msg| format!("[local] {msg}"))
            .collect();
        diagnostics.extend(
            self.control_path
                .poll_diagnostics()
                .into_iter()
                .map(|msg| format!("[control] {msg}")),
        );
        diagnostics
    }
}
