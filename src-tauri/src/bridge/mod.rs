//! Agent bridge: the local server the `podium` CLI talks to.
//!
//! Every terminal gets a random token in its environment; a CLI request
//! carrying that token acts as that terminal. Requests are one JSON line
//! (`{"v":1,"token","argv","cwd","stdin"}`), answered with one JSON line
//! (`{"stdout","stderr","exit"}`). All parsing happens here so the CLI never
//! changes with the command set.
//!
//! The canvas graph (who is connected to what, note names, roles) is owned by
//! the frontend and mirrored here through `bridge_sync_graph`; operations
//! that change the canvas (notes, spawning) are forwarded to the frontend and
//! awaited. Timing-sensitive work (delivery, idle detection, the watchdog)
//! lives in Rust: a hidden webview throttles its timers, and agents must keep
//! being served overnight.

mod agents;
mod args;
mod commands;
mod graph;
pub mod launch;

use agents::{Activity, AgentEntry, AgentState, Agents, AskOutcome, Envelope};
pub use graph::GraphSnapshot;
use graph::Graph;
pub use launch::OrchestrationSettings;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::pty::PtyManager;

/// Requests above this size are rejected (notes are the largest payloads).
const MAX_REQUEST_BYTES: u64 = 8 * 1024 * 1024;
/// Frontend round trips (note edits, spawning) answer well within this.
const UI_TIMEOUT: Duration = Duration::from_secs(30);
/// Watchdog period: idle detection and inbox delivery.
const TICK: Duration = Duration::from_millis(250);
/// Appended to prompts that expect an answer.
const REPLY_FOOTER: &str = "\n\n(When you are done, answer with: podium reply \"<your answer>\". For a long answer, write it to a file and run: podium reply --file <path>)";
/// Pause between pasting a prompt and submitting it, so the TUI has
/// finished handling the paste before Enter arrives.
const SUBMIT_DELAY: Duration = Duration::from_millis(150);

pub struct Bridge {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    app: AppHandle,
    port: u16,
    /// Directory holding the `podium` executable, prepended to each
    /// terminal's PATH.
    cli_dir: Option<PathBuf>,
    /// Claude Code settings file with Podium's hooks (`--settings`).
    claude_settings: Option<PathBuf>,
    state: Mutex<BridgeState>,
    ui_seq: AtomicU64,
    ui_pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
}

#[derive(Default)]
pub(crate) struct BridgeState {
    /// token → terminal id
    tokens: HashMap<String, String>,
    graph: Graph,
    agents: Agents,
    settings: OrchestrationSettings,
}

/// Outcome of one CLI request.
pub struct Reply {
    pub stdout: String,
    pub stderr: String,
    pub exit: i32,
}

impl Reply {
    pub fn ok(text: impl Into<String>) -> Self {
        let mut stdout = text.into();
        if !stdout.is_empty() && !stdout.ends_with('\n') {
            stdout.push('\n');
        }
        Reply {
            stdout,
            stderr: String::new(),
            exit: 0,
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        let mut stderr = format!("podium: {}", text.into());
        if !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        Reply {
            stdout: String::new(),
            stderr,
            exit: 1,
        }
    }

    /// Nothing printed, success (hooks).
    pub fn silent() -> Self {
        Reply {
            stdout: String::new(),
            stderr: String::new(),
            exit: 0,
        }
    }
}

#[derive(Deserialize)]
struct Request {
    token: String,
    argv: Vec<String>,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    stdin: Option<String>,
}

/// Context of one authenticated request.
pub struct Caller {
    pub id: String,
    pub cwd: String,
    pub stdin: Option<String>,
}

impl Bridge {
    /// Binds the server on an ephemeral loopback port and starts accepting.
    pub fn start(app: AppHandle) -> Result<Self, String> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|e| format!("agent bridge: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let cli_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(PathBuf::from));
        let claude_settings = app
            .path()
            .app_data_dir()
            .ok()
            .and_then(|dir| launch::write_claude_settings(&dir).ok());
        let inner = Arc::new(Inner {
            app,
            port,
            cli_dir,
            claude_settings,
            state: Mutex::new(BridgeState::default()),
            ui_seq: AtomicU64::new(1),
            ui_pending: Mutex::new(HashMap::new()),
        });

        let server = Arc::clone(&inner);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let inner = Arc::clone(&server);
                std::thread::spawn(move || {
                    let _ = handle_connection(&inner, stream);
                });
            }
        });

        let watchdog = Arc::clone(&inner);
        std::thread::spawn(move || loop {
            std::thread::sleep(TICK);
            watchdog.tick();
        });

        Ok(Bridge { inner })
    }

    /// Registers a terminal about to be spawned. Returns the environment
    /// that lets agents inside it reach the bridge as that terminal, and the
    /// activity counters its PTY reader must update.
    pub fn register_terminal(&self, id: &str) -> (Vec<(String, String)>, Arc<Activity>) {
        let token = random_token();
        let activity = Arc::new(Activity::default());
        if let Ok(mut state) = self.inner.state.lock() {
            state.tokens.retain(|_, owner| owner != id);
            state.tokens.insert(token.clone(), id.to_owned());
            let previous_kind = state.agents.entries.get(id).and_then(|e| e.kind.clone());
            let mut entry = AgentEntry::new(Arc::clone(&activity));
            entry.kind = previous_kind;
            state.agents.entries.insert(id.to_owned(), entry);
        }
        self.inner.emit_state(id, AgentState::Starting);

        let mut env = vec![
            (String::from("PODIUM_PORT"), self.inner.port.to_string()),
            (String::from("PODIUM_TOKEN"), token),
            (String::from("PODIUM_TERMINAL_ID"), id.to_owned()),
        ];
        if let Some(path) = &self.inner.claude_settings {
            env.push((
                String::from("PODIUM_CLAUDE_SETTINGS"),
                path.to_string_lossy().into_owned(),
            ));
        }
        if let Some(dir) = &self.inner.cli_dir {
            let exe = dir.join(if cfg!(windows) { "podium.exe" } else { "podium" });
            env.push((String::from("PODIUM_CLI"), exe.to_string_lossy().into_owned()));
            let mut paths = vec![dir.clone()];
            if let Some(existing) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&existing));
            }
            if let Ok(joined) = std::env::join_paths(paths) {
                env.push((String::from("PATH"), joined.to_string_lossy().into_owned()));
            }
        }
        (env, activity)
    }

    /// The terminal's process is gone: its token stops working and anyone
    /// waiting on it is told.
    pub fn unregister_terminal(&self, id: &str, activity: &Arc<Activity>) {
        let orphaned = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            // A newer process may have been registered under the same id.
            let current = state
                .agents
                .entries
                .get(id)
                .is_some_and(|e| Arc::ptr_eq(&e.activity, activity));
            if !current {
                return;
            }
            state.tokens.retain(|_, owner| owner != id);
            if let Some(entry) = state.agents.entries.get_mut(id) {
                entry.set_state(AgentState::Exited);
                entry.inbox.clear();
            }
            state.agents.resolve(id, None, false, || AskOutcome::Exited)
        };
        self.inner.emit_state(id, AgentState::Exited);
        self.inner.notify_orphans(id, orphaned);
    }
}

impl Inner {
    /// Asks the frontend to perform a canvas operation and waits for it.
    pub(crate) fn ui_request(&self, kind: &str, payload: Value) -> Result<Value, String> {
        let id = self.ui_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.ui_pending
            .lock()
            .map_err(|e| e.to_string())?
            .insert(id, tx);
        let emitted = self.app.emit(
            "bridge-request",
            json!({ "id": id, "kind": kind, "payload": payload }),
        );
        let result = match emitted {
            Ok(()) => rx
                .recv_timeout(UI_TIMEOUT)
                .unwrap_or_else(|_| Err(String::from("the Podium window did not respond"))),
            Err(e) => Err(e.to_string()),
        };
        if let Ok(mut pending) = self.ui_pending.lock() {
            pending.remove(&id);
        }
        result
    }

    pub(crate) fn graph(&self) -> Graph {
        self.state
            .lock()
            .map(|s| s.graph.clone())
            .unwrap_or_default()
    }

    pub(crate) fn settings(&self) -> OrchestrationSettings {
        self.state
            .lock()
            .map(|s| s.settings.clone())
            .unwrap_or_default()
    }

    pub(crate) fn name_of(&self, id: &str) -> String {
        self.graph()
            .node(id)
            .map_or_else(|| String::from("a terminal"), |n| n.name.clone())
    }

    pub(crate) fn pty(&self) -> State<'_, PtyManager> {
        self.app.state::<PtyManager>()
    }

    pub(crate) fn emit_state(&self, id: &str, state: AgentState) {
        let _ = self
            .app
            .emit("agent-state", json!({ "id": id, "state": state }));
    }

    pub(crate) fn agent_state(&self, id: &str) -> Option<AgentState> {
        self.state
            .lock()
            .ok()?
            .agents
            .entries
            .get(id)
            .map(|e| e.state)
    }

    /// Queues a message for a terminal; it is written when the terminal is
    /// idle (raw input right away).
    pub(crate) fn enqueue(&self, to: &str, envelope: Envelope) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.agents.entries.get_mut(to) {
                if let Some(from) = &envelope.from {
                    entry.last_sender = Some(from.clone());
                }
                entry.inbox.push_back(envelope);
            }
        }
        self.tick();
    }

    /// One watchdog pass: silence-based transitions, then deliveries.
    pub(crate) fn tick(&self) {
        let now = agents::now_ms();
        let mut changes = Vec::new();
        let mut deliveries: Vec<(String, Envelope)> = Vec::new();
        let mut orphaned = Vec::new();
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let BridgeState { agents, .. } = &mut *state;
            let pty = self.pty();
            let mut finished = Vec::new();
            for (id, entry) in &mut agents.entries {
                let before = entry.state;
                if let Some(next) = entry.tick(now, || pty.bracketed_paste(id)) {
                    changes.push((id.clone(), next));
                    if before == AgentState::Working && next == AgentState::Idle {
                        finished.push(id.clone());
                    }
                }
            }
            // Settle asks of turns that just ended before anything new is
            // delivered, so a fresh ask is never mistaken for the old turn.
            for id in finished {
                let settled = agents.resolve(&id, None, true, || AskOutcome::Finished);
                if !settled.is_empty() {
                    orphaned.push((id, settled));
                }
            }
            let mut delivered_asks = Vec::new();
            for (id, entry) in &mut agents.entries {
                let ready = matches!(entry.state, AgentState::Idle);
                let raw_first = entry.inbox.front().is_some_and(|e| e.raw);
                if (ready || raw_first) && entry.state != AgentState::Exited {
                    if let Some(envelope) = entry.inbox.pop_front() {
                        if !envelope.raw {
                            entry.mark_delivered();
                            changes.push((id.clone(), AgentState::Working));
                        }
                        delivered_asks.extend(envelope.ask);
                        deliveries.push((id.clone(), envelope));
                    }
                }
            }
            for ask in delivered_asks {
                agents.mark_ask_delivered(ask);
            }
        }
        for (id, state) in changes {
            self.emit_state(&id, state);
        }
        for (id, settled) in orphaned {
            self.notify_orphans(&id, settled);
        }
        for (id, envelope) in deliveries {
            self.deliver(&id, envelope);
        }
    }

    /// Writes one message into a terminal.
    fn deliver(&self, id: &str, envelope: Envelope) {
        let pty = self.pty();
        if envelope.raw {
            let _ = pty.write_bytes(id, envelope.text.as_bytes());
            return;
        }
        // TUIs that enabled bracketed paste take the whole prompt, newlines
        // included, as one input; Enter then submits it. A bare shell gets
        // the text alone as one command line.
        if pty.bracketed_paste(id) {
            let mut text = match &envelope.from {
                Some(from) => format!(
                    "[Podium] Message from \"{}\":\n{}",
                    self.name_of(from),
                    envelope.text
                ),
                None => envelope.text,
            };
            if envelope.ask.is_some() {
                text.push_str(REPLY_FOOTER);
            }
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(text.replace("\r\n", "\n").as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            if pty.write_bytes(id, &bytes).is_err() {
                return;
            }
            std::thread::sleep(SUBMIT_DELAY);
            let _ = pty.write_bytes(id, b"\r");
        } else if envelope.from.is_some() {
            // Podium's own notices are for agents; typed into a bare shell
            // they would only run as a bogus command.
            let line = envelope.text.replace("\r\n", "\n").replace('\n', " ");
            let _ = pty.write_bytes(id, format!("{line}\r").as_bytes());
        }
    }

    /// A terminal went from working to idle: settle asks waiting on it.
    pub(crate) fn on_turn_finished(&self, id: &str) {
        let orphaned = match self.state.lock() {
            Ok(mut state) => state.agents.resolve(id, None, true, || AskOutcome::Finished),
            Err(_) => return,
        };
        self.notify_orphans(id, orphaned);
    }

    /// Outcomes of asks whose asker stopped waiting reach it as messages.
    pub(crate) fn notify_orphans(&self, target: &str, orphaned: Vec<(String, AskOutcome)>) {
        let name = self.name_of(target);
        for (asker, outcome) in orphaned {
            let text = match outcome {
                AskOutcome::Reply(reply) => format!("[Podium] Reply from \"{name}\":\n{reply}"),
                AskOutcome::Finished => format!(
                    "[Podium] \"{name}\" finished its turn without replying. Read its screen with: podium check \"{name}\""
                ),
                AskOutcome::Exited => format!("[Podium] \"{name}\" exited before replying."),
            };
            self.enqueue(
                &asker,
                Envelope {
                    from: None,
                    text,
                    raw: false,
                    ask: None,
                },
            );
        }
    }

    /// Hook event or explicit state from an agent.
    pub(crate) fn set_agent_state(&self, id: &str, next: AgentState, hooked: bool) {
        let (changed, finished) = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.agents.entries.get_mut(id) else {
                return;
            };
            if hooked {
                entry.hooked = true;
            }
            let was_working = entry.state == AgentState::Working;
            let changed = entry.set_state(next);
            (changed, changed && was_working && next == AgentState::Idle)
        };
        if changed {
            self.emit_state(id, next);
        }
        if finished {
            self.on_turn_finished(id);
        }
        self.tick();
    }
}

fn random_token() -> String {
    // RandomState is seeded from the OS RNG per instance; four independent
    // hashers give 256 bits without pulling in a crypto dependency.
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    (0..4u64)
        .map(|i| {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(i);
            hasher.write_u64(agents::now_ms());
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

fn handle_connection(inner: &Arc<Inner>, stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let reply = match serde_json::from_str::<Request>(line.trim_end()) {
        Ok(request) => dispatch(inner, request),
        Err(e) => Reply::err(format!("malformed request ({e})")),
    };
    let mut out = stream;
    let mut body = json!({
        "stdout": reply.stdout,
        "stderr": reply.stderr,
        "exit": reply.exit,
    })
    .to_string();
    body.push('\n');
    out.write_all(body.as_bytes())?;
    out.flush()
}

fn dispatch(inner: &Arc<Inner>, request: Request) -> Reply {
    let caller_id = inner
        .state
        .lock()
        .ok()
        .and_then(|s| s.tokens.get(&request.token).cloned());
    let Some(id) = caller_id else {
        return Reply::err("unknown terminal token (was this terminal restarted?)");
    };
    let caller = Caller {
        id,
        cwd: request.cwd,
        stdin: request.stdin,
    };
    commands::run(inner, &caller, &request.argv)
}

/// Mirrors the frontend's canvas graph.
#[tauri::command]
pub fn bridge_sync_graph(bridge: State<'_, Bridge>, graph: GraphSnapshot) -> Result<(), String> {
    let mut state = bridge.inner.state.lock().map_err(|e| e.to_string())?;
    state.graph = Graph::from(graph);
    Ok(())
}

/// Mirrors the orchestration part of the app settings.
#[tauri::command]
pub fn bridge_sync_settings(
    bridge: State<'_, Bridge>,
    settings: OrchestrationSettings,
) -> Result<(), String> {
    let mut state = bridge.inner.state.lock().map_err(|e| e.to_string())?;
    state.settings = settings;
    Ok(())
}

/// Frontend's answer to a `bridge-request` event.
#[tauri::command]
pub fn bridge_respond(
    bridge: State<'_, Bridge>,
    id: u64,
    ok: bool,
    value: Value,
) -> Result<(), String> {
    let sender = bridge
        .inner
        .ui_pending
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&id);
    if let Some(sender) = sender {
        let result = if ok {
            Ok(value)
        } else {
            Err(value.as_str().unwrap_or("request failed").to_owned())
        };
        let _ = sender.send(result);
    }
    Ok(())
}

/// Current state of every known terminal (frontend badges after reload).
#[tauri::command]
pub fn bridge_agent_states(bridge: State<'_, Bridge>) -> Result<HashMap<String, AgentState>, String> {
    let state = bridge.inner.state.lock().map_err(|e| e.to_string())?;
    Ok(state
        .agents
        .entries
        .iter()
        .map(|(id, e)| (id.clone(), e.state))
        .collect())
}

/// Shell command that starts an agent wired to the bridge (hooks, autonomy
/// flags), for terminals the user starts from the toolbar.
#[tauri::command]
pub fn bridge_agent_command(bridge: State<'_, Bridge>, agent: String, recruit: bool) -> String {
    let settings = bridge.inner.settings();
    launch::agent_command(&agent, &settings, bridge.inner.claude_settings.as_deref(), recruit, None)
}
