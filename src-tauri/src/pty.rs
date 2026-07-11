//! PTY management for Podium.
//!
//! All platform-specific code lives in this module. portable-pty abstracts
//! ConPTY (Windows) vs openpty (Unix); the only conditional code here is
//! default shell detection.

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
}

#[derive(Default)]
pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
}

/// Default shell: $SHELL on Unix; if unset, the first of bash/zsh/sh that
/// exists (bash is ubiquitous on Linux, zsh is the macOS default, sh is the
/// POSIX floor). On Windows prefer PowerShell 7 (pwsh.exe) if on PATH, else
/// powershell.exe.
#[cfg(unix)]
fn default_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() {
            return shell;
        }
    }
    for candidate in ["/bin/bash", "/bin/zsh", "/bin/sh"] {
        if std::path::Path::new(candidate).is_file() {
            return String::from(candidate);
        }
    }
    String::from("/bin/sh")
}

#[cfg(windows)]
fn default_shell() -> String {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.join("pwsh.exe").is_file() {
                return String::from("pwsh.exe");
            }
        }
    }
    String::from("powershell.exe")
}

#[tauri::command]
pub fn create_terminal(
    app: AppHandle,
    manager: State<'_, PtyManager>,
    id: String,
    cwd: Option<String>,
    command: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    {
        let sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
        if sessions.contains_key(&id) {
            return Err(format!("terminal '{id}' already exists"));
        }
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let mut cmd = CommandBuilder::new(default_shell());
    cmd.env("TERM", "xterm-256color");
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    // Startup command goes through the spawned shell so PATH resolution works
    // the same on both platforms. \r submits (never \n).
    if let Some(command) = command {
        if !command.is_empty() {
            writer
                .write_all(format!("{command}\r").as_bytes())
                .map_err(|e| e.to_string())?;
        }
    }

    {
        let mut sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
        sessions.insert(
            id.clone(),
            PtySession {
                master: pair.master,
                writer,
                killer,
            },
        );
    }

    // Dedicated reader thread per session; small reads keep latency low.
    let sessions = Arc::clone(&manager.sessions);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = app.emit(&format!("terminal-output:{id}"), buf[..n].to_vec());
                }
            }
        }
        let exit_code = child.wait().ok().map(|status| status.exit_code());
        if let Ok(mut sessions) = sessions.lock() {
            sessions.remove(&id);
        }
        let _ = app.emit(&format!("terminal-exit:{id}"), exit_code);
    });

    Ok(())
}

#[tauri::command]
pub fn write_to_terminal(
    manager: State<'_, PtyManager>,
    id: String,
    data: String,
) -> Result<(), String> {
    let mut sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| format!("terminal '{id}' not found"))?;
    session
        .writer
        .write_all(data.as_bytes())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resize_terminal(
    manager: State<'_, PtyManager>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
    let session = sessions
        .get(&id)
        .ok_or_else(|| format!("terminal '{id}' not found"))?;
    session
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn kill_terminal(manager: State<'_, PtyManager>, id: String) -> Result<(), String> {
    let mut sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
    if let Some(mut session) = sessions.remove(&id) {
        session.killer.kill().map_err(|e| e.to_string())?;
    }
    Ok(())
}
