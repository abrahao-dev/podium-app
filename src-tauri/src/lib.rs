mod bridge;
mod pty;
mod settings;
mod workspace;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(pty::PtyManager::default())
        .setup(|app| {
            let bridge = bridge::Bridge::start(app.handle().clone())?;
            app.manage(bridge);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            pty::create_terminal,
            pty::write_to_terminal,
            pty::resize_terminal,
            pty::get_terminal_cwds,
            pty::get_terminal_agents,
            pty::kill_terminal,
            pty::set_ui_visible,
            pty::set_terminal_output_paused,
            bridge::bridge_sync_graph,
            bridge::bridge_respond,
            bridge::bridge_sync_settings,
            bridge::bridge_agent_states,
            bridge::bridge_agent_command,
            bridge::launch::install_agent_skills,
            workspace::save_workspace,
            workspace::load_workspace,
            settings::save_settings,
            settings::load_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
