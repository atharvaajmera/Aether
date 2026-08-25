pub mod commands;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn get_device_name() -> Option<String> {
    whoami::devicename().ok()
}

#[tauri::command]
fn get_username() -> Option<String> {
    whoami::username().ok()
}

#[tauri::command]
fn get_local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            commands::cleanup_old_transfer_logs(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            get_device_name,
            get_username,
            get_local_ip,
            commands::send_file_command,
            commands::receive_file_command,
            commands::discover_peers_command,
            commands::send_file_remote_command,
            commands::receive_file_remote_command,
            commands::generate_room_code_command,
            commands::generate_peer_id_command,
            commands::fetch_turn_credentials_command,
            commands::respond_to_incoming_command,
            commands::cancel_session_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
