mod pty;
mod workspace;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first. A second launch (menu icon clicked twice)
        // would otherwise spawn its own PTYs and auto-save over the same
        // workspace.json, silently clobbering the first window's layout.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
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
