//! Workspace persistence: one JSON file per workspace in app_data_dir.
//! Paths built with PathBuf only — no manual separator concatenation.

use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

fn workspaces_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("workspaces");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.contains(['/', '\\', '.'])
        || !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid workspace name '{name}'"));
    }
    Ok(())
}

#[tauri::command]
pub fn save_workspace(app: AppHandle, name: String, data: String) -> Result<(), String> {
    validate_name(&name)?;
    let path = workspaces_dir(&app)?.join(format!("{name}.json"));
    fs::write(path, data).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_workspace(app: AppHandle, name: String) -> Result<Option<String>, String> {
    validate_name(&name)?;
    let path = workspaces_dir(&app)?.join(format!("{name}.json"));
    if !path.is_file() {
        return Ok(None);
    }
    fs::read_to_string(path).map(Some).map_err(|e| e.to_string())
}
