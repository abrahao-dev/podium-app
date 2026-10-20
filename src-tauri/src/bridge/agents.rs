//! Per-terminal agent state, message delivery and reply tracking.
//!
//! A terminal is `Idle` when it can take a prompt. Claude Code reports its
//! turns through hooks (exact). Agents without hooks fall back to output
//! silence; plain PowerShell terminals to their prompt redraw (OSC 9;9).
//! Messages wait in an inbox until the target is idle, so a busy agent is
//! never interrupted.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Quiet period after which an agent without hooks counts as done.
const SILENCE_IDLE: Duration = Duration::from_millis(3500);
/// Quiet period after which a freshly spawned terminal counts as ready.
const BOOT_SETTLE: Duration = Duration::from_millis(2500);
/// A terminal that never printed anything is assumed ready after this.
const BOOT_MAX: Duration = Duration::from_secs(20);
/// Agent TUIs boot for at least this long after the spawn.
const AGENT_BOOT_MIN: Duration = Duration::from_secs(3);
/// An agent TUI that never signalled readiness is assumed ready after this.
const AGENT_BOOT_MAX: Duration = Duration::from_secs(90);
/// Minimum time a delivered prompt is considered in flight.
const DELIVERY_GRACE: Duration = Duration::from_millis(1500);

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum AgentState {
    Starting,
    Idle,
    Working,
    WaitingInput,
    Exited,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Starting => "starting",
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::WaitingInput => "waiting for input",
            AgentState::Exited => "exited",
        }
    }
}

/// Output activity, updated lock-free by the PTY reader thread.
#[derive(Default)]
pub struct Activity {
    /// Unix ms of the last output chunk (0 = none yet).
    last_output_ms: AtomicU64,
    /// Shell prompt redraws seen (PowerShell OSC 9;9).
    prompts: AtomicU64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

impl Activity {
    pub fn output(&self) {
        self.last_output_ms.store(now_ms(), Ordering::Relaxed);
    }

    pub fn prompt(&self) {
        self.prompts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn quiet_for(&self, now: u64) -> Option<Duration> {
        match self.last_output_ms.load(Ordering::Relaxed) {
            0 => None,
            last => Some(Duration::from_millis(now.saturating_sub(last))),
        }
    }
}

pub struct Envelope {
    /// Sender terminal id (None = Podium itself).
    pub from: Option<String>,
    pub text: String,
    /// Write as keystrokes, unwrapped and without submitting.
    pub raw: bool,
    /// Ask this message belongs to; marked delivered when it is written.
    pub ask: Option<u64>,
}

pub enum AskOutcome {
    Reply(String),
    /// The target finished its turn without an explicit reply.
    Finished,
    Exited,
}

pub struct Ask {
    pub id: u64,
    pub from: String,
    pub to: String,
    /// None once the asker stopped waiting; the outcome is then delivered
    /// to the asker as a message instead.
    pub waiter: Option<mpsc::Sender<AskOutcome>>,
    pub delivered: bool,
}

pub struct AgentEntry {
    pub activity: Arc<Activity>,
    pub state: AgentState,
    pub since: Instant,
    /// Hooks have reported at least once: trust them over silence.
    pub hooked: bool,
    /// "claude" | "codex" | "opencode" | "shell", when known.
    pub kind: Option<String>,
    pub inbox: VecDeque<Envelope>,
    delivered_at: Option<Instant>,
    prompts_at_delivery: u64,
    spawned_at: Instant,
    /// Terminal that last sent this one a prompt (default `reply` target).
    pub last_sender: Option<String>,
    /// Watchdog already reported this terminal as stuck in its current state.
    pub stuck_reported: bool,
}

impl AgentEntry {
    pub fn new(activity: Arc<Activity>) -> Self {
        let now = Instant::now();
        AgentEntry {
            activity,
            state: AgentState::Starting,
            since: now,
            hooked: false,
            kind: None,
            inbox: VecDeque::new(),
            delivered_at: None,
            prompts_at_delivery: 0,
            spawned_at: now,
            last_sender: None,
            stuck_reported: false,
        }
    }

    pub fn is_agent(&self) -> bool {
        matches!(self.kind.as_deref(), Some("claude" | "codex" | "opencode"))
    }

    pub fn set_state(&mut self, state: AgentState) -> bool {
        if self.state == state {
            return false;
        }
        self.state = state;
        self.since = Instant::now();
        self.stuck_reported = false;
        if state != AgentState::Working {
            self.delivered_at = None;
        }
        true
    }

    pub fn mark_delivered(&mut self) {
        self.set_state(AgentState::Working);
        self.delivered_at = Some(Instant::now());
        self.prompts_at_delivery = self.activity.prompts.load(Ordering::Relaxed);
    }

    /// Silence / prompt based transitions; returns the new state if changed.
    /// `tui_ready` reports whether the program in the terminal enabled
    /// bracketed paste, which agent TUIs do once their input is live (a bare
    /// shell prompt does not), so a prompt is never pasted into the shell
    /// before the agent has started.
    pub fn tick(&mut self, now_unix: u64, tui_ready: impl Fn() -> bool) -> Option<AgentState> {
        let quiet = self.activity.quiet_for(now_unix);
        let next = match self.state {
            AgentState::Starting if !self.hooked => {
                let settled = quiet.is_some_and(|q| q >= BOOT_SETTLE);
                let age = self.spawned_at.elapsed();
                let ready = if self.is_agent() {
                    (settled && age >= AGENT_BOOT_MIN && tui_ready()) || age >= AGENT_BOOT_MAX
                } else {
                    settled || age >= BOOT_MAX
                };
                ready.then_some(AgentState::Idle)
            }
            AgentState::Working if !self.hooked => {
                let elapsed = self.delivered_at?.elapsed();
                let prompt_back = self.kind.as_deref() == Some("shell")
                    && elapsed >= Duration::from_millis(300)
                    && self.activity.prompts.load(Ordering::Relaxed) > self.prompts_at_delivery;
                let silent =
                    elapsed >= DELIVERY_GRACE && quiet.is_none_or(|q| q >= SILENCE_IDLE);
                (prompt_back || silent).then_some(AgentState::Idle)
            }
            _ => None,
        };
        let next = next?;
        self.set_state(next).then_some(next)
    }
}

/// All agent bookkeeping, guarded by the bridge state mutex.
#[derive(Default)]
pub struct Agents {
    pub entries: HashMap<String, AgentEntry>,
    pub asks: Vec<Ask>,
    next_ask: u64,
}

impl Agents {
    pub fn new_ask(&mut self, from: &str, to: &str, waiter: mpsc::Sender<AskOutcome>) -> u64 {
        self.next_ask += 1;
        let id = self.next_ask;
        self.asks.push(Ask {
            id,
            from: from.to_owned(),
            to: to.to_owned(),
            waiter: Some(waiter),
            delivered: false,
        });
        id
    }

    /// Resolves asks addressed to `to` (optionally only from `from`).
    /// Returns the outcomes nobody is waiting for anymore, as (asker, outcome).
    pub fn resolve<F>(&mut self, to: &str, from: Option<&str>, only_delivered: bool, outcome: F) -> Vec<(String, AskOutcome)>
    where
        F: Fn() -> AskOutcome,
    {
        let mut orphaned = Vec::new();
        self.asks.retain(|ask| {
            let matches = ask.to == to
                && from.is_none_or(|f| ask.from == f)
                && (!only_delivered || ask.delivered);
            if !matches {
                return true;
            }
            match &ask.waiter {
                Some(waiter) => {
                    if waiter.send(outcome()).is_err() {
                        orphaned.push((ask.from.clone(), outcome()));
                    }
                }
                None => orphaned.push((ask.from.clone(), outcome())),
            }
            false
        });
        orphaned
    }

    pub fn mark_ask_delivered(&mut self, ask_id: u64) {
        if let Some(ask) = self.asks.iter_mut().find(|a| a.id == ask_id) {
            ask.delivered = true;
        }
    }

    pub fn detach(&mut self, ask_id: u64) {
        if let Some(ask) = self.asks.iter_mut().find(|a| a.id == ask_id) {
            ask.waiter = None;
        }
    }

    pub fn pending_from(&self, asker: &str, target: &str) -> bool {
        self.asks.iter().any(|a| a.from == asker && a.to == target)
    }
}
