//! App settings persistence: a single `settings.json` in app_data_dir.
//! Writes are atomic (temp file in the same directory, then rename) so a
//! crash mid-write never leaves a truncated file behind.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const SETTINGS_FILE: &str = "settings.json";
const SETTINGS_TMP_FILE: &str = "settings.json.tmp";

fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn write_atomic(dir: &Path, data: &str) -> Result<(), String> {
    let tmp = dir.join(SETTINGS_TMP_FILE);
    let target = dir.join(SETTINGS_FILE);
    {
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    // `fs::rename` replaces an existing target on both Unix and Windows
    // (MoveFileExW with MOVEFILE_REPLACE_EXISTING).
    fs::rename(&tmp, &target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

fn read_existing(dir: &Path) -> Result<Option<String>, String> {
    let path = dir.join(SETTINGS_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_settings(app: AppHandle, data: String) -> Result<(), String> {
    write_atomic(&data_dir(&app)?, &data)
}

#[tauri::command]
pub fn load_settings(app: AppHandle) -> Result<Option<String>, String> {
    read_existing(&data_dir(&app)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("podium-settings-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_reads_as_none() {
        let dir = temp_dir("missing");
        assert_eq!(read_existing(&dir).unwrap(), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_then_overwrite_round_trips() {
        let dir = temp_dir("roundtrip");
        write_atomic(&dir, "{\"version\":1}").unwrap();
        write_atomic(&dir, "{\"version\":2}").unwrap();
        assert_eq!(
            read_existing(&dir).unwrap().as_deref(),
            Some("{\"version\":2}")
        );
        assert!(!dir.join(SETTINGS_TMP_FILE).exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
