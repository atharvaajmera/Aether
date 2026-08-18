use crate::frb_generated::StreamSink;
use flutter_rust_bridge::frb;
use plenum::app::engine::PlenumCore;
use plenum::app::types::{
    CorePermissions, DiscoverRequest, ReceiveRemoteRequest, ReceiveRequest, SendRemoteRequest,
    SendRequest, SessionControl, TransferOptions, generate_peer_id, generate_room_code,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Live transfer sessions, keyed by the Dart-supplied session token. The
/// engine calls are blocking run-to-completion, so Dart needs an out-of-band
/// handle to cancel a transfer or answer an incoming-transfer prompt; this
/// registry is that side channel.
fn sessions() -> &'static Mutex<HashMap<String, SessionControl>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, SessionControl>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_session(token: &str, control: SessionControl) {
    if token.is_empty() {
        return;
    }
    sessions()
        .lock()
        .unwrap()
        .insert(token.to_string(), control);
}

fn unregister_session(token: &str) {
    if token.is_empty() {
        return;
    }
    sessions().lock().unwrap().remove(token);
}

/// Requests cancellation of the transfer running under `session_token`.
/// The blocking transfer loop notices the flag within tens of milliseconds,
/// sends a `Close` to the peer, emits a `Cancelled` event, and returns.
#[frb(sync)]
pub fn cancel_session(session_token: String) {
    if let Some(control) = sessions().lock().unwrap().get(&session_token) {
        control.cancel();
    }
}

/// Answers the accept gate for an incoming transfer (`IncomingRequest` event)
/// on the session running under `session_token`.
#[frb(sync)]
pub fn respond_to_incoming(session_token: String, accept: bool) {
    if let Some(control) = sessions().lock().unwrap().get(&session_token) {
        if accept {
            control.accept();
        } else {
            control.decline();
        }
    }
}

#[frb(sync)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

pub fn start_discovery(sink: StreamSink<String>, timeout_secs: u64) -> anyhow::Result<()> {
    let req = DiscoverRequest {
        token: None,
        timeout_secs,
        permissions: CorePermissions::mobile_defaults(),
    };

    let mut core = PlenumCore::new();
    let mut sink_wrapper = |event: plenum::app::types::PlenumEvent| {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = sink.add(json);
        }
    };

    match core.discover_peer(req, &mut sink_wrapper) {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("Discovery failed: {}", e)),
    }
}

pub fn start_send(
    sink: StreamSink<String>,
    session_token: String,
    file_path: String,
    peer_address: String,
    optional_pin: Option<String>,
    device_name: Option<String>,
) -> anyhow::Result<String> {
    let req = SendRequest {
        file_path: PathBuf::from(file_path),
        address: Some(peer_address),
        discovery_token: optional_pin,
        device_name,
        permissions: CorePermissions::mobile_defaults(),
        options: TransferOptions::default(),
    };

    let mut core = PlenumCore::new();
    register_session(&session_token, core.control());
    let mut sink_wrapper = |event: plenum::app::types::PlenumEvent| {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = sink.add(json);
        }
    };

    let result = core.send_file(req, &mut sink_wrapper);
    unregister_session(&session_token);
    match result {
        Ok(summary) => Ok(serde_json::to_string(&summary).unwrap_or_default()),
        Err(e) => Err(anyhow::anyhow!("Send failed: {}", e)),
    }
}

pub fn start_receive(
    sink: StreamSink<String>,
    session_token: String,
    output_dir: String,
    port: u16,
    announce: bool,
    device_name: Option<String>,
    require_pin: bool,
    auto_accept: bool,
) -> anyhow::Result<String> {
    let req = ReceiveRequest {
        port,
        output_dir: PathBuf::from(output_dir),
        announce_on_lan: announce,
        device_name,
        require_pin,
        auto_accept,
        permissions: CorePermissions::mobile_defaults(),
        options: TransferOptions::default(),
    };

    let mut core = PlenumCore::new();
    register_session(&session_token, core.control());
    let mut sink_wrapper = |event: plenum::app::types::PlenumEvent| {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = sink.add(json);
        }
    };

    let result = core.receive_file(req, &mut sink_wrapper);
    unregister_session(&session_token);
    match result {
        Ok(summary) => Ok(serde_json::to_string(&summary).unwrap_or_default()),
        Err(e) => Err(anyhow::anyhow!("Receive failed: {}", e)),
    }
}

/// Sends a file over the internet via a relay/signaling server, negotiating a
/// WebRTC data channel. Mirrors `start_send`, but for internet (non-LAN) transfers.
///
/// `ice_servers_json` is a JSON-encoded array of `{ urls: string[], username?:
/// string, credential?: string }`, matching `plenum::signaling::IceServer`.
/// Passed as JSON (rather than a plain FFI struct) because `IceServer` is
/// defined in the `plenum` crate, so flutter_rust_bridge would otherwise
/// generate it as an opaque handle Dart cannot construct field-by-field.
pub fn start_send_remote(
    sink: StreamSink<String>,
    session_token: String,
    file_path: String,
    relay_server_url: String,
    session_id: String,
    my_peer_id: String,
    ice_servers_json: String,
    connect_timeout_secs: u64,
    device_name: Option<String>,
) -> anyhow::Result<String> {
    let ice_servers = serde_json::from_str(&ice_servers_json)
        .map_err(|e| anyhow::anyhow!("Invalid ice_servers_json: {}", e))?;

    let req = SendRemoteRequest {
        file_path: PathBuf::from(file_path),
        relay_server_url,
        session_id,
        my_peer_id,
        ice_servers,
        connect_timeout_secs,
        device_name,
        permissions: CorePermissions::mobile_defaults(),
        options: TransferOptions::default(),
    };

    let mut core = PlenumCore::new();
    register_session(&session_token, core.control());
    let mut sink_wrapper = |event: plenum::app::types::PlenumEvent| {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = sink.add(json);
        }
    };

    let result = core.send_file_remote(req, &mut sink_wrapper);
    unregister_session(&session_token);
    match result {
        Ok(summary) => Ok(serde_json::to_string(&summary).unwrap_or_default()),
        Err(e) => Err(anyhow::anyhow!("Send failed: {}", e)),
    }
}

/// Receives a file over the internet via a relay/signaling server, negotiating
/// a WebRTC data channel. Mirrors `start_receive`, but for internet (non-LAN) transfers.
///
/// See [`start_send_remote`] for the `ice_servers_json` shape/rationale.
pub fn start_receive_remote(
    sink: StreamSink<String>,
    session_token: String,
    output_dir: String,
    relay_server_url: String,
    session_id: String,
    my_peer_id: String,
    ice_servers_json: String,
    connect_timeout_secs: u64,
    auto_accept: bool,
    device_name: Option<String>,
) -> anyhow::Result<String> {
    let ice_servers = serde_json::from_str(&ice_servers_json)
        .map_err(|e| anyhow::anyhow!("Invalid ice_servers_json: {}", e))?;

    let req = ReceiveRemoteRequest {
        output_dir: PathBuf::from(output_dir),
        relay_server_url,
        session_id,
        my_peer_id,
        ice_servers,
        connect_timeout_secs,
        auto_accept,
        device_name,
        permissions: CorePermissions::mobile_defaults(),
        options: TransferOptions::default(),
    };

    let mut core = PlenumCore::new();
    register_session(&session_token, core.control());
    let mut sink_wrapper = |event: plenum::app::types::PlenumEvent| {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = sink.add(json);
        }
    };

    let result = core.receive_file_remote(req, &mut sink_wrapper);
    unregister_session(&session_token);
    match result {
        Ok(summary) => Ok(serde_json::to_string(&summary).unwrap_or_default()),
        Err(e) => Err(anyhow::anyhow!("Receive failed: {}", e)),
    }
}

/// Generates a display-ready room code for internet transfers, without
/// blocking on a relay-server connection (so the receive UI can show it
/// immediately).
#[frb(sync)]
pub fn generate_room_code_sync() -> String {
    generate_room_code()
}

/// Generates a random per-connection peer id for internet transfers.
#[frb(sync)]
pub fn generate_peer_id_sync() -> String {
    generate_peer_id()
}
