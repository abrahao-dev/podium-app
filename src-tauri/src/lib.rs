mod pty;
mod workspace;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(pty::PtyManager::default())
        .invoke_handler(tauri::generate_handler![
            pty::create_terminal,
            pty::write_to_terminal,
            pty::resize_terminal,
            pty::kill_terminal,
            workspace::save_workspace,
            workspace::load_workspace
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
