use plenum::app::engine::PlenumCore;
use plenum::app::types::{
    generate_peer_id, generate_room_code, DiscoverRequest, DiscoverySummary, PlenumEvent,
    ReceiveRemoteRequest, ReceiveRequest, SendRemoteRequest, SendRequest, SessionControl,
    TransferSummary,
};
use plenum::signaling::IceServer;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

fn current_session() -> &'static Mutex<Option<SessionControl>> {
    static SESSION: OnceLock<Mutex<Option<SessionControl>>> = OnceLock::new();
    SESSION.get_or_init(|| Mutex::new(None))
}

fn register_session(control: SessionControl) {
    *current_session().lock().unwrap() = Some(control);
}

fn unregister_session() {
    *current_session().lock().unwrap() = None;
}

#[tauri::command]
pub fn respond_to_incoming_command(accept: bool) {
    if let Some(control) = current_session().lock().unwrap().as_ref() {
        if accept {
            control.accept();
        } else {
            control.decline();
        }
    }
}

/// Requests cancellation of the currently running transfer session.
#[tauri::command]
pub fn cancel_session_command() {
    if let Some(control) = current_session().lock().unwrap().as_ref() {
        control.cancel();
    }
}

fn default_device_name() -> Option<String> {
    whoami::devicename().ok()
}

#[tauri::command]
pub async fn send_file_command(
    app: AppHandle,
    mut request: SendRequest,
) -> Result<TransferSummary, String> {
    if request.device_name.is_none() {
        request.device_name = default_device_name();
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut core = PlenumCore::new();
        register_session(core.control());
        let mut sink = |event: PlenumEvent| {
            let _ = app.emit("plenum-event", event);
        };
        let result = core.send_file(request, &mut sink).map_err(|e| e.to_string());
        unregister_session();
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn receive_file_command(
    app: AppHandle,
    mut request: ReceiveRequest,
) -> Result<TransferSummary, String> {
    if request.device_name.is_none() {
        request.device_name = default_device_name();
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut core = PlenumCore::new();
        register_session(core.control());
        let mut sink = |event: PlenumEvent| {
            let _ = app.emit("plenum-event", event);
        };
        let result = core
            .receive_file(request, &mut sink)
            .map_err(|e| e.to_string());
        unregister_session();
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn discover_peers_command(
    app: AppHandle,
    request: DiscoverRequest,
) -> Result<DiscoverySummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut core = PlenumCore::new();
        let mut sink = |event: PlenumEvent| {
            let _ = app.emit("plenum-event", event);
        };
        core.discover_peer(request, &mut sink)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn send_file_remote_command(
    app: AppHandle,
    mut request: SendRemoteRequest,
) -> Result<TransferSummary, String> {
    if request.device_name.is_none() {
        request.device_name = default_device_name();
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut core = PlenumCore::new();
        register_session(core.control());
        let mut sink = |event: PlenumEvent| {
            let _ = app.emit("plenum-event", event);
        };
        let result = core
            .send_file_remote(request, &mut sink)
            .map_err(|e| e.to_string());
        unregister_session();
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn receive_file_remote_command(
    app: AppHandle,
    mut request: ReceiveRemoteRequest,
) -> Result<TransferSummary, String> {
    if request.device_name.is_none() {
        request.device_name = default_device_name();
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut core = PlenumCore::new();
        register_session(core.control());
        let mut sink = |event: PlenumEvent| {
            let _ = app.emit("plenum-event", event);
        };
        let result = core
            .receive_file_remote(request, &mut sink)
            .map_err(|e| e.to_string());
        unregister_session();
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn generate_room_code_command() -> String {
    generate_room_code()
}

/// Generates a random per-connection peer id for internet transfers.
#[tauri::command]
pub fn generate_peer_id_command() -> String {
    generate_peer_id()
}

#[tauri::command]
pub async fn fetch_turn_credentials_command(
    relay_server_url: String,
    peer_id: String,
) -> Result<Option<IceServer>, String> {
    Ok(plenum::rtc::turn::fetch_turn_credentials(&relay_server_url, &peer_id).await)
}
