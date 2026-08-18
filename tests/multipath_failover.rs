use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use plenum::transport::{MultipathTransport, Transport, TransportError, TransportResult};

#[derive(Clone)]
struct Endpoint {
    incoming: Arc<Mutex<VecDeque<Vec<u8>>>>,
    remote_incoming: Arc<Mutex<VecDeque<Vec<u8>>>>,
    closed: bool,
}

impl Endpoint {
    fn pair() -> (Self, Self) {
        let left = Arc::new(Mutex::new(VecDeque::new()));
        let right = Arc::new(Mutex::new(VecDeque::new()));
        (
            Self {
                incoming: left.clone(),
                remote_incoming: right.clone(),
                closed: false,
            },
            Self {
                incoming: right,
                remote_incoming: left,
                closed: false,
            },
        )
    }
}

impl Transport for Endpoint {
    fn send(&mut self, bytes: &[u8]) -> TransportResult<()> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        self.remote_incoming
            .lock()
            .unwrap()
            .push_back(bytes.to_vec());
        Ok(())
    }

    fn recv(&mut self) -> TransportResult<Option<Vec<u8>>> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        Ok(self.incoming.lock().unwrap().pop_front())
    }

    fn close(&mut self) -> TransportResult<()> {
        self.closed = true;
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}

#[test]
fn local_error_propagates_when_failover_is_not_enabled() {
    let (mut local, _local_peer) = Endpoint::pair();
    let (control, _control_peer) = Endpoint::pair();
    local.close().unwrap();

    let mut multipath = MultipathTransport::new(Box::new(local), Box::new(control));
    let error = multipath.send(b"must not disappear").unwrap_err();

    assert_eq!(error, TransportError::Closed);
}

#[test]
fn enabled_failover_delivers_to_the_remote_secondary_endpoint() {
    let (mut local, _local_peer) = Endpoint::pair();
    let (control, mut control_peer) = Endpoint::pair();
    local.close().unwrap();

    let mut multipath = MultipathTransport::new(Box::new(local), Box::new(control));
    multipath.enable_failover();
    multipath.send(b"delivered on backup").unwrap();

    assert_eq!(
        control_peer.recv().unwrap(),
        Some(b"delivered on backup".to_vec())
    );
}
