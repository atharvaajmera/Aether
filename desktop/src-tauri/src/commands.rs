use plenum::app::engine::PlenumCore;
use plenum::app::types::{
    generate_peer_id, generate_room_code, DiscoverRequest, DiscoverySummary, PlenumEvent,
    ReceiveRemoteRequest, ReceiveRequest, SendRemoteRequest, SendRequest, SessionControl,
    TransferSummary,
};
use plenum::signaling::IceServer;
use serde_json::json;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Emits a plenum-event stamped with the producing session's id so the
/// frontend can drop late events from a transfer 
fn emit_event(app: &AppHandle, session_id: u64, event: PlenumEvent) {
    let _ = app.emit(
        "plenum-event",
        json!({ "session_id": session_id, "event": event }),
    );
}

fn create_transfer_log(app: &AppHandle, role: &str) -> Option<(PathBuf, BufWriter<File>)> {
    let directory = app.path().app_log_dir().ok()?;
    fs::create_dir_all(&directory).ok()?;
    let path = directory.join(format!("{role}-{}.jsonl", unix_millis()));
    let file = File::create(&path).ok()?;
    Some((path, BufWriter::new(file)))
}

fn write_diagnostic(log: &mut Option<(PathBuf, BufWriter<File>)>, value: serde_json::Value) {
    let Some((_, writer)) = log.as_mut() else {
        return;
    };
    if serde_json::to_writer(&mut *writer, &value).is_ok() {
        let _ = writer.write_all(b"\n");
        let _ = writer.flush();
    }
}

fn current_session() -> &'static Mutex<Option<(u64, SessionControl)>> {
    static SESSION: OnceLock<Mutex<Option<(u64, SessionControl)>>> = OnceLock::new();
    SESSION.get_or_init(|| Mutex::new(None))
}

fn register_session(control: SessionControl) -> u64 {
    static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    *current_session().lock().unwrap() = Some((session_id, control));
    session_id
}

fn unregister_session(session_id: u64) {
    let mut current = current_session().lock().unwrap();
    if current.as_ref().is_some_and(|(id, _)| *id == session_id) {
        *current = None;
    }
}

#[tauri::command]
pub fn respond_to_incoming_command(accept: bool) {
    if let Some((_, control)) = current_session().lock().unwrap().as_ref() {
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
    if let Some((_, control)) = current_session().lock().unwrap().as_ref() {
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
        let session_id = register_session(core.control());
        let mut sink = |event: PlenumEvent| {
            emit_event(&app, session_id, event);
        };
        let result = core
            .send_file(request, &mut sink)
            .map_err(|e| e.to_string());
        unregister_session(session_id);
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
        let mut diagnostic_log = create_transfer_log(&app, "lan-receive");
        write_diagnostic(
            &mut diagnostic_log,
            json!({
                "timestamp_ms": unix_millis(),
                "kind": "command_started",
                "role": "receiver",
                "port": request.port,
                "output_dir": &request.output_dir,
            }),
        );
        let mut core = PlenumCore::new();
        let session_id = register_session(core.control());
        let mut sink = |event: PlenumEvent| {
            write_diagnostic(
                &mut diagnostic_log,
                json!({
                    "timestamp_ms": unix_millis(),
                    "kind": "event",
                    "event": &event,
                }),
            );
            emit_event(&app, session_id, event);
        };
        let result = core
            .receive_file(request, &mut sink)
            .map_err(|e| e.to_string());
        drop(sink);
        write_diagnostic(
            &mut diagnostic_log,
            json!({
                "timestamp_ms": unix_millis(),
                "kind": "command_finished",
                "result": match &result {
                    Ok(summary) => json!({ "ok": summary }),
                    Err(error) => json!({ "error": error }),
                },
            }),
        );
        unregister_session(session_id);
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
            emit_event(&app, 0, event);
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
        let session_id = register_session(core.control());
        let mut sink = |event: PlenumEvent| {
            emit_event(&app, session_id, event);
        };
        let result = core
            .send_file_remote(request, &mut sink)
            .map_err(|e| e.to_string());
        unregister_session(session_id);
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
        let session_id = register_session(core.control());
        let mut sink = |event: PlenumEvent| {
            emit_event(&app, session_id, event);
        };
        let result = core
            .receive_file_remote(request, &mut sink)
            .map_err(|e| e.to_string());
        unregister_session(session_id);
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
