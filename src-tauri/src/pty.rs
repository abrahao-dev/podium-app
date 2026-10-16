//! PTY management for Podium.
//!
//! All platform-specific code lives in this module. portable-pty abstracts
//! ConPTY (Windows) vs openpty (Unix); the conditional code here is default
//! shell detection and working-directory tracking.

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    /// Last directory reported by the shell prompt hook (OSC 9;9).
    #[cfg(windows)]
    cwd: Arc<Mutex<Option<String>>>,
    /// Shell PID; its working directory is read straight from the OS.
    #[cfg(unix)]
    pid: Option<u32>,
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

#[cfg(unix)]
fn shell_command() -> CommandBuilder {
    CommandBuilder::new(default_shell())
}

/// PowerShell's `Set-Location` does not change the process working directory,
/// so the OS cannot be asked for it. Instead the prompt is wrapped (after the
/// user's profile has loaded) to emit OSC 9;9 with the current filesystem
/// path, the same sequence Windows Terminal uses for its own cwd tracking.
#[cfg(windows)]
const PROMPT_HOOK: &str = concat!(
    "$global:__podiumPrompt = $function:prompt\n",
    "function global:prompt {\n",
    "  $osc = ''\n",
    "  if ($PWD.Provider.Name -eq 'FileSystem') {\n",
    "    $osc = [char]27 + ']9;9;' + $PWD.ProviderPath + [char]7\n",
    "  }\n",
    "  $osc + (& $global:__podiumPrompt)\n",
    "}\n",
);

#[cfg(windows)]
fn shell_command() -> CommandBuilder {
    // -EncodedCommand (base64 of UTF-16LE) sidesteps command-line quoting.
    let utf16: Vec<u8> = PROMPT_HOOK
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut cmd = CommandBuilder::new(default_shell());
    cmd.args(["-NoLogo", "-NoExit", "-EncodedCommand"]);
    cmd.arg(base64_encode(&utf16));
    cmd
}

#[cfg(windows)]
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

const OSC_CWD_PREFIX: &[u8] = b"\x1b]9;9;";
/// Upper bound for an unterminated sequence carried across reads.
const OSC_CWD_MAX: usize = 4096;

/// Incremental scanner for OSC 9;9 sequences in the PTY output stream.
/// Sequences may be split across reads, so an unterminated tail is carried
/// over to the next chunk.
#[cfg_attr(unix, allow(dead_code))]
#[derive(Default)]
struct OscCwdScanner {
    pending: Vec<u8>,
}

#[cfg_attr(unix, allow(dead_code))]
impl OscCwdScanner {
    /// Feeds a chunk and returns the last complete path found in it, if any.
    fn feed(&mut self, chunk: &[u8]) -> Option<String> {
        if self.pending.is_empty() && !chunk.contains(&0x1b) {
            return None;
        }
        self.pending.extend_from_slice(chunk);

        let mut latest = None;
        let mut pos = 0;
        while let Some(rel) = find_subslice(&self.pending[pos..], OSC_CWD_PREFIX) {
            let body = pos + rel + OSC_CWD_PREFIX.len();
            // Terminated by BEL or ST (ESC \).
            match self.pending[body..]
                .iter()
                .position(|&b| b == 0x07 || b == 0x1b)
            {
                Some(len) => {
                    if let Some(path) = parse_osc_path(&self.pending[body..body + len]) {
                        latest = Some(path);
                    }
                    pos = body + len;
                }
                None => {
                    let tail = self.pending.split_off(pos + rel);
                    self.pending = if tail.len() <= OSC_CWD_MAX {
                        tail
                    } else {
                        Vec::new()
                    };
                    return latest;
                }
            }
        }

        // Nothing open: keep only a possible partial prefix at the very end.
        let keep = (1..OSC_CWD_PREFIX.len())
            .rev()
            .find(|&n| self.pending.ends_with(&OSC_CWD_PREFIX[..n]))
            .unwrap_or(0);
        let drop_len = self.pending.len() - keep;
        self.pending.drain(..drop_len);
        latest
    }
}

#[cfg_attr(unix, allow(dead_code))]
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg_attr(unix, allow(dead_code))]
fn parse_osc_path(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let path = text.trim().trim_matches('"');
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(target_os = "linux")]
fn process_cwd(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn process_cwd(pid: u32) -> Option<String> {
    use std::ffi::CStr;
    use std::mem::{size_of, MaybeUninit};

    let pid = libc::c_int::try_from(pid).ok()?;
    let size = libc::c_int::try_from(size_of::<libc::proc_vnodepathinfo>()).ok()?;
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    // SAFETY: the buffer is a zeroed proc_vnodepathinfo of exactly `size`
    // bytes, which is what PROC_PIDVNODEPATHINFO writes into.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: proc_pidinfo filled the whole struct (checked above).
    let info = unsafe { info.assume_init() };
    // vip_path is a NUL-terminated char[MAXPATHLEN] split into 32x32 rows.
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|&c| c as u8)
        .collect();
    let path = CStr::from_bytes_until_nul(&bytes).ok()?.to_str().ok()?;
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_cwd(_pid: u32) -> Option<String> {
    None
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

    let mut cmd = shell_command();
    cmd.env("TERM", "xterm-256color");
    // A saved directory may have been removed since; fall back to the default.
    if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
        cmd.cwd(dir);
    }

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

    let killer = child.clone_killer();
    #[cfg(unix)]
    let pid = child.process_id();
    #[cfg(windows)]
    let cwd_slot: Arc<Mutex<Option<String>>> = Arc::default();
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
                #[cfg(windows)]
                cwd: Arc::clone(&cwd_slot),
                #[cfg(unix)]
                pid,
            },
        );
    }

    // Dedicated reader thread per session; small reads keep latency low.
    let sessions = Arc::clone(&manager.sessions);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        #[cfg(windows)]
        let mut scanner = OscCwdScanner::default();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    #[cfg(windows)]
                    if let Some(path) = scanner.feed(&buf[..n]) {
                        if let Ok(mut slot) = cwd_slot.lock() {
                            *slot = Some(path);
                        }
                    }
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

fn session_cwd(session: &PtySession) -> Option<String> {
    #[cfg(windows)]
    {
        session.cwd.lock().ok().and_then(|slot| slot.clone())
    }
    #[cfg(unix)]
    {
        session.pid.and_then(process_cwd)
    }
}

/// Current working directory of each running terminal's shell, keyed by
/// terminal id. Queried on demand (after a submitted line and on window
/// close), never polled.
#[tauri::command]
pub fn get_terminal_cwds(
    manager: State<'_, PtyManager>,
    ids: Vec<String>,
) -> Result<HashMap<String, String>, String> {
    let sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
    Ok(ids
        .into_iter()
        .filter_map(|id| {
            let cwd = session_cwd(sessions.get(&id)?)?;
            Some((id, cwd))
        })
        .collect())
}

#[tauri::command]
pub fn kill_terminal(manager: State<'_, PtyManager>, id: String) -> Result<(), String> {
    let mut sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
    if let Some(mut session) = sessions.remove(&id) {
        session.killer.kill().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_reads_bel_and_st_terminated_sequences() {
        let mut scanner = OscCwdScanner::default();
        assert_eq!(
            scanner.feed(&[&b"PS> \x1b]9;9;"[..], br"C:\Users\dev", b"\x07PS> "].concat()),
            Some(String::from(r"C:\Users\dev"))
        );
        assert_eq!(
            scanner.feed(&[&b"\x1b]9;9;"[..], br#""D:\work dir""#, b"\x1b\\"].concat()),
            Some(String::from(r"D:\work dir"))
        );
    }

    #[test]
    fn scanner_handles_sequences_split_across_reads() {
        let mut scanner = OscCwdScanner::default();
        assert_eq!(scanner.feed(b"output\x1b]9"), None);
        assert_eq!(scanner.feed(br";9;C:\pro"), None);
        assert_eq!(
            scanner.feed(b"jects\x07more"),
            Some(String::from(r"C:\projects"))
        );
        assert!(scanner.pending.is_empty());
    }

    #[test]
    fn scanner_keeps_last_path_and_ignores_other_escapes() {
        let mut scanner = OscCwdScanner::default();
        assert_eq!(
            scanner.feed(b"\x1b[31mred\x1b[0m\x1b]9;9;/a\x07\x1b]9;9;/b\x07"),
            Some(String::from("/b"))
        );
        assert_eq!(scanner.feed(b"\x1b]0;title\x07"), None);
    }

    #[cfg(windows)]
    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
