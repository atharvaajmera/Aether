use plenum::app::engine::PlenumCore;
use plenum::app::types::{
    generate_peer_id, generate_room_code, DiscoverRequest, DiscoverySummary, PlenumEvent,
    ReceiveRemoteRequest, ReceiveRequest, SendRemoteRequest, SendRequest, SessionControl,
    TransferSummary,
};
// Only referenced by the dev-only diagnostic logging path.
#[cfg(debug_assertions)]
use plenum::app::types::LogLevel;
use plenum::signaling::IceServer;
use serde_json::json;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
// `Manager` powers `app.path()`, used only by dev-only log file handling.
#[cfg(debug_assertions)]
use tauri::Manager;

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

/// Diagnostic transfer logging is a **development-only** aid.
///
/// Release builds persist nothing to disk — no output directories, filenames,
/// peer/device names, or network diagnostics ever hit a log file — so there is
/// no retention/consent surface to manage in production. In debug builds a
/// per-transfer `<role>-<ts>.jsonl` is written to the app log directory and
/// pruned by [`cleanup_old_transfer_logs`].
///
/// Returns `Err` (dev only) so the caller can tell the user diagnostics are
/// unavailable instead of failing silently.
#[cfg(debug_assertions)]
fn create_transfer_log(app: &AppHandle, role: &str) -> Result<(PathBuf, BufWriter<File>), String> {
    let directory = app.path().app_log_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let path = directory.join(format!("{role}-{}.jsonl", unix_millis()));
    let file = File::create(&path).map_err(|e| e.to_string())?;
    Ok((path, BufWriter::new(file)))
}

/// Opens a diagnostic log for a transfer, if diagnostics are enabled for this
/// build.
///
/// Debug builds: creates the log and, on failure, emits a `Warn` event so the
/// developer sees *why* diagnostics are missing (issue: silent `None`).
/// Release builds: always `None` by design — the silence is expected, so no
/// warning is emitted.
#[cfg(debug_assertions)]
fn open_transfer_log(
    app: &AppHandle,
    session_id: u64,
    role: &str,
) -> Option<(PathBuf, BufWriter<File>)> {
    match create_transfer_log(app, role) {
        Ok(log) => Some(log),
        Err(err) => {
            emit_event(
                app,
                session_id,
                PlenumEvent::Log {
                    level: LogLevel::Warn,
                    message: format!("diagnostic log unavailable: {err}"),
                },
            );
            None
        }
    }
}

#[cfg(not(debug_assertions))]
#[inline]
fn open_transfer_log(
    _app: &AppHandle,
    _session_id: u64,
    _role: &str,
) -> Option<(PathBuf, BufWriter<File>)> {
    None
}

/// Prunes diagnostic logs older than 7 days from the app log directory.
///
/// Called once at startup. No-op in release builds (which never write logs).
#[cfg(debug_assertions)]
pub fn cleanup_old_transfer_logs(app: &AppHandle) {
    const MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;
    let Ok(directory) = app.path().app_log_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let aged_out = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age.as_secs() > MAX_AGE_SECS);
        if aged_out {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(not(debug_assertions))]
#[inline]
pub fn cleanup_old_transfer_logs(_app: &AppHandle) {}

fn write_diagnostic(log: &mut Option<(PathBuf, BufWriter<File>)>, value: serde_json::Value) {
    let Some((_, writer)) = log.as_mut() else {
        return;
    };
    if serde_json::to_writer(&mut *writer, &value).is_ok() {
        let _ = writer.write_all(b"\n");
        let _ = writer.flush();
    }
}

/// Registry of every currently-running transfer session, keyed by the numeric
/// session id handed out by [`register_session`].
///
/// Previously this held a single global slot, so a second concurrent transfer
/// (receiver + sender, overlapping mode switches, a stale command finishing
/// after a new one started, or a future multi-window UI) would clobber the
/// first, and the id-less `cancel`/`respond` commands acted on whichever
/// session happened to occupy the slot. Keying by id makes every control
/// operation target exactly the session the UI names.
fn sessions() -> &'static Mutex<HashMap<u64, SessionControl>> {
    static SESSIONS: OnceLock<Mutex<HashMap<u64, SessionControl>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Locks the session registry, recovering the guard if a previous holder
/// panicked. A poisoned lock must not wedge every subsequent transfer.
fn lock_sessions() -> std::sync::MutexGuard<'static, HashMap<u64, SessionControl>> {
    sessions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn register_session(control: SessionControl) -> u64 {
    static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    lock_sessions().insert(session_id, control);
    session_id
}

fn unregister_session(session_id: u64) {
    lock_sessions().remove(&session_id);
}

/// Looks up a session's control handle by id. Cloning is cheap — `SessionControl`
/// is a pair of `Arc`s — and lets us drop the registry lock before signaling.
fn session_control(session_id: u64) -> Option<SessionControl> {
    lock_sessions().get(&session_id).cloned()
}

/// Conveys the accept/decline signal to a specific pending session.
///
/// Returns an error (rather than `()`) so the UI can distinguish success from a
/// session that no longer exists — already finished, timed out, cancelled, or a
/// stale id from a superseded transfer. Without this, the UI would assume every
/// response landed.
#[tauri::command]
pub fn respond_to_incoming_command(session_id: u64, accept: bool) -> Result<(), String> {
    let Some(control) = session_control(session_id) else {
        return Err("transfer request is no longer active".to_string());
    };
    if accept {
        control.accept();
    } else {
        control.decline();
    }
    Ok(())
}

// Requests cancellation of a specific running transfer session.
#[tauri::command]
pub fn cancel_session_command(session_id: u64) {
    if let Some(control) = session_control(session_id) {
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
        let mut core = PlenumCore::new();
        let session_id = register_session(core.control());
        let mut diagnostic_log = open_transfer_log(&app, session_id, "lan-receive");
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
