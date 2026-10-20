//! PTY management for Podium.
//!
//! All platform-specific code lives in this module. portable-pty abstracts
//! ConPTY (Windows) vs openpty (Unix); the conditional code here is default
//! shell detection, working-directory tracking and the process-table access
//! used to detect agent CLIs running inside a terminal.

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, Emitter, State};

/// Output is forwarded to the webview at most once per frame while a terminal
/// is busy; an isolated chunk after a quiet period goes out immediately so
/// keystroke echo gets no added latency.
const OUTPUT_FRAME: Duration = Duration::from_millis(16);
/// Upper bound for one coalesced output message.
const OUTPUT_MAX_BATCH: usize = 256 * 1024;
/// Rows of history kept by the headless screen mirror.
const SCREEN_SCROLLBACK: usize = 200;

/// Message from a session's reader thread to its emitter thread.
enum OutputMsg {
    Data(Vec<u8>),
    /// Replace whatever the view shows with the mirrored screen state.
    Resync,
}

/// Headless copy of the terminal screen, fed the same bytes as the view.
/// It survives while the webview is not consuming output (window hidden or
/// a view falling behind), so the view can be restored from it instead of the
/// PTY ever being paused: agents keep running at full speed regardless of
/// what the UI is doing.
pub type ScreenMirror = Arc<Mutex<vt100::Parser>>;

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    screen: ScreenMirror,
    /// Set by the view when its write queue is backed up.
    paused: Arc<AtomicBool>,
    output_tx: mpsc::Sender<OutputMsg>,
    /// Last directory reported by the shell prompt hook (OSC 9;9).
    #[cfg(windows)]
    cwd: Arc<Mutex<Option<String>>>,
    /// Shell PID. On Unix its working directory is read straight from the
    /// OS; on every platform it is the root of the agent process walk.
    pid: Option<u32>,
}

#[derive(Default)]
pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    /// The webview is hidden (minimized / occluded): it throttles timers, so
    /// xterm would parse output far slower than agents produce it.
    ui_hidden: Arc<AtomicBool>,
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
#[allow(clippy::too_many_arguments)]
pub fn create_terminal(
    app: AppHandle,
    manager: State<'_, PtyManager>,
    id: String,
    cwd: Option<String>,
    command: Option<String>,
    cols: u16,
    rows: u16,
    on_output: Channel<Response>,
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

    let screen: ScreenMirror = Arc::new(Mutex::new(new_screen(rows, cols)));
    let paused = Arc::new(AtomicBool::new(false));
    let (output_tx, output_rx) = mpsc::channel::<OutputMsg>();

    {
        let mut sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
        sessions.insert(
            id.clone(),
            PtySession {
                master: pair.master,
                writer,
                killer,
                screen: Arc::clone(&screen),
                paused: Arc::clone(&paused),
                output_tx: output_tx.clone(),
                #[cfg(windows)]
                cwd: Arc::clone(&cwd_slot),
                pid,
            },
        );
    }

    let emitter = {
        let screen = Arc::clone(&screen);
        let ui_hidden = Arc::clone(&manager.ui_hidden);
        std::thread::spawn(move || {
            run_output_emitter(&output_rx, &screen, &paused, &ui_hidden, &on_output);
        })
    };

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
                    // Mirror and enqueue under one lock: a resync snapshot then
                    // covers exactly the chunks queued before it.
                    if let Ok(mut parser) = screen.lock() {
                        parser.process(&buf[..n]);
                        let _ = output_tx.send(OutputMsg::Data(buf[..n].to_vec()));
                    }
                }
            }
        }
        // Dropping both senders (ours and the session's) closes the channel:
        // the emitter flushes what is left, then the exit is announced.
        drop(output_tx);
        if let Ok(mut sessions) = sessions.lock() {
            sessions.remove(&id);
        }
        let _ = emitter.join();
        let exit_code = child.wait().ok().map(|status| status.exit_code());
        let _ = app.emit(&format!("terminal-exit:{id}"), exit_code);
    });

    Ok(())
}

fn new_screen(rows: u16, cols: u16) -> vt100::Parser {
    vt100::Parser::new(
        std::num::NonZeroU16::new(rows).unwrap_or(std::num::NonZeroU16::MIN),
        std::num::NonZeroU16::new(cols).unwrap_or(std::num::NonZeroU16::MIN),
        SCREEN_SCROLLBACK,
    )
}

/// Coalesces a session's output into at most one message per frame and
/// forwards it to the view. While the view is not consuming output (hidden
/// window, or paused by its own backpressure) output is dropped, and the view
/// is restored from the screen mirror once it consumes again.
fn run_output_emitter(
    rx: &mpsc::Receiver<OutputMsg>,
    screen: &ScreenMirror,
    paused: &AtomicBool,
    ui_hidden: &AtomicBool,
    channel: &Channel<Response>,
) {
    let mut last_emit = Instant::now()
        .checked_sub(OUTPUT_FRAME)
        .unwrap_or_else(Instant::now);
    let mut stale = false;
    let mut closed = false;
    while !closed {
        let mut batch = Vec::new();
        let mut resync = false;
        match rx.recv() {
            Ok(OutputMsg::Data(chunk)) => batch = chunk,
            Ok(OutputMsg::Resync) => resync = true,
            Err(_) => break,
        }
        let deadline = last_emit + OUTPUT_FRAME;
        while !resync && batch.len() < OUTPUT_MAX_BATCH {
            let now = Instant::now();
            let next = if now >= deadline {
                rx.try_recv().map_err(|e| match e {
                    mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                    mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                })
            } else {
                rx.recv_timeout(deadline - now)
            };
            match next {
                Ok(OutputMsg::Data(chunk)) => batch.extend_from_slice(&chunk),
                Ok(OutputMsg::Resync) => resync = true,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => closed = true,
            }
            if closed {
                break;
            }
        }

        if paused.load(Ordering::Relaxed) || ui_hidden.load(Ordering::Relaxed) {
            stale = true;
            continue;
        }
        if resync || stale {
            let snapshot = match screen.lock() {
                Ok(parser) => {
                    // Everything queued so far is already part of the mirror.
                    while rx.try_recv().is_ok() {}
                    let mut bytes = b"\x1bc".to_vec();
                    bytes.extend_from_slice(parser.screen().state_formatted().as_bytes());
                    bytes
                }
                Err(_) => continue,
            };
            stale = false;
            let _ = channel.send(Response::new(snapshot));
        } else if !batch.is_empty() {
            let _ = channel.send(Response::new(batch));
        }
        last_emit = Instant::now();
    }
}

/// The webview reports whether it is visible. Hidden webviews throttle
/// timers (xterm parses on them), so output is not forwarded meanwhile and
/// every terminal is restored from its screen mirror when it shows again.
#[tauri::command]
pub fn set_ui_visible(manager: State<'_, PtyManager>, visible: bool) -> Result<(), String> {
    manager.ui_hidden.store(!visible, Ordering::Relaxed);
    if visible {
        let sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
        for session in sessions.values() {
            let _ = session.output_tx.send(OutputMsg::Resync);
        }
    }
    Ok(())
}

/// Backpressure from a view whose xterm write queue is backed up: output is
/// held back (the PTY keeps being drained) and the view is restored from the
/// screen mirror when it resumes.
#[tauri::command]
pub fn set_terminal_output_paused(
    manager: State<'_, PtyManager>,
    id: String,
    paused: bool,
) -> Result<(), String> {
    let sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
    let Some(session) = sessions.get(&id) else {
        return Ok(());
    };
    session.paused.store(paused, Ordering::Relaxed);
    if !paused {
        let _ = session.output_tx.send(OutputMsg::Resync);
    }
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
    if let Ok(mut parser) = session.screen.lock() {
        if let (Some(r), Some(c)) = (
            std::num::NonZeroU16::new(rows),
            std::num::NonZeroU16::new(cols),
        ) {
            parser.set_size(r, c);
        }
    }
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

// ---------------------------------------------------------------------------
// Agent session detection
//
// Agents are launched by the user inside the shell, so they are discovered
// from the process tree: one process-table snapshot per command call, walked
// from each terminal's shell PID. Session ids come from the agents' own state
// files, which are undocumented internals; every lookup degrades to `None`.
// ---------------------------------------------------------------------------

/// Agent CLI running inside a terminal, as reported to the frontend.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    /// "claude" | "codex" | "opencode"
    pub kind: String,
    /// Agent's own session id when it could be determined; None → frontend
    /// falls back to "continue last session in this cwd".
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentKind {
    Claude,
    Codex,
    OpenCode,
}

impl AgentKind {
    fn as_str(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::OpenCode => "opencode",
        }
    }
}

/// Maximum depth below the shell (shims such as cmd → node → native binary
/// put the agent two or three levels down).
const AGENT_MAX_DEPTH: usize = 6;
/// Maximum processes inspected per terminal.
const AGENT_MAX_VISITED: usize = 256;
/// Tolerance when comparing process start times with timestamps written by
/// the agents themselves.
const START_SLACK_MS: i64 = 2_000;
/// Newest `sessions/YYYY/MM/DD` directories scanned for Codex rollouts.
const CODEX_DAY_DIRS: usize = 7;
/// Upper bound for rollout files considered per call.
const CODEX_MAX_FILES: usize = 512;
/// Upper bound for the session-meta line read from a rollout file (it embeds
/// the base instructions, typically tens of KB).
const CODEX_HEAD_MAX: u64 = 4 * 1024 * 1024;

/// Process start time. `filetime` is the raw Windows creation time, which
/// Claude Code records verbatim as `procStart` in its session file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessStart {
    epoch_ms: i64,
    filetime: Option<u64>,
}

/// Read access to the process tree. Implementations cache per call.
trait ProcessSource {
    fn children(&mut self, pid: u32) -> Vec<u32>;
    fn name(&mut self, pid: u32) -> Option<String>;
    fn start(&mut self, pid: u32) -> Option<ProcessStart>;
}

struct ProcEntry {
    pid: u32,
    ppid: u32,
    name: String,
    start: Option<ProcessStart>,
}

/// Full process-table snapshot indexed by parent PID. Start times missing
/// from the snapshot are resolved on demand through `lookup_start`.
struct ProcessTable {
    names: HashMap<u32, String>,
    children: HashMap<u32, Vec<u32>>,
    starts: HashMap<u32, Option<ProcessStart>>,
    lookup_start: fn(u32) -> Option<ProcessStart>,
}

impl ProcessTable {
    fn new(entries: Vec<ProcEntry>, lookup_start: fn(u32) -> Option<ProcessStart>) -> Self {
        let mut table = ProcessTable {
            names: HashMap::with_capacity(entries.len()),
            children: HashMap::new(),
            starts: HashMap::new(),
            lookup_start,
        };
        for entry in entries {
            // PID 0 (and anything claiming to be its own parent) would loop.
            if entry.pid != entry.ppid {
                table
                    .children
                    .entry(entry.ppid)
                    .or_default()
                    .push(entry.pid);
            }
            if entry.start.is_some() {
                table.starts.insert(entry.pid, entry.start);
            }
            table.names.insert(entry.pid, entry.name);
        }
        table
    }
}

impl ProcessSource for ProcessTable {
    fn children(&mut self, pid: u32) -> Vec<u32> {
        self.children.get(&pid).cloned().unwrap_or_default()
    }

    fn name(&mut self, pid: u32) -> Option<String> {
        self.names.get(&pid).cloned()
    }

    fn start(&mut self, pid: u32) -> Option<ProcessStart> {
        let lookup = self.lookup_start;
        *self.starts.entry(pid).or_insert_with(|| lookup(pid))
    }
}

/// Agent kind from an executable or command name (`claude.exe`, `codex`,
/// `opencode2`, ...). Case-insensitive; a trailing `.exe` is ignored.
fn agent_kind_from_name(name: &str) -> Option<AgentKind> {
    let lower = name.trim().to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    match stem {
        "claude" => Some(AgentKind::Claude),
        "codex" => Some(AgentKind::Codex),
        // `opencode` plus the `opencode2` / `opencode2-node` builds.
        s if s.starts_with("opencode") => Some(AgentKind::OpenCode),
        _ => None,
    }
}

/// Fields Podium reads from Claude Code's `sessions/<pid>.json`.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ClaudeSessionFile {
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    session_id: Option<String>,
    /// Epoch milliseconds.
    #[serde(default)]
    started_at: Option<i64>,
    /// Process creation time as recorded by Claude (Windows FILETIME).
    #[serde(default)]
    proc_start: Option<String>,
}

fn parse_claude_session(json: &str) -> Option<ClaudeSessionFile> {
    serde_json::from_str(json).ok()
}

/// Rejects session files left behind by a dead process whose PID was reused.
fn claude_session_matches(
    file: &ClaudeSessionFile,
    pid: u32,
    start: Option<&ProcessStart>,
) -> bool {
    if file.pid.is_some_and(|recorded| recorded != pid) {
        return false;
    }
    let Some(start) = start else {
        return true;
    };
    let recorded_filetime = file
        .proc_start
        .as_deref()
        .and_then(|s| s.trim().parse::<u64>().ok());
    if let (Some(actual), Some(recorded)) = (start.filetime, recorded_filetime) {
        return actual == recorded;
    }
    // A session cannot have started before the process that owns it.
    file.started_at
        .is_none_or(|started| started + START_SLACK_MS >= start.epoch_ms)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FoundAgent {
    kind: AgentKind,
    /// Innermost process of the agent (the native binary behind any shim).
    pid: u32,
    start: Option<ProcessStart>,
    /// Claude session id taken from its session file.
    claude_session_id: Option<String>,
}

fn valid_claude_session<S: ProcessSource>(
    src: &mut S,
    pid: u32,
    read_claude: &mut dyn FnMut(u32) -> Option<ClaudeSessionFile>,
) -> Option<ClaudeSessionFile> {
    let file = read_claude(pid)?;
    let start = src.start(pid);
    claude_session_matches(&file, pid, start.as_ref()).then_some(file)
}

/// Classifies one process. A process is an agent when its executable name is
/// one of the agent CLIs, or (for Claude) when a valid Claude session file
/// exists for its PID — that covers npm installs running `node cli.js`.
/// Launcher shims are followed down through direct children of the same
/// kind, so `node codex.js` → `codex.exe` resolves to the native binary.
fn classify_agent<S: ProcessSource>(
    src: &mut S,
    pid: u32,
    read_claude: &mut dyn FnMut(u32) -> Option<ClaudeSessionFile>,
) -> Option<FoundAgent> {
    let by_name = agent_kind_from_name(&src.name(pid)?);
    let outer_claude = match by_name {
        Some(AgentKind::Claude) | None => valid_claude_session(src, pid, read_claude),
        Some(_) => None,
    };
    let kind = by_name.or(outer_claude.as_ref().map(|_| AgentKind::Claude))?;

    let mut chain = vec![pid];
    while chain.len() < AGENT_MAX_DEPTH {
        let current = chain[chain.len() - 1];
        let next = src
            .children(current)
            .into_iter()
            .find(|&child| src.name(child).and_then(|n| agent_kind_from_name(&n)) == Some(kind));
        match next {
            Some(child) => chain.push(child),
            None => break,
        }
    }

    let mut claude_session_id = outer_claude.and_then(|file| file.session_id);
    if kind == AgentKind::Claude && claude_session_id.is_none() {
        for &inner in &chain[1..] {
            claude_session_id =
                valid_claude_session(src, inner, read_claude).and_then(|file| file.session_id);
            if claude_session_id.is_some() {
                break;
            }
        }
    }

    let innermost = chain[chain.len() - 1];
    Some(FoundAgent {
        kind,
        pid: innermost,
        start: src.start(innermost),
        claude_session_id,
    })
}

/// Finds the agent running under `shell_pid`.
///
/// Depth-first walk of the shell's descendants (bounded by
/// `AGENT_MAX_DEPTH` / `AGENT_MAX_VISITED`). The walk does not descend into
/// an agent: whatever an agent spawns (tool shells, MCP servers, nested
/// agents run as tools) belongs to that agent's session. When several agents
/// sit side by side under the shell (e.g. a background job), the most
/// recently started one wins. Agents that claim to have started before the
/// shell are discarded: on Windows parent PIDs are never re-parented, so a
/// stale parent PID can point at a reused shell PID.
fn find_agent<S: ProcessSource>(
    src: &mut S,
    shell_pid: u32,
    read_claude: &mut dyn FnMut(u32) -> Option<ClaudeSessionFile>,
) -> Option<FoundAgent> {
    let shell_start = src.start(shell_pid);
    let mut found: Vec<FoundAgent> = Vec::new();
    let mut stack = vec![(shell_pid, 0usize)];
    let mut visited = 0usize;
    'walk: while let Some((pid, depth)) = stack.pop() {
        for child in src.children(pid) {
            visited += 1;
            if visited > AGENT_MAX_VISITED {
                break 'walk;
            }
            if let Some(agent) = classify_agent(src, child, read_claude) {
                found.push(agent);
            } else if depth + 1 < AGENT_MAX_DEPTH {
                stack.push((child, depth + 1));
            }
        }
    }
    found
        .into_iter()
        .filter(|agent| match (&shell_start, &agent.start) {
            (Some(shell), Some(agent)) => agent.epoch_ms + START_SLACK_MS >= shell.epoch_ms,
            _ => true,
        })
        .max_by_key(|agent| agent.start.map_or(i64::MIN, |s| s.epoch_ms))
}

/// Session-meta record (first line) of a Codex rollout file:
/// `{"timestamp":…,"type":"session_meta","payload":{"id":…,"cwd":…,…}}`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexMeta {
    /// Thread id, the argument `codex resume` takes.
    id: String,
    cwd: Option<String>,
    started_ms: Option<i64>,
    /// Sub-agent / internal threads share the cwd but are not resumable on
    /// their own.
    subagent: bool,
}

#[derive(Deserialize)]
struct CodexHeadLine {
    #[serde(rename = "type")]
    kind: String,
    payload: CodexMetaPayload,
}

#[derive(Deserialize)]
struct CodexMetaPayload {
    id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    parent_thread_id: Option<serde_json::Value>,
    #[serde(default)]
    source: Option<serde_json::Value>,
}

fn parse_codex_meta(line: &str) -> Option<CodexMeta> {
    let head: CodexHeadLine = serde_json::from_str(line.trim()).ok()?;
    if head.kind != "session_meta" || head.payload.id.is_empty() {
        return None;
    }
    let payload = head.payload;
    let has_parent = payload
        .parent_thread_id
        .as_ref()
        .is_some_and(|v| !v.is_null());
    let internal_source = payload.source.as_ref().is_some_and(|source| {
        source
            .as_object()
            .is_some_and(|o| o.contains_key("subagent") || o.contains_key("internal"))
    });
    Some(CodexMeta {
        id: payload.id,
        cwd: payload.cwd,
        started_ms: payload
            .timestamp
            .as_deref()
            .and_then(parse_utc_timestamp_ms),
        subagent: has_parent || internal_source,
    })
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff…]Z` into epoch milliseconds.
fn parse_utc_timestamp_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(range)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let rest = s.get(19..)?;
    let millis = match rest.strip_prefix('.') {
        Some(frac) => {
            let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
            if digits.is_empty() || !frac[digits.len()..].eq_ignore_ascii_case("z") {
                return None;
            }
            format!("{digits:0<3}")[..3].parse::<i64>().ok()?
        }
        None if rest.eq_ignore_ascii_case("z") => 0,
        None => return None,
    };
    // Days from civil (proleptic Gregorian), Howard Hinnant's algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis)
}

/// Compares two filesystem paths the way the OS would: separators and a
/// trailing separator are ignored, so is the `\\?\` verbatim prefix, and on
/// Windows the comparison is case-insensitive.
fn same_path(a: &str, b: &str) -> bool {
    fn normalize(p: &str) -> String {
        let p = p.trim();
        let p = p.strip_prefix(r"\\?\").unwrap_or(p);
        let mut out = p.replace('\\', "/");
        while out.len() > 1 && out.ends_with('/') {
            out.pop();
        }
        if cfg!(windows) {
            out.to_lowercase()
        } else {
            out
        }
    }
    normalize(a) == normalize(b)
}

#[derive(Debug, Clone)]
struct CodexCandidate {
    meta: CodexMeta,
    modified_ms: i64,
}

/// Picks the rollout belonging to a Codex process.
///
/// Only top-level threads in the same cwd that were written after the
/// process started qualify. Among those, a thread created after the process
/// started is a new session of this process: the one created closest after
/// the start wins (so a second Codex started later in the same directory
/// does not steal the first one's session). If none was created after the
/// start, the process resumed an older thread: the most recently written one
/// wins. Known limitation: `/new` inside a running Codex keeps reporting the
/// first thread of that process.
fn select_codex_session(
    candidates: &[CodexCandidate],
    process_start_ms: Option<i64>,
    cwd: Option<&str>,
) -> Option<String> {
    let eligible: Vec<&CodexCandidate> = candidates
        .iter()
        .filter(|c| !c.meta.subagent)
        .filter(|c| match (cwd, c.meta.cwd.as_deref()) {
            (Some(want), Some(have)) => same_path(want, have),
            (Some(_), None) => false,
            (None, _) => true,
        })
        .filter(|c| process_start_ms.is_none_or(|start| c.modified_ms + START_SLACK_MS >= start))
        .collect();
    let newest_written = || {
        eligible
            .iter()
            .max_by_key(|c| c.modified_ms)
            .map(|c| c.meta.id.clone())
    };
    let Some(start) = process_start_ms else {
        return newest_written();
    };
    eligible
        .iter()
        .filter_map(|c| {
            let created = c.meta.started_ms?;
            (created + START_SLACK_MS >= start).then_some((created, c))
        })
        .min_by_key(|(created, _)| *created)
        .map(|(_, c)| c.meta.id.clone())
        .or_else(newest_written)
}

/// Thread id encoded in a rollout file name:
/// `rollout-YYYY-MM-DDThh-mm-ss-<thread id>[_<rollout id>].jsonl[.zst]`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn codex_thread_id_from_file_name(name: &str) -> Option<String> {
    let core = name
        .strip_suffix(".zst")
        .unwrap_or(name)
        .strip_prefix("rollout-")?
        .strip_suffix(".jsonl")?;
    if core.get(19..20)? != "-" {
        return None;
    }
    let ids = core.get(20..)?;
    let thread = ids.split_once('_').map_or(ids, |(thread, _)| thread);
    (thread.len() == 36).then(|| thread.to_owned())
}

fn is_plain_rollout_name(name: &str) -> bool {
    name.starts_with("rollout-") && name.ends_with(".jsonl")
}

fn read_codex_meta(path: &Path) -> Option<CodexMeta> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file.take(CODEX_HEAD_MAX));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).ok()?;
    parse_codex_meta(std::str::from_utf8(&line).ok()?)
}

fn modified_ms(meta: &std::fs::Metadata) -> Option<i64> {
    let since = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// Numeric subdirectories of `dir`, newest (highest) first.
fn numeric_subdirs_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let numeric = !name.is_empty() && name.bytes().all(|c| c.is_ascii_digit());
            (numeric && entry.file_type().ok()?.is_dir()).then(|| (name, entry.path()))
        })
        .collect();
    // Zero-padded names, but compare numerically anyway.
    dirs.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| b.cmp(a)));
    dirs.into_iter().map(|(_, path)| path).collect()
}

/// Plain rollout files of the newest `CODEX_DAY_DIRS` day directories under
/// `sessions/YYYY/MM/DD`, with their modification time. Never walks the
/// whole tree. Compressed (`.zst`) rollouts are archived ones and skipped.
fn recent_codex_rollouts(sessions_root: &Path) -> Vec<(PathBuf, i64)> {
    let mut day_dirs = Vec::new();
    'collect: for year in numeric_subdirs_desc(sessions_root).into_iter().take(2) {
        for month in numeric_subdirs_desc(&year).into_iter().take(3) {
            for day in numeric_subdirs_desc(&month) {
                day_dirs.push(day);
                if day_dirs.len() >= CODEX_DAY_DIRS {
                    break 'collect;
                }
            }
        }
    }
    let mut files = Vec::new();
    for dir in day_dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_str().is_some_and(is_plain_rollout_name) {
                continue;
            }
            if let Some(ms) = entry.metadata().ok().as_ref().and_then(modified_ms) {
                files.push((entry.path(), ms));
                if files.len() >= CODEX_MAX_FILES {
                    return files;
                }
            }
        }
    }
    files
}

/// Resolves state directories once per command call.
struct AgentDirs {
    claude_sessions: Option<PathBuf>,
    codex_sessions: Option<PathBuf>,
    /// Lazily listed rollout files, shared by every Codex terminal.
    codex_rollouts: Option<Vec<(PathBuf, i64)>>,
}

fn non_empty_env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        non_empty_env("USERPROFILE")
    }
    #[cfg(unix)]
    {
        non_empty_env("HOME")
    }
}

impl AgentDirs {
    /// `CLAUDE_CONFIG_DIR` replaces `~/.claude`; `CODEX_HOME` replaces
    /// `~/.codex`. Only Podium's own environment is visible here: a variable
    /// exported from the user's shell profile is not.
    fn from_env() -> Self {
        let home = home_dir();
        let claude_root =
            non_empty_env("CLAUDE_CONFIG_DIR").or_else(|| home.as_ref().map(|h| h.join(".claude")));
        let codex_root =
            non_empty_env("CODEX_HOME").or_else(|| home.as_ref().map(|h| h.join(".codex")));
        AgentDirs {
            claude_sessions: claude_root.map(|root| root.join("sessions")),
            codex_sessions: codex_root.map(|root| root.join("sessions")),
            codex_rollouts: None,
        }
    }

    fn read_claude_session(&self, pid: u32) -> Option<ClaudeSessionFile> {
        let path = self.claude_sessions.as_ref()?.join(format!("{pid}.json"));
        parse_claude_session(&std::fs::read_to_string(path).ok()?)
    }

    fn codex_session_id(&mut self, agent: &FoundAgent, cwd: Option<&str>) -> Option<String> {
        #[cfg(target_os = "linux")]
        if let Some(id) = codex_session_from_open_files(agent.pid) {
            return Some(id);
        }
        let root = self.codex_sessions.as_ref()?;
        let rollouts = self
            .codex_rollouts
            .get_or_insert_with(|| recent_codex_rollouts(root));
        let start_ms = agent.start.map(|s| s.epoch_ms);
        let mut files: Vec<&(PathBuf, i64)> = rollouts
            .iter()
            .filter(|(_, modified)| start_ms.is_none_or(|start| modified + START_SLACK_MS >= start))
            .collect();
        if start_ms.is_none() {
            // Without a start time only the latest writes are worth reading.
            files.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
            files.truncate(16);
        }
        let candidates: Vec<CodexCandidate> = files
            .into_iter()
            .filter_map(|(path, modified)| {
                Some(CodexCandidate {
                    meta: read_codex_meta(path)?,
                    modified_ms: *modified,
                })
            })
            .collect();
        select_codex_session(&candidates, start_ms, cwd)
    }
}

/// On Linux the running Codex keeps its rollout open, which identifies the
/// thread exactly.
#[cfg(target_os = "linux")]
fn codex_session_from_open_files(pid: u32) -> Option<String> {
    let entries = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
    let mut best: Option<(i64, String)> = None;
    for entry in entries.flatten().take(4096) {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_plain_rollout_name(name) {
            continue;
        }
        let Some(meta) = read_codex_meta(&target) else {
            continue;
        };
        if meta.subagent || codex_thread_id_from_file_name(name).is_none() {
            continue;
        }
        let modified = std::fs::metadata(&target)
            .ok()
            .as_ref()
            .and_then(modified_ms)
            .unwrap_or(i64::MIN);
        if best.as_ref().is_none_or(|(m, _)| modified > *m) {
            best = Some((modified, meta.id));
        }
    }
    best.map(|(_, id)| id)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn filetime_to_epoch_ms(filetime: u64) -> i64 {
    // 100 ns ticks since 1601-01-01 → ms since 1970-01-01.
    i64::try_from(filetime / 10_000).unwrap_or(i64::MAX) - 11_644_473_600_000
}

#[cfg(windows)]
fn process_start_windows(pid: u32) -> Option<ProcessStart> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: plain FFI call; a null handle signals failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: `handle` is a valid process handle opened above and every out
    // pointer refers to a live FILETIME.
    let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    // SAFETY: `handle` was returned by OpenProcess and is closed exactly once.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    let filetime = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Some(ProcessStart {
        epoch_ms: filetime_to_epoch_ms(filetime),
        filetime: Some(filetime),
    })
}

/// One Toolhelp snapshot (exe name + parent PID of every process). Start
/// times are fetched lazily, only for processes the walk asks about.
#[cfg(windows)]
fn system_processes() -> Option<ProcessTable> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: plain FFI call; failure is INVALID_HANDLE_VALUE.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return None;
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).ok()?,
        ..Default::default()
    };
    let mut entries = Vec::with_capacity(512);
    // SAFETY: `snapshot` is a valid Toolhelp handle and `entry.dwSize` is set
    // as the API requires.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let exe = &entry.szExeFile;
        let len = exe.iter().position(|&c| c == 0).unwrap_or(exe.len());
        entries.push(ProcEntry {
            pid: entry.th32ProcessID,
            ppid: entry.th32ParentProcessID,
            name: String::from_utf16_lossy(&exe[..len]),
            start: None,
        });
        // SAFETY: same handle and entry as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot handle is closed exactly once.
    unsafe { CloseHandle(snapshot) };
    Some(ProcessTable::new(entries, process_start_windows))
}

#[cfg(target_os = "macos")]
fn macos_bsd_info(pid: libc::c_int) -> Option<libc::proc_bsdinfo> {
    use std::mem::{size_of, MaybeUninit};

    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: the buffer is a zeroed proc_bsdinfo of exactly `size` bytes,
    // which is what PROC_PIDTBSDINFO writes into.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: proc_pidinfo filled the whole struct when it reports `size`.
    (written == size).then(|| unsafe { info.assume_init() })
}

#[cfg(target_os = "macos")]
fn c_chars_to_string(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Every PID with its parent, name and start time via libproc. Processes
/// owned by other users may be unreadable; they cannot be our descendants.
#[cfg(target_os = "macos")]
fn system_processes() -> Option<ProcessTable> {
    use std::mem::size_of;

    // SAFETY: a null buffer asks only for the current number of PIDs.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let count = usize::try_from(count).ok().filter(|&n| n > 0)?;
    let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
    let bytes = libc::c_int::try_from(pids.len() * size_of::<libc::pid_t>()).ok()?;
    // SAFETY: `pids` holds exactly `bytes` bytes of writable pid_t slots.
    let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    let listed = usize::try_from(listed).ok()?;
    pids.truncate(listed.min(pids.len()));

    let entries = pids
        .into_iter()
        .filter(|&pid| pid > 0)
        .filter_map(|pid| {
            let info = macos_bsd_info(pid)?;
            let name = c_chars_to_string(&info.pbi_name);
            let name = if name.is_empty() {
                c_chars_to_string(&info.pbi_comm)
            } else {
                name
            };
            let secs = i64::try_from(info.pbi_start_tvsec).ok()?;
            let micros = i64::try_from(info.pbi_start_tvusec).ok()?;
            Some(ProcEntry {
                pid: info.pbi_pid,
                ppid: info.pbi_ppid,
                name,
                start: Some(ProcessStart {
                    epoch_ms: secs * 1000 + micros / 1000,
                    filetime: None,
                }),
            })
        })
        .collect();
    Some(ProcessTable::new(entries, |_| None))
}

/// Fields of `/proc/<pid>/stat` used here.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcStat {
    comm: String,
    ppid: u32,
    /// Field 22: start time in clock ticks after boot.
    start_ticks: u64,
}

/// Parses `/proc/<pid>/stat`. `comm` sits in parentheses and may itself
/// contain spaces and parentheses, so it spans to the *last* `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_stat(content: &str) -> Option<ProcStat> {
    let open = content.find('(')?;
    let close = content.rfind(')')?;
    if close < open {
        return None;
    }
    content[..open].trim().parse::<u32>().ok()?;
    let comm = content[open + 1..close].to_owned();
    // Fields after comm start at field 3 (state).
    let rest: Vec<&str> = content[close + 1..].split_whitespace().collect();
    let ppid = rest.get(1)?.parse().ok()?;
    let start_ticks = rest.get(19)?.parse().ok()?;
    Some(ProcStat {
        comm,
        ppid,
        start_ticks,
    })
}

#[cfg(target_os = "linux")]
struct LinuxProcesses {
    stats: HashMap<u32, Option<ProcStat>>,
    /// Full `/proc` scan, used only when the kernel lacks
    /// `/proc/<pid>/task/<tid>/children` (CONFIG_PROC_CHILDREN).
    table: Option<ProcessTable>,
    ticks_per_sec: i64,
    boot_ms: Option<i64>,
}

#[cfg(target_os = "linux")]
fn linux_read_stat(pid: u32) -> Option<ProcStat> {
    parse_proc_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(target_os = "linux")]
fn linux_clock() -> (i64, Option<i64>) {
    // SAFETY: sysconf has no preconditions.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    // c_long is i64 on every 64-bit Linux target Podium builds for.
    let ticks: i64 = if ticks > 0 { ticks } else { 100 };
    let boot_ms = std::fs::read_to_string("/proc/stat").ok().and_then(|stat| {
        stat.lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|secs| secs.trim().parse::<i64>().ok())
            .map(|secs| secs * 1000)
    });
    (ticks, boot_ms)
}

#[cfg(target_os = "linux")]
fn linux_start_ms(
    stat: &ProcStat,
    ticks_per_sec: i64,
    boot_ms: Option<i64>,
) -> Option<ProcessStart> {
    let ticks = i64::try_from(stat.start_ticks).ok()?;
    Some(ProcessStart {
        epoch_ms: boot_ms? + ticks * 1000 / ticks_per_sec,
        filetime: None,
    })
}

#[cfg(target_os = "linux")]
fn linux_start_lookup(pid: u32) -> Option<ProcessStart> {
    let (ticks, boot_ms) = linux_clock();
    linux_start_ms(&linux_read_stat(pid)?, ticks, boot_ms)
}

#[cfg(target_os = "linux")]
fn system_processes() -> Option<LinuxProcesses> {
    let (ticks_per_sec, boot_ms) = linux_clock();
    let table = if Path::new("/proc/thread-self/children").exists() {
        None
    } else {
        let entries = std::fs::read_dir("/proc")
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
                let stat = linux_read_stat(pid)?;
                Some(ProcEntry {
                    pid,
                    ppid: stat.ppid,
                    start: linux_start_ms(&stat, ticks_per_sec, boot_ms),
                    name: stat.comm,
                })
            })
            .collect();
        Some(ProcessTable::new(entries, linux_start_lookup))
    };
    Some(LinuxProcesses {
        stats: HashMap::new(),
        table,
        ticks_per_sec,
        boot_ms,
    })
}

#[cfg(target_os = "linux")]
impl LinuxProcesses {
    fn stat(&mut self, pid: u32) -> Option<ProcStat> {
        self.stats
            .entry(pid)
            .or_insert_with(|| linux_read_stat(pid))
            .clone()
    }
}

#[cfg(target_os = "linux")]
impl ProcessSource for LinuxProcesses {
    fn children(&mut self, pid: u32) -> Vec<u32> {
        if let Some(table) = self.table.as_mut() {
            return table.children(pid);
        }
        // Children are listed per thread; read every task of the process.
        let mut out = Vec::new();
        if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
            for task in tasks.flatten().take(256) {
                if let Ok(list) = std::fs::read_to_string(task.path().join("children")) {
                    out.extend(
                        list.split_whitespace()
                            .filter_map(|p| p.parse::<u32>().ok()),
                    );
                }
            }
        }
        out
    }

    fn name(&mut self, pid: u32) -> Option<String> {
        if let Some(table) = self.table.as_mut() {
            return table.name(pid);
        }
        self.stat(pid).map(|stat| stat.comm)
    }

    fn start(&mut self, pid: u32) -> Option<ProcessStart> {
        if let Some(table) = self.table.as_mut() {
            return table.start(pid);
        }
        let stat = self.stat(pid)?;
        linux_start_ms(&stat, self.ticks_per_sec, self.boot_ms)
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn system_processes() -> Option<ProcessTable> {
    None
}

/// Detects the agent in one terminal and resolves its session id.
fn detect_agent<S: ProcessSource>(
    src: &mut S,
    dirs: &mut AgentDirs,
    shell_pid: u32,
    terminal_cwd: Option<&str>,
) -> Option<AgentSession> {
    let agent = find_agent(src, shell_pid, &mut |pid| dirs.read_claude_session(pid))?;
    let session_id = match agent.kind {
        AgentKind::Claude => agent.claude_session_id.clone(),
        AgentKind::Codex => {
            // The agent's own cwd when the OS exposes it; on Windows the
            // shell's last prompt directory (where the user launched it).
            #[cfg(unix)]
            let cwd = process_cwd(agent.pid).or_else(|| terminal_cwd.map(str::to_owned));
            #[cfg(windows)]
            let cwd = terminal_cwd.map(str::to_owned);
            dirs.codex_session_id(&agent, cwd.as_deref())
        }
        // OpenCode keeps sessions in its SQLite database behind a local
        // server; nothing cheaply ties a session to a running process.
        AgentKind::OpenCode => None,
    };
    Some(AgentSession {
        kind: agent.kind.as_str().to_owned(),
        session_id,
    })
}

/// Agent CLI (Claude Code, Codex, OpenCode) running in each terminal's shell,
/// keyed by terminal id; terminals without one are omitted. Queried on
/// demand (after a submitted line and on window close), never polled. Runs
/// off the main thread: the process snapshot takes a few milliseconds.
#[tauri::command(async)]
pub fn get_terminal_agents(
    manager: State<'_, PtyManager>,
    ids: Vec<String>,
) -> Result<HashMap<String, AgentSession>, String> {
    let targets: Vec<(String, u32, Option<String>)> = {
        let sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
        ids.into_iter()
            .filter_map(|id| {
                let session = sessions.get(&id)?;
                let pid = session.pid?;
                let cwd = session_cwd(session);
                Some((id, pid, cwd))
            })
            .collect()
    };
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let mut processes =
        system_processes().ok_or_else(|| String::from("process table unavailable"))?;
    let mut dirs = AgentDirs::from_env();
    Ok(targets
        .into_iter()
        .filter_map(|(id, pid, cwd)| {
            let agent = detect_agent(&mut processes, &mut dirs, pid, cwd.as_deref())?;
            Some((id, agent))
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

    fn start(epoch_ms: i64) -> Option<ProcessStart> {
        Some(ProcessStart {
            epoch_ms,
            filetime: None,
        })
    }

    /// Synthetic process table: (pid, ppid, name, start ms).
    fn table(rows: &[(u32, u32, &str, i64)]) -> ProcessTable {
        let entries = rows
            .iter()
            .map(|&(pid, ppid, name, ms)| ProcEntry {
                pid,
                ppid,
                name: name.to_owned(),
                start: start(ms),
            })
            .collect();
        ProcessTable::new(entries, |_| None)
    }

    fn claude_file(pid: u32, session: &str, started_at: i64) -> ClaudeSessionFile {
        ClaudeSessionFile {
            pid: Some(pid),
            session_id: Some(session.to_owned()),
            started_at: Some(started_at),
            proc_start: None,
        }
    }

    #[test]
    fn agent_names_are_matched_case_insensitively() {
        assert_eq!(agent_kind_from_name("claude"), Some(AgentKind::Claude));
        assert_eq!(agent_kind_from_name("Claude.EXE"), Some(AgentKind::Claude));
        assert_eq!(agent_kind_from_name("codex.exe"), Some(AgentKind::Codex));
        assert_eq!(agent_kind_from_name("opencode"), Some(AgentKind::OpenCode));
        assert_eq!(
            agent_kind_from_name("opencode2-node"),
            Some(AgentKind::OpenCode)
        );
        assert_eq!(agent_kind_from_name("node.exe"), None);
        assert_eq!(agent_kind_from_name("claude-helper"), None);
        assert_eq!(agent_kind_from_name("pwsh.exe"), None);
    }

    #[test]
    fn walk_finds_native_claude_under_the_shell() {
        // pwsh → claude.exe → (bash, cmd → node MCP server)
        let mut src = table(&[
            (10, 1, "pwsh.exe", 1_000),
            (20, 10, "claude.exe", 2_000),
            (30, 20, "bash.exe", 3_000),
            (31, 20, "cmd.exe", 3_000),
            (32, 31, "node.exe", 3_100),
            (99, 1, "claude.exe", 500),
        ]);
        let mut read = |pid: u32| (pid == 20).then(|| claude_file(20, "abc", 2_500));
        let agent = find_agent(&mut src, 10, &mut read).expect("agent");
        assert_eq!(agent.kind, AgentKind::Claude);
        assert_eq!(agent.pid, 20);
        assert_eq!(agent.claude_session_id.as_deref(), Some("abc"));
    }

    #[test]
    fn walk_detects_node_claude_by_session_file() {
        let mut src = table(&[(10, 1, "bash", 1_000), (20, 10, "node", 2_000)]);
        let mut read = |pid: u32| (pid == 20).then(|| claude_file(20, "from-node", 2_100));
        let agent = find_agent(&mut src, 10, &mut read).expect("agent");
        assert_eq!(agent.kind, AgentKind::Claude);
        assert_eq!(agent.claude_session_id.as_deref(), Some("from-node"));
    }

    #[test]
    fn walk_follows_shims_to_the_native_binary() {
        // cmd shim → node codex.js → codex.exe
        let mut src = table(&[
            (10, 1, "pwsh.exe", 1_000),
            (20, 10, "cmd.exe", 2_000),
            (21, 20, "node.exe", 2_010),
            (22, 21, "codex.exe", 2_050),
            (23, 22, "codex.exe", 2_060),
        ]);
        let agent = find_agent(&mut src, 10, &mut |_| None).expect("agent");
        assert_eq!(agent.kind, AgentKind::Codex);
        assert_eq!(agent.pid, 23);
        assert_eq!(agent.start, start(2_060));
    }

    #[test]
    fn walk_ignores_plain_shells_and_stale_session_files() {
        let mut src = table(&[
            (10, 1, "bash", 1_000),
            (20, 10, "vim", 2_000),
            (21, 10, "node", 3_000),
        ]);
        // PID 21 was reused: its session file predates the process.
        let mut read = |pid: u32| (pid == 21).then(|| claude_file(21, "stale", 100));
        assert_eq!(find_agent(&mut src, 10, &mut read), None);
    }

    #[test]
    fn walk_does_not_descend_into_an_agent() {
        // claude runs codex as a tool through bash: claude stays the agent.
        let mut src = table(&[
            (10, 1, "zsh", 1_000),
            (20, 10, "claude", 2_000),
            (30, 20, "bash", 3_000),
            (40, 30, "codex", 4_000),
        ]);
        let agent = find_agent(&mut src, 10, &mut |_| None).expect("agent");
        assert_eq!(agent.kind, AgentKind::Claude);
        assert_eq!(agent.claude_session_id, None);
    }

    #[test]
    fn walk_prefers_the_most_recent_sibling_and_rejects_reused_parents() {
        let mut src = table(&[
            (10, 1, "bash", 10_000),
            (20, 10, "opencode", 20_000),
            (21, 10, "codex", 30_000),
            // Started long before the shell: stale parent PID.
            (22, 10, "claude", 1_000),
        ]);
        let agent = find_agent(&mut src, 10, &mut |_| None).expect("agent");
        assert_eq!(agent.kind, AgentKind::Codex);
        assert_eq!(agent.pid, 21);
    }

    #[test]
    fn walk_is_bounded_in_depth() {
        let mut rows = vec![(10, 1, "bash", 1_000)];
        for depth in 0..10u32 {
            let parent = if depth == 0 { 10 } else { 99 + depth };
            rows.push((100 + depth, parent, "sh", 2_000));
        }
        rows.push((200, 109, "claude", 3_000));
        let mut src = table(&rows);
        assert_eq!(find_agent(&mut src, 10, &mut |_| None), None);
    }

    #[test]
    fn claude_session_file_is_parsed_leniently() {
        let json = r#"{"pid":11852,"sessionId":"6dccf4e4-a677-4b0e-a4d0-a1f5a9707a8e","cwd":"C:\\Users\\dev","startedAt":1792460558735,"procStart":"134369341562858832","version":"2.1.303","peerFeatures":["notify_idle"],"status":"busy"}"#;
        let file = parse_claude_session(json).expect("parsed");
        assert_eq!(file.pid, Some(11852));
        assert_eq!(
            file.session_id.as_deref(),
            Some("6dccf4e4-a677-4b0e-a4d0-a1f5a9707a8e")
        );
        assert_eq!(file.started_at, Some(1_792_460_558_735));
        assert_eq!(file.proc_start.as_deref(), Some("134369341562858832"));
        assert_eq!(
            parse_claude_session("{}"),
            Some(ClaudeSessionFile::default())
        );
        assert_eq!(parse_claude_session("not json"), None);
    }

    #[test]
    fn claude_session_validation_uses_filetime_then_started_at() {
        let mut file = claude_file(7, "s", 5_000);
        assert!(claude_session_matches(&file, 7, None));
        assert!(!claude_session_matches(&file, 8, None));
        assert!(claude_session_matches(&file, 7, start(4_000).as_ref()));
        assert!(!claude_session_matches(&file, 7, start(60_000).as_ref()));

        file.proc_start = Some(String::from("134369341562858832"));
        let exact = ProcessStart {
            epoch_ms: 60_000,
            filetime: Some(134_369_341_562_858_832),
        };
        assert!(claude_session_matches(&file, 7, Some(&exact)));
        let other = ProcessStart {
            filetime: Some(1),
            ..exact
        };
        assert!(!claude_session_matches(&file, 7, Some(&other)));
    }

    #[test]
    fn filetime_converts_to_unix_ms() {
        assert_eq!(filetime_to_epoch_ms(116_444_736_000_000_000), 0);
        assert_eq!(
            filetime_to_epoch_ms(134_369_341_562_858_832),
            1_792_460_556_285
        );
    }

    #[test]
    fn proc_stat_parsing_handles_odd_comm() {
        let line = "4242 (my (weird) cmd) S 4100 4242 4100 34816 4242 4194304 120 0 0 0 1 2 0 0 20 0 1 0 987654 1234567 89 18446744073709551615";
        let stat = parse_proc_stat(line).expect("parsed");
        assert_eq!(stat.comm, "my (weird) cmd");
        assert_eq!(stat.ppid, 4100);
        assert_eq!(stat.start_ticks, 987_654);
        assert_eq!(parse_proc_stat("12 (x) S 1"), None);
        assert_eq!(parse_proc_stat("garbage"), None);
    }

    #[test]
    fn utc_timestamps_parse_to_epoch_ms() {
        assert_eq!(parse_utc_timestamp_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_utc_timestamp_ms("2026-01-01T00:00:00.5Z"),
            Some(1_767_225_600_500)
        );
        assert_eq!(
            parse_utc_timestamp_ms("2024-02-29T12:34:56.789Z"),
            Some(1_709_210_096_789)
        );
        assert_eq!(parse_utc_timestamp_ms("2024-02-29T12:34:56+02:00"), None);
        assert_eq!(parse_utc_timestamp_ms("nonsense"), None);
    }

    const CODEX_ID: &str = "0199a1b2-0000-7000-8000-000000000001";
    const CODEX_META: &str = r#"{"timestamp":"2026-10-19T12:00:01.000Z","type":"session_meta","payload":{"session_id":"0199a1b2-0000-7000-8000-000000000001","id":"0199a1b2-0000-7000-8000-000000000001","timestamp":"2026-10-19T12:00:00.250Z","cwd":"C:\\work\\repo","originator":"codex_cli_rs","cli_version":"0.130.0","source":"cli","base_instructions":{"text":"..."}}}"#;

    #[test]
    fn codex_meta_line_is_parsed() {
        let meta = parse_codex_meta(CODEX_META).expect("meta");
        assert_eq!(meta.id, CODEX_ID);
        assert_eq!(meta.cwd.as_deref(), Some(r"C:\work\repo"));
        assert_eq!(
            meta.started_ms,
            parse_utc_timestamp_ms("2026-10-19T12:00:00.250Z")
        );
        assert!(!meta.subagent);

        let sub = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"x","cwd":"/r","timestamp":"2026-10-19T12:00:00Z","source":{"subagent":{"thread_spawn":{}}},"parent_thread_id":"p"}}"#;
        assert!(parse_codex_meta(sub).expect("meta").subagent);
        let other = r#"{"timestamp":"t","type":"response_item","payload":{"id":"x"}}"#;
        assert_eq!(parse_codex_meta(other), None);
        assert_eq!(parse_codex_meta("{"), None);
    }

    #[test]
    fn codex_thread_id_comes_from_the_file_name() {
        let plain = format!("rollout-2026-10-19T09-00-00-{CODEX_ID}.jsonl");
        assert_eq!(
            codex_thread_id_from_file_name(&plain).as_deref(),
            Some(CODEX_ID)
        );
        let reverted = format!(
            "rollout-2026-10-19T09-00-00-{CODEX_ID}_0199a1b2-0000-7000-8000-000000000002.jsonl.zst"
        );
        assert_eq!(
            codex_thread_id_from_file_name(&reverted).as_deref(),
            Some(CODEX_ID)
        );
        assert_eq!(codex_thread_id_from_file_name("rollout-bad.jsonl"), None);
        assert!(is_plain_rollout_name(&plain));
        assert!(!is_plain_rollout_name(&reverted));
    }

    fn candidate(
        id: &str,
        cwd: &str,
        started: i64,
        modified: i64,
        subagent: bool,
    ) -> CodexCandidate {
        CodexCandidate {
            meta: CodexMeta {
                id: id.to_owned(),
                cwd: Some(cwd.to_owned()),
                started_ms: Some(started),
                subagent,
            },
            modified_ms: modified,
        }
    }

    #[test]
    fn codex_matching_prefers_first_thread_created_after_start() {
        let candidates = [
            candidate("old", "/repo", 1_000, 1_500, false),
            candidate("mine", "/repo", 10_500, 20_000, false),
            candidate("later-instance", "/repo", 15_000, 25_000, false),
            candidate("sub", "/repo", 10_100, 26_000, true),
            candidate("elsewhere", "/other", 10_200, 27_000, false),
        ];
        assert_eq!(
            select_codex_session(&candidates, Some(10_000), Some("/repo/")).as_deref(),
            Some("mine")
        );
        assert_eq!(
            select_codex_session(&candidates, Some(14_000), Some("/repo")).as_deref(),
            Some("later-instance")
        );
    }

    #[test]
    fn codex_matching_falls_back_to_resumed_thread() {
        let candidates = [
            candidate("resumed", "/repo", 1_000, 50_000, false),
            candidate("untouched", "/repo", 2_000, 3_000, false),
        ];
        assert_eq!(
            select_codex_session(&candidates, Some(40_000), Some("/repo")).as_deref(),
            Some("resumed")
        );
        assert_eq!(
            select_codex_session(&candidates, Some(60_000), Some("/repo")),
            None
        );
        assert_eq!(
            select_codex_session(&candidates, None, None).as_deref(),
            Some("resumed")
        );
    }

    #[test]
    fn paths_compare_like_the_os() {
        assert!(same_path("/a/b/", "/a/b"));
        assert!(same_path(r"\\?\C:\Work\Repo", r"C:\Work\Repo\"));
        assert!(same_path(r"C:\work\repo", "C:/work/repo"));
        assert!(!same_path("/a/b", "/a/c"));
        assert_eq!(same_path(r"C:\Work", r"c:\work"), cfg!(windows));
    }

    #[test]
    fn codex_rollouts_listing_takes_newest_day_dirs() {
        let root = std::env::temp_dir().join(format!("podium-codex-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (year, month, days) in [("2025", "12", 1..=31u32), ("2026", "01", 1..=3u32)] {
            for day in days {
                let dir = root.join(year).join(month).join(format!("{day:02}"));
                std::fs::create_dir_all(&dir).expect("mkdir");
                let name = format!("rollout-{year}-{month}-{day:02}T09-00-00-{CODEX_ID}.jsonl");
                std::fs::write(dir.join(name), format!("{CODEX_META}\n{{}}\n")).expect("write");
            }
        }
        std::fs::write(
            root.join("2026").join("01").join("03").join("notes.txt"),
            "x",
        )
        .expect("write");

        let files = recent_codex_rollouts(&root);
        let mut days: Vec<String> = files
            .iter()
            .filter_map(|(path, _)| {
                let day = path.parent()?.file_name()?;
                Some(day.to_string_lossy().into_owned())
            })
            .collect();
        days.sort();
        assert_eq!(days, ["01", "02", "03", "28", "29", "30", "31"]);
        assert_eq!(
            read_codex_meta(&files[0].0).map(|m| m.id).as_deref(),
            Some(CODEX_ID)
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
