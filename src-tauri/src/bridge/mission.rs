//! Missions: an orchestrator's task board, executed by recruits in git
//! worktrees and integrated through a merge queue.
//!
//! The board lives here, not in the orchestrator's context: it is persisted
//! after every change, so a compacted or restarted orchestrator picks up
//! with `podium mission status`. Each task gets its own worktree and branch
//! off the mission's integration branch; finished branches are merged one at
//! a time and checked with the mission's verify command before they count.
//! Conflicts go back to the worker that wrote the change, with the other
//! side's intent; repeated failures escalate to the orchestrator and then to
//! the user. Nothing is ever pushed.

use super::agents::{AgentState, Envelope};
use super::Inner;
use crate::worktree::{self, MergeOutcome};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::Emitter;

const SETUP_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const OUTPUT_TAIL_LINES: usize = 60;
/// Merge/verify failures a worker may fix before the orchestrator is told.
const WORKER_ATTEMPTS: u32 = 2;
/// Total failures before the task is left for the user.
const MAX_ATTEMPTS: u32 = 4;
/// A worker idle this long without `podium done` is nudged once.
const IDLE_NUDGE_MS: u64 = 90_000;
/// After the nudge, idling this long again is reported to the orchestrator.
const IDLE_REPORT_MS: u64 = 6 * 60_000;
/// Agents that hit a usage limit are prompted to continue this often.
const LIMIT_RETRY_MS: u64 = 15 * 60_000;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Pending,
    Ready,
    Starting,
    Running,
    Merging,
    Done,
    Failed,
    NeedsHuman,
    Cancelled,
}

impl TaskStatus {
    fn label(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Ready => "ready",
            TaskStatus::Starting => "starting",
            TaskStatus::Running => "running",
            TaskStatus::Merging => "merging",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::NeedsHuman => "needs human",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    fn active(self) -> bool {
        matches!(self, TaskStatus::Starting | TaskStatus::Running | TaskStatus::Merging)
    }

    fn settled(self) -> bool {
        matches!(
            self,
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::NeedsHuman | TaskStatus::Cancelled
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub files: Vec<String>,
    pub deps: Vec<String>,
    pub agent: String,
    pub model: Option<String>,
    pub status: TaskStatus,
    pub assignee: Option<String>,
    pub worker_name: Option<String>,
    pub branch: String,
    pub worktree: PathBuf,
    pub attempts: u32,
    pub summary: Option<String>,
    pub last_error: Option<String>,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    /// Unix ms when the worker was last seen idle without finishing.
    #[serde(default)]
    pub idle_since_ms: Option<u64>,
    #[serde(default)]
    pub nudged: bool,
    #[serde(default)]
    pub idle_reported: bool,
    /// Files last reported as conflicting with the integration branch.
    #[serde(default)]
    pub conflict_notice: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum MissionState {
    Preparing,
    Active,
    Stopped,
    Completed,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Mission {
    pub id: String,
    pub title: String,
    pub orchestrator: String,
    pub repo: PathBuf,
    pub base: String,
    pub integration_branch: String,
    pub integration_dir: PathBuf,
    pub verify: Option<String>,
    pub setup: Option<String>,
    pub state: MissionState,
    /// New tasks become ready as soon as they are added.
    pub auto: bool,
    pub created_ms: u64,
    pub tasks: Vec<Task>,
    pub log: Vec<String>,
    #[serde(default)]
    pub completion_reported: bool,
}

impl Mission {
    fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id.eq_ignore_ascii_case(id))
    }

    fn task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id.eq_ignore_ascii_case(id))
    }

    fn note(&mut self, line: impl Into<String>) {
        let stamp = chrono_like(super::agents::now_ms());
        self.log.push(format!("{stamp} {}", line.into()));
        if self.log.len() > 400 {
            self.log.drain(..100);
        }
    }

    fn counts(&self) -> (usize, usize) {
        let done = self.tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
        (done, self.tasks.len())
    }
}

/// HH:MM:SS (UTC) for log lines; no date/time dependency needed.
fn chrono_like(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{:02}:{:02}:{:02}Z", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= max {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        String::from("mission")
    } else {
        trimmed
    }
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// A task as the orchestrator describes it.
pub struct NewTask {
    pub title: String,
    pub prompt: String,
    pub files: Vec<String>,
    pub deps: Vec<String>,
    pub agent: String,
    pub model: Option<String>,
}

pub struct MissionBook {
    pub missions: Vec<Mission>,
    dir: Option<PathBuf>,
    /// Usage-limit retry schedule per terminal id (unix ms).
    limit_retry: HashMap<String, u64>,
}

pub struct Missions {
    pub book: Mutex<MissionBook>,
    merge_tx: Mutex<Option<mpsc::Sender<(String, String)>>>,
}

impl Missions {
    pub fn load(app_data: Option<&Path>) -> Self {
        let dir = app_data.map(|d| d.join("missions"));
        let mut missions = Vec::new();
        if let Some(dir) = &dir {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        if let Ok(mission) = serde_json::from_str::<Mission>(&text) {
                            missions.push(mission);
                        }
                    }
                }
            }
        }
        // Work interrupted by a restart resumes from a safe point.
        for mission in &mut missions {
            for task in &mut mission.tasks {
                match task.status {
                    TaskStatus::Starting => task.status = TaskStatus::Ready,
                    TaskStatus::Running | TaskStatus::Merging => {
                        task.status = TaskStatus::Ready;
                        task.assignee = None;
                        task.note_restart();
                    }
                    _ => {}
                }
            }
        }
        missions.sort_by_key(|m| m.created_ms);
        Missions {
            book: Mutex::new(MissionBook {
                missions,
                dir,
                limit_retry: HashMap::new(),
            }),
            merge_tx: Mutex::new(None),
        }
    }
}

impl Task {
    fn note_restart(&mut self) {
        self.last_error = Some(String::from(
            "Podium restarted while this task was in progress; it will be handed out again (its worktree and commits are kept).",
        ));
    }
}

impl MissionBook {
    fn save(&self, mission: &Mission) {
        let Some(dir) = &self.dir else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let path = dir.join(format!("{}.json", mission.id));
        let tmp = dir.join(format!("{}.json.tmp", mission.id));
        if let Ok(body) = serde_json::to_string_pretty(mission) {
            if std::fs::write(&tmp, body).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    pub fn active_for(&self, orchestrator: &str) -> Option<usize> {
        self.missions.iter().rposition(|m| {
            m.orchestrator == orchestrator
                && matches!(m.state, MissionState::Preparing | MissionState::Active)
        })
    }

    /// Mission and task a worker terminal is assigned to.
    pub fn worker_task(&self, terminal: &str) -> Option<(usize, usize)> {
        for (mi, mission) in self.missions.iter().enumerate() {
            if !matches!(mission.state, MissionState::Active) {
                continue;
            }
            for (ti, task) in mission.tasks.iter().enumerate() {
                if task.assignee.as_deref() == Some(terminal) && task.status.active() {
                    return Some((mi, ti));
                }
            }
        }
        None
    }
}

fn worktrees_root() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    // Short on purpose: Windows tools still trip over long node_modules paths.
    home.join(".podium").join("wt")
}

impl Inner {
    fn emit_mission(&self, mission: &Mission) {
        let _ = self.app.emit("mission-update", mission);
    }

    /// Persists and broadcasts mission `index` after a change.
    fn commit_mission(&self, book: &MissionBook, index: usize) {
        if let Some(mission) = book.missions.get(index) {
            book.save(mission);
            self.emit_mission(mission);
        }
    }

    fn podium_message(&self, to: &str, text: String) {
        self.enqueue(
            to,
            Envelope {
                from: None,
                text,
                raw: false,
                ask: None,
            },
        );
    }

    fn refresh_keep_awake(&self, book: &MissionBook) {
        let busy = book.missions.iter().any(|m| {
            m.state == MissionState::Active
                && m.tasks.iter().any(|t| t.status.active() || t.status == TaskStatus::Ready)
        }) || book.missions.iter().any(|m| m.state == MissionState::Preparing);
        crate::pty::set_keep_awake(busy);
    }

    fn merge_sender(self: &Arc<Self>) -> mpsc::Sender<(String, String)> {
        let mut slot = match self.missions.merge_tx.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(tx) = slot.as_ref() {
            return tx.clone();
        }
        let (tx, rx) = mpsc::channel::<(String, String)>();
        let inner = Arc::clone(self);
        std::thread::spawn(move || {
            for (mission, task) in rx {
                inner.merge_task(&mission, &task);
            }
        });
        *slot = Some(tx.clone());
        tx
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    pub(crate) fn mission_start(
        self: &Arc<Self>,
        orchestrator: &str,
        cwd: &str,
        title: &str,
        verify: Option<String>,
        setup: Option<String>,
        base: Option<String>,
    ) -> Result<String, String> {
        let repo = worktree::repo_root(Path::new(cwd))
            .map_err(|e| format!("not inside a git repository ({e})"))?;
        let base = match base {
            Some(b) => b,
            None => worktree::current_branch(&repo)?
                .unwrap_or(worktree::head_commit(&repo)?),
        };
        let now = super::agents::now_ms();
        let id = format!("{}-{}", slug(title, 24), base36(now / 1000));
        let repo_name = repo
            .file_name()
            .map(|n| slug(&n.to_string_lossy(), 16))
            .unwrap_or_else(|| String::from("repo"));
        let root = worktrees_root().join(format!("{repo_name}-{id}"));
        let mission = Mission {
            id: id.clone(),
            title: title.to_owned(),
            orchestrator: orchestrator.to_owned(),
            repo: repo.clone(),
            base: base.clone(),
            integration_branch: format!("podium/{id}"),
            integration_dir: root.join("integration"),
            verify,
            setup,
            state: MissionState::Preparing,
            auto: false,
            created_ms: now,
            tasks: Vec::new(),
            log: Vec::new(),
            completion_reported: false,
        };
        {
            let mut book = self.missions.book.lock().map_err(|e| e.to_string())?;
            if let Some(i) = book.active_for(orchestrator) {
                return Err(format!(
                    "mission \"{}\" is still active. Finish it first: podium mission finish",
                    book.missions[i].title
                ));
            }
            book.missions.push(mission.clone());
            let index = book.missions.len() - 1;
            book.missions[index].note(format!("mission created from {base}"));
            self.commit_mission(&book, index);
            self.refresh_keep_awake(&book);
        }
        let inner = Arc::clone(self);
        std::thread::spawn(move || inner.prepare_mission(&id));
        Ok(format!(
            "Mission \"{title}\" created.\nIntegration branch: {} (from {base})\nIntegration worktree: {}\nPreparing it now{}. Add tasks with `podium task add`; they start once preparation is done and you run `podium task run --all`.",
            mission.integration_branch,
            mission.integration_dir.display(),
            if mission.setup.is_some() { " (running setup)" } else { "" }
        ))
    }

    fn prepare_mission(self: &Arc<Self>, id: &str) {
        let Some(mission) = self.mission_snapshot(id) else {
            return;
        };
        let result = worktree::add_worktree(
            &mission.repo,
            &mission.integration_dir,
            &mission.integration_branch,
            &mission.base,
        )
        .and_then(|()| match &mission.setup {
            Some(setup) => {
                let mut cmd = crate::pty::shell_exec(setup);
                cmd.current_dir(&mission.integration_dir);
                let run = worktree::run_with_timeout(cmd, SETUP_TIMEOUT, OUTPUT_TAIL_LINES);
                if run.success {
                    Ok(())
                } else {
                    Err(format!("setup failed in the integration worktree:\n{}", run.output_tail))
                }
            }
            None => Ok(()),
        });
        let Ok(mut book) = self.missions.book.lock() else {
            return;
        };
        let Some(index) = book.missions.iter().position(|m| m.id == id) else {
            return;
        };
        let orchestrator = book.missions[index].orchestrator.clone();
        let message = match result {
            Ok(()) => {
                book.missions[index].state = MissionState::Active;
                book.missions[index].note("integration worktree ready");
                None
            }
            Err(e) => {
                book.missions[index].state = MissionState::Stopped;
                book.missions[index].note(format!("preparation failed: {e}"));
                Some(format!(
                    "[Podium] Mission \"{}\" could not be prepared:\n{e}\nFix the cause and start a new mission.",
                    book.missions[index].title
                ))
            }
        };
        self.commit_mission(&book, index);
        self.refresh_keep_awake(&book);
        drop(book);
        if let Some(text) = message {
            self.podium_message(&orchestrator, text);
        }
    }

    fn mission_snapshot(&self, id: &str) -> Option<Mission> {
        self.missions
            .book
            .lock()
            .ok()?
            .missions
            .iter()
            .find(|m| m.id == id)
            .cloned()
    }

    pub(crate) fn mission_status(&self, orchestrator: &str) -> Result<String, String> {
        let book = self.missions.book.lock().map_err(|e| e.to_string())?;
        let index = book
            .missions
            .iter()
            .rposition(|m| m.orchestrator == orchestrator)
            .ok_or_else(|| String::from("no mission yet. Start one with: podium mission start \"Title\""))?;
        Ok(describe_mission(&book.missions[index]))
    }

    pub(crate) fn task_add(&self, orchestrator: &str, new: NewTask) -> Result<String, String> {
        let NewTask {
            title,
            prompt,
            files,
            deps,
            agent,
            model,
        } = new;
        let title = title.as_str();
        let mut book = self.missions.book.lock().map_err(|e| e.to_string())?;
        let index = book
            .active_for(orchestrator)
            .ok_or_else(|| String::from("no active mission. Start one with: podium mission start \"Title\""))?;
        let mission = &mut book.missions[index];
        for dep in &deps {
            if mission.task(dep).is_none() {
                return Err(format!("unknown dependency '{dep}'"));
            }
        }
        let id = format!("t{}", mission.tasks.len() + 1);
        let root = mission
            .integration_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(worktrees_root);
        let status = if mission.auto {
            TaskStatus::Ready
        } else {
            TaskStatus::Pending
        };
        mission.tasks.push(Task {
            id: id.clone(),
            title: title.to_owned(),
            prompt,
            files: files.clone(),
            deps: deps.iter().map(|d| d.to_lowercase()).collect(),
            agent,
            model,
            status,
            assignee: None,
            worker_name: None,
            // A sibling, not a child: git refuses `podium/x/t1` next to `podium/x`.
            branch: format!("{}-{id}", mission.integration_branch),
            worktree: root.join(&id),
            attempts: 0,
            summary: None,
            last_error: None,
            started_ms: None,
            finished_ms: None,
            idle_since_ms: None,
            nudged: false,
            idle_reported: false,
            conflict_notice: Vec::new(),
        });
        mission.note(format!("{id} added: {title}"));
        mission.completion_reported = false;
        self.commit_mission(&book, index);
        Ok(format!(
            "Added {id}: {title}{}{}",
            if files.is_empty() {
                String::from(" (no file scope: it will not run in parallel with other tasks)")
            } else {
                format!(" [files: {}]", files.join(", "))
            },
            if deps.is_empty() {
                String::new()
            } else {
                format!(" [after: {}]", deps.join(", "))
            }
        ))
    }

    pub(crate) fn task_run(&self, orchestrator: &str, which: Option<&str>) -> Result<String, String> {
        let mut book = self.missions.book.lock().map_err(|e| e.to_string())?;
        let index = book
            .active_for(orchestrator)
            .ok_or_else(|| String::from("no active mission"))?;
        let mission = &mut book.missions[index];
        let mut started = Vec::new();
        match which {
            None => {
                mission.auto = true;
                for task in &mut mission.tasks {
                    if task.status == TaskStatus::Pending {
                        task.status = TaskStatus::Ready;
                        started.push(task.id.clone());
                    }
                }
            }
            Some(id) => {
                let task = mission
                    .task_mut(id)
                    .ok_or_else(|| format!("unknown task '{id}'"))?;
                if task.status != TaskStatus::Pending {
                    return Err(format!("{} is {}", task.id, task.status.label()));
                }
                task.status = TaskStatus::Ready;
                started.push(task.id.clone());
            }
        }
        mission.note(format!("ready: {}", started.join(", ")));
        self.commit_mission(&book, index);
        self.refresh_keep_awake(&book);
        Ok(format!(
            "Queued {} task(s). Podium starts each one when its dependencies are merged, no running task touches the same files, and a recruit slot and enough memory are free. You will get a message for every merge and problem; `podium mission status` shows the board.",
            started.len()
        ))
    }

    pub(crate) fn task_retry(&self, orchestrator: &str, id: &str) -> Result<String, String> {
        let mut book = self.missions.book.lock().map_err(|e| e.to_string())?;
        let index = book
            .active_for(orchestrator)
            .ok_or_else(|| String::from("no active mission"))?;
        let mission = &mut book.missions[index];
        let task = mission.task_mut(id).ok_or_else(|| format!("unknown task '{id}'"))?;
        if !matches!(task.status, TaskStatus::Failed | TaskStatus::NeedsHuman | TaskStatus::Cancelled) {
            return Err(format!("{} is {}; only failed tasks can be retried", task.id, task.status.label()));
        }
        task.status = TaskStatus::Ready;
        task.attempts = 0;
        task.assignee = None;
        let tid = task.id.clone();
        mission.note(format!("{tid} retried"));
        mission.completion_reported = false;
        self.commit_mission(&book, index);
        self.refresh_keep_awake(&book);
        Ok(format!("{tid} is ready again."))
    }

    pub(crate) fn task_cancel(&self, orchestrator: &str, id: &str) -> Result<String, String> {
        let (worker, tid) = {
            let mut book = self.missions.book.lock().map_err(|e| e.to_string())?;
            let index = book
                .active_for(orchestrator)
                .ok_or_else(|| String::from("no active mission"))?;
            let mission = &mut book.missions[index];
            let task = mission.task_mut(id).ok_or_else(|| format!("unknown task '{id}'"))?;
            if task.status.settled() {
                return Err(format!("{} is already {}", task.id, task.status.label()));
            }
            task.status = TaskStatus::Cancelled;
            task.finished_ms = Some(super::agents::now_ms());
            let worker = task.assignee.take();
            let tid = task.id.clone();
            mission.note(format!("{tid} cancelled"));
            self.commit_mission(&book, index);
            (worker, tid)
        };
        if let Some(worker) = worker {
            let _ = self.ui_request("dismiss", json!({ "id": worker }));
        }
        Ok(format!("{tid} cancelled (its worktree is kept for reference)."))
    }

    pub(crate) fn mission_stop(&self, orchestrator: &str, finish: bool) -> Result<String, String> {
        let (mission, workers) = {
            let mut book = self.missions.book.lock().map_err(|e| e.to_string())?;
            let index = book
                .active_for(orchestrator)
                .ok_or_else(|| String::from("no active mission"))?;
            let mission = &mut book.missions[index];
            if finish && mission.tasks.iter().any(|t| t.status.active()) {
                return Err(String::from(
                    "tasks are still running or merging; wait for them, cancel them, or use `podium mission stop`",
                ));
            }
            mission.state = if finish {
                MissionState::Completed
            } else {
                MissionState::Stopped
            };
            let workers: Vec<String> = mission
                .tasks
                .iter_mut()
                .filter(|t| t.status.active() || t.status == TaskStatus::Ready)
                .filter_map(|t| {
                    t.status = TaskStatus::Cancelled;
                    t.assignee.take()
                })
                .collect();
            mission.note(if finish { "mission finished" } else { "mission stopped" });
            let snapshot = mission.clone();
            self.commit_mission(&book, index);
            self.refresh_keep_awake(&book);
            (snapshot, workers)
        };
        for worker in workers {
            let _ = self.ui_request("dismiss", json!({ "id": worker }));
        }
        if finish {
            let _ = worktree::remove_worktree(&mission.repo, &mission.integration_dir, None);
            // The mission folder only held worktrees; drop it once empty.
            if let Some(root) = mission.integration_dir.parent() {
                let _ = std::fs::remove_dir(root);
            }
        }
        let (done, total) = mission.counts();
        Ok(format!(
            "Mission \"{}\" {}. {done}/{total} tasks merged into {}.\nReview: git log {}..{}   |   git diff {}...{}\nNothing was pushed.",
            mission.title,
            if finish { "finished" } else { "stopped" },
            mission.integration_branch,
            mission.base,
            mission.integration_branch,
            mission.base,
            mission.integration_branch
        ))
    }

    /// `podium done`: the worker finished its task.
    pub(crate) fn task_done(self: &Arc<Self>, worker: &str, summary: &str) -> Result<String, String> {
        let (mission_id, task_id, worktree_dir, repo, integration, branch, title) = {
            let book = self.missions.book.lock().map_err(|e| e.to_string())?;
            let (mi, ti) = book
                .worker_task(worker)
                .ok_or_else(|| String::from("you have no open mission task"))?;
            let mission = &book.missions[mi];
            let task = &mission.tasks[ti];
            if task.status == TaskStatus::Merging {
                return Err(String::from("already submitted; wait for the merge result"));
            }
            (
                mission.id.clone(),
                task.id.clone(),
                task.worktree.clone(),
                mission.repo.clone(),
                mission.integration_branch.clone(),
                task.branch.clone(),
                task.title.clone(),
            )
        };
        worktree::commit_all(&worktree_dir, &format!("{task_id}: {title}"))?;
        let ahead = worktree::commits_ahead(&repo, &integration, &branch)?;
        let mut book = self.missions.book.lock().map_err(|e| e.to_string())?;
        let Some(mi) = book.missions.iter().position(|m| m.id == mission_id) else {
            return Err(String::from("mission vanished"));
        };
        let mission = &mut book.missions[mi];
        let Some(task) = mission.task_mut(&task_id) else {
            return Err(String::from("task vanished"));
        };
        task.summary = Some(summary.to_owned());
        task.idle_since_ms = None;
        if ahead == 0 {
            task.status = TaskStatus::Done;
            task.finished_ms = Some(super::agents::now_ms());
            mission.note(format!("{task_id} done without changes"));
            self.commit_mission(&book, mi);
            drop(book);
            self.finish_task_cleanup(&mission_id, &task_id, "done (no changes to merge)");
            return Ok(String::from("No changes to merge; task closed. Thank you, you can stop now."));
        }
        task.status = TaskStatus::Merging;
        mission.note(format!("{task_id} submitted ({ahead} commit(s))"));
        self.commit_mission(&book, mi);
        drop(book);
        let _ = self.merge_sender().send((mission_id, task_id));
        Ok(String::from(
            "Submitted. Podium is merging your branch and running the checks; stop here and wait — if anything needs fixing you will get a message.",
        ))
    }

    /// `podium fail`: the worker gives up on its task.
    pub(crate) fn task_fail(&self, worker: &str, reason: &str) -> Result<String, String> {
        let (orchestrator, text) = {
            let mut book = self.missions.book.lock().map_err(|e| e.to_string())?;
            let (mi, ti) = book
                .worker_task(worker)
                .ok_or_else(|| String::from("you have no open mission task"))?;
            let mission = &mut book.missions[mi];
            let task = &mut mission.tasks[ti];
            task.status = TaskStatus::Failed;
            task.last_error = Some(reason.to_owned());
            task.finished_ms = Some(super::agents::now_ms());
            let text = format!(
                "[Podium] Mission \"{}\": {} ({}) failed: {reason}\nRetry it with `podium task retry {}` after changing the plan, or leave it for the user.",
                mission.title, task.id, task.title, task.id
            );
            let tid = task.id.clone();
            mission.note(format!("{tid} failed: {reason}"));
            let orchestrator = mission.orchestrator.clone();
            self.commit_mission(&book, mi);
            (orchestrator, text)
        };
        self.podium_message(&orchestrator, text);
        Ok(String::from("Recorded. The orchestrator has been told; you can stop now."))
    }

    // -----------------------------------------------------------------------
    // Merge queue
    // -----------------------------------------------------------------------

    fn merge_task(self: &Arc<Self>, mission_id: &str, task_id: &str) {
        let Some(mission) = self.mission_snapshot(mission_id) else {
            return;
        };
        let Some(task) = mission.task(task_id).cloned() else {
            return;
        };
        if task.status != TaskStatus::Merging {
            return;
        }
        let message = format!("Merge {}: {}", task.id, task.title);
        let outcome = worktree::merge_branch(&mission.integration_dir, &task.branch, &message);
        let failure = match outcome {
            Err(e) => Some(format!("merge failed: {e}")),
            Ok(MergeOutcome::Conflict { files }) => Some(format!(
                "CONFLICT with work already merged, in: {}.\nIn your worktree run `git merge {}`, resolve the conflicts so both changes survive (read the other side's commits with `git log -p {}..{} -- <file>`), commit, then run `podium done \"...\"` again.",
                files.join(", "),
                mission.integration_branch,
                task.branch,
                mission.integration_branch
            )),
            Ok(MergeOutcome::UpToDate) => None,
            Ok(MergeOutcome::Merged { commit }) => match &mission.verify {
                None => None,
                Some(verify) => {
                    let mut cmd = crate::pty::shell_exec(verify);
                    cmd.current_dir(&mission.integration_dir);
                    let run = worktree::run_with_timeout(cmd, VERIFY_TIMEOUT, OUTPUT_TAIL_LINES);
                    if run.success {
                        None
                    } else {
                        let _ = worktree::undo_merge(&mission.integration_dir, &commit);
                        Some(format!(
                            "your change merged cleanly but `{verify}` failed on the integration branch{}:\n{}\nFix it in your worktree (run `git merge {}` first to get the latest integrated work), commit, then run `podium done \"...\"` again.",
                            if run.timed_out { " (timed out)" } else { "" },
                            run.output_tail,
                            mission.integration_branch
                        ))
                    }
                }
            },
        };
        match failure {
            None => {
                {
                    let Ok(mut book) = self.missions.book.lock() else {
                        return;
                    };
                    let Some(mi) = book.missions.iter().position(|m| m.id == mission_id) else {
                        return;
                    };
                    if let Some(t) = book.missions[mi].task_mut(task_id) {
                        t.status = TaskStatus::Done;
                        t.finished_ms = Some(super::agents::now_ms());
                    }
                    book.missions[mi].note(format!("{task_id} merged"));
                    self.commit_mission(&book, mi);
                }
                self.finish_task_cleanup(mission_id, task_id, "merged");
            }
            Some(problem) => self.merge_failed(mission_id, task_id, &problem),
        }
    }

    fn merge_failed(&self, mission_id: &str, task_id: &str, problem: &str) {
        let Ok(mut book) = self.missions.book.lock() else {
            return;
        };
        let Some(mi) = book.missions.iter().position(|m| m.id == mission_id) else {
            return;
        };
        let mission = &mut book.missions[mi];
        let orchestrator = mission.orchestrator.clone();
        let title = mission.title.clone();
        let Some(task) = mission.task_mut(task_id) else {
            return;
        };
        task.attempts += 1;
        task.last_error = Some(problem.to_owned());
        let attempts = task.attempts;
        let worker = task.assignee.clone();
        let task_title = task.title.clone();
        let mut to_worker = None;
        let mut to_orchestrator = None;
        let mut dismiss = None;
        if attempts >= MAX_ATTEMPTS {
            task.status = TaskStatus::NeedsHuman;
            task.finished_ms = Some(super::agents::now_ms());
            dismiss = task.assignee.take();
            to_orchestrator = Some(format!(
                "[Podium] Mission \"{title}\": {task_id} ({task_title}) still fails after {attempts} attempts and is left for the user. Its branch and worktree are kept. Last problem:\n{problem}"
            ));
        } else {
            task.status = TaskStatus::Running;
            to_worker = Some(format!("[Podium] Task {task_id} was not integrated: {problem}"));
            if attempts > WORKER_ATTEMPTS {
                to_orchestrator = Some(format!(
                    "[Podium] Mission \"{title}\": {task_id} ({task_title}) failed to integrate {attempts} times; the worker is on it again. You may want to step in (podium check / podium ask). Problem:\n{problem}"
                ));
            }
        }
        book.missions[mi].note(format!("{task_id} not integrated (attempt {attempts})"));
        self.commit_mission(&book, mi);
        self.refresh_keep_awake(&book);
        drop(book);
        if let (Some(worker), Some(text)) = (worker, to_worker) {
            self.podium_message(&worker, text);
        }
        if let Some(text) = to_orchestrator {
            self.podium_message(&orchestrator, text);
        }
        if let Some(worker) = dismiss {
            let _ = self.ui_request("dismiss", json!({ "id": worker }));
        }
    }

    /// Removes a finished task's worktree and worker, and reports progress.
    fn finish_task_cleanup(&self, mission_id: &str, task_id: &str, verb: &str) {
        let Some(mission) = self.mission_snapshot(mission_id) else {
            return;
        };
        let Some(task) = mission.task(task_id).cloned() else {
            return;
        };
        // The worker's shell sits inside the worktree, and Windows will not
        // delete a directory a process is using: close the worker first.
        if let Some(worker) = &task.assignee {
            let _ = self.ui_request("dismiss", json!({ "id": worker }));
        }
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(800));
            }
            if worktree::remove_worktree(&mission.repo, &task.worktree, Some(&task.branch)).is_ok() {
                break;
            }
        }
        let (done, total) = mission.counts();
        self.podium_message(
            &mission.orchestrator,
            format!(
                "[Podium] Mission \"{}\": {} ({}) {verb}. Summary: {}\nProgress: {done}/{total} merged.",
                mission.title,
                task.id,
                task.title,
                task.summary.as_deref().unwrap_or("-")
            ),
        );
        if let Ok(book) = self.missions.book.lock() {
            self.refresh_keep_awake(&book);
        }
    }

    // -----------------------------------------------------------------------
    // Scheduler and watchdog (called from the bridge tick)
    // -----------------------------------------------------------------------

    pub(crate) fn mission_tick(self: &Arc<Self>) {
        let settings = self.settings();
        let now = super::agents::now_ms();
        let graph = self.graph();
        let mut to_start: Vec<(String, String)> = Vec::new();
        let mut completions: Vec<(String, String)> = Vec::new();
        let mut workers: Vec<(String, String, String, String)> = Vec::new();
        {
            let Ok(mut book) = self.missions.book.lock() else {
                return;
            };
            let free_ok = crate::pty::free_memory_mb()
                .is_none_or(|free| free >= settings.min_free_memory_mb);
            let mut changed = Vec::new();
            for (mi, mission) in book.missions.iter_mut().enumerate() {
                // After a restart, a mission resumes only once its
                // orchestrator is back on the canvas.
                if mission.state != MissionState::Active || graph.node(&mission.orchestrator).is_none() {
                    continue;
                }
                let other_recruits = graph
                    .recruits(&mission.orchestrator)
                    .iter()
                    .filter(|r| !mission.tasks.iter().any(|t| t.assignee.as_deref() == Some(r.id.as_str())))
                    .count();
                let mut active: Vec<Vec<String>> = mission
                    .tasks
                    .iter()
                    .filter(|t| t.status.active())
                    .map(|t| t.files.clone())
                    .collect();
                let mut slots = settings
                    .max_recruits
                    .saturating_sub(other_recruits + active.len());
                let done: Vec<String> = mission
                    .tasks
                    .iter()
                    .filter(|t| t.status == TaskStatus::Done)
                    .map(|t| t.id.clone())
                    .collect();
                for task in &mut mission.tasks {
                    if slots == 0 || !free_ok {
                        break;
                    }
                    if task.status != TaskStatus::Ready
                        || !task.deps.iter().all(|d| done.iter().any(|x| x == d))
                        || active.iter().any(|scope| worktree::scopes_overlap(scope, &task.files))
                    {
                        continue;
                    }
                    task.status = TaskStatus::Starting;
                    task.started_ms = Some(now);
                    active.push(task.files.clone());
                    slots -= 1;
                    to_start.push((mission.id.clone(), task.id.clone()));
                    changed.push(mi);
                }
                for task in &mission.tasks {
                    if task.status == TaskStatus::Running {
                        if let Some(worker) = &task.assignee {
                            workers.push((mission.id.clone(), task.id.clone(), worker.clone(), mission.orchestrator.clone()));
                        }
                    }
                }
                // Tasks added but not yet run are not settled: the plan is
                // still being written.
                let all_settled = !mission.tasks.is_empty()
                    && mission.tasks.iter().all(|t| t.status.settled());
                if all_settled && !mission.completion_reported {
                    mission.completion_reported = true;
                    completions.push((mission.orchestrator.clone(), describe_completion(mission)));
                    changed.push(mi);
                }
            }
            changed.dedup();
            for mi in changed {
                self.commit_mission(&book, mi);
            }
            self.refresh_keep_awake(&book);
        }
        for (mission, task) in to_start {
            let inner = Arc::clone(self);
            std::thread::spawn(move || inner.start_task(&mission, &task));
        }
        for (orchestrator, text) in completions {
            self.podium_message(&orchestrator, text);
        }
        for (mission, task, worker, orchestrator) in workers {
            self.watch_worker(&mission, &task, &worker, &orchestrator, now);
        }
        self.watch_limits(now);
    }

    fn start_task(self: &Arc<Self>, mission_id: &str, task_id: &str) {
        let Some(mission) = self.mission_snapshot(mission_id) else {
            return;
        };
        let Some(task) = mission.task(task_id).cloned() else {
            return;
        };
        let settings = self.settings();
        // Several missions may run at once: worker names stay unique on the canvas.
        let base_name = format!("{}-{}", task.id, slug(&task.title, 18));
        let taken: Vec<String> = self
            .graph()
            .terminal_names()
            .iter()
            .map(|n| n.to_lowercase())
            .collect();
        let name = std::iter::once(base_name.clone())
            .chain((2..).map(|i| format!("{base_name}-{i}")))
            .find(|candidate| !taken.contains(&candidate.to_lowercase()))
            .unwrap_or(base_name);
        let result = (|| -> Result<String, String> {
            if !task.worktree.is_dir() {
                worktree::add_worktree(&mission.repo, &task.worktree, &task.branch, &mission.integration_branch)?;
            }
            if let Some(setup) = &mission.setup {
                let mut cmd = crate::pty::shell_exec(setup);
                cmd.current_dir(&task.worktree);
                let run = worktree::run_with_timeout(cmd, SETUP_TIMEOUT, OUTPUT_TAIL_LINES);
                if !run.success {
                    return Err(format!("setup failed:\n{}", run.output_tail));
                }
            }
            let command = super::launch::agent_command(
                &task.agent,
                &settings,
                self.claude_settings.as_deref(),
                true,
                task.model.as_deref(),
            );
            let created = self.ui_request(
                "spawn",
                json!({
                    "parent": mission.orchestrator,
                    "name": name,
                    "command": command,
                    "agent": task.agent,
                    "cwd": task.worktree.to_string_lossy(),
                }),
            )?;
            created
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| String::from("the canvas did not create the worker"))
        })();
        let Ok(mut book) = self.missions.book.lock() else {
            return;
        };
        let Some(mi) = book.missions.iter().position(|m| m.id == mission_id) else {
            return;
        };
        let orchestrator_name = self.name_of(&mission.orchestrator);
        let mut failure = None;
        let mut first_prompt = None;
        if let Some(t) = book.missions[mi].task_mut(task_id) {
            match &result {
                Ok(id) => {
                    t.status = TaskStatus::Running;
                    t.assignee = Some(id.clone());
                    t.worker_name = Some(name.clone());
                    t.idle_since_ms = None;
                    t.nudged = false;
                    t.idle_reported = false;
                    first_prompt = Some(worker_prompt(&mission, t, &name, &orchestrator_name));
                }
                Err(e) => {
                    t.status = TaskStatus::Failed;
                    t.last_error = Some(e.clone());
                    t.finished_ms = Some(super::agents::now_ms());
                    failure = Some(format!(
                        "[Podium] Mission \"{}\": {} ({}) could not start: {e}",
                        mission.title, t.id, t.title
                    ));
                }
            }
        }
        book.missions[mi].note(match &result {
            Ok(_) => format!("{task_id} started by {name}"),
            Err(e) => format!("{task_id} could not start: {e}"),
        });
        self.commit_mission(&book, mi);
        drop(book);
        if let (Ok(id), Some(prompt)) = (&result, first_prompt) {
            if let Ok(mut state) = self.state.lock() {
                if let Some(entry) = state.agents.entries.get_mut(id) {
                    entry.kind = Some(task.agent.clone());
                }
            }
            self.enqueue(
                id,
                Envelope {
                    from: Some(mission.orchestrator.clone()),
                    text: prompt,
                    raw: false,
                    ask: None,
                },
            );
        }
        if let Some(text) = failure {
            self.podium_message(&mission.orchestrator, text);
        }
    }

    /// Nudges a worker that stopped without `podium done`, then reports it;
    /// runs a non-blocking conflict pre-check whenever it pauses.
    fn watch_worker(self: &Arc<Self>, mission_id: &str, task_id: &str, worker: &str, orchestrator: &str, now: u64) {
        let idle = self.agent_state(worker) == Some(AgentState::Idle);
        let mut nudge = None;
        let mut report = None;
        {
            let Ok(mut book) = self.missions.book.lock() else {
                return;
            };
            let Some(mi) = book.missions.iter().position(|m| m.id == mission_id) else {
                return;
            };
            let title = book.missions[mi].title.clone();
            let Some(task) = book.missions[mi].task_mut(task_id) else {
                return;
            };
            if !idle {
                task.idle_since_ms = None;
                return;
            }
            if task.idle_since_ms.is_none() {
                // The worker just paused: look for conflicts with what was
                // merged meanwhile, without touching any checkout.
                let mission = &book.missions[mi];
                let check = (
                    mission.repo.clone(),
                    mission.integration_branch.clone(),
                    task_id.to_owned(),
                    mission_id.to_owned(),
                );
                let inner = Arc::clone(self);
                std::thread::spawn(move || inner.precheck_conflicts(&check.0, &check.1, &check.3, &check.2));
                let Some(task) = book.missions[mi].task_mut(task_id) else {
                    return;
                };
                task.idle_since_ms = Some(now);
                return;
            }
            let since = task.idle_since_ms.unwrap_or(now);
            let idle_for = now.saturating_sub(since);
            if !task.nudged && idle_for >= IDLE_NUDGE_MS {
                task.nudged = true;
                task.idle_since_ms = Some(now);
                nudge = Some(format!(
                    "[Podium] Task {} is still open. If it is complete, commit and run: podium done \"<summary>\". If you are blocked, run: podium fail \"<why>\". Otherwise keep going.",
                    task.id
                ));
            } else if task.nudged && !task.idle_reported && idle_for >= IDLE_REPORT_MS {
                task.idle_reported = true;
                report = Some(format!(
                    "[Podium] Mission \"{title}\": the worker of {} ({}) has been idle for {} minutes without finishing. Check it with: podium check \"{}\"",
                    task.id,
                    task.title,
                    idle_for / 60_000,
                    task.worker_name.as_deref().unwrap_or("?")
                ));
            }
            if nudge.is_some() || report.is_some() {
                book.save(&book.missions[mi]);
            }
        }
        if let Some(text) = nudge {
            self.podium_message(worker, text);
        }
        if let Some(text) = report {
            self.podium_message(orchestrator, text);
        }
    }

    fn precheck_conflicts(&self, repo: &Path, integration: &str, mission_id: &str, task_id: &str) {
        let Some(mission) = self.mission_snapshot(mission_id) else {
            return;
        };
        let Some(task) = mission.task(task_id) else {
            return;
        };
        if worktree::commits_ahead(repo, integration, &task.branch).unwrap_or(0) == 0 {
            return;
        }
        let Ok(files) = worktree::conflicting_files(repo, integration, &task.branch) else {
            return;
        };
        if files.is_empty() || files == task.conflict_notice {
            return;
        }
        let worker = task.assignee.clone();
        if let Ok(mut book) = self.missions.book.lock() {
            if let Some(mi) = book.missions.iter().position(|m| m.id == mission_id) {
                if let Some(t) = book.missions[mi].task_mut(task_id) {
                    t.conflict_notice = files.clone();
                }
                book.save(&book.missions[mi]);
            }
        }
        if let Some(worker) = worker {
            self.podium_message(
                &worker,
                format!(
                    "[Podium] Heads-up: your committed work already conflicts with what was merged into {integration} in: {}. Run `git merge {integration}` in your worktree now and resolve it while the context is fresh.",
                    files.join(", ")
                ),
            );
        }
    }

    /// Agents of active missions that stopped on a usage limit are prompted
    /// to continue every `LIMIT_RETRY_MS` until they work again.
    fn watch_limits(&self, now: u64) {
        let participants: Vec<String> = match self.missions.book.lock() {
            Ok(book) => book
                .missions
                .iter()
                .filter(|m| m.state == MissionState::Active)
                .flat_map(|m| {
                    std::iter::once(m.orchestrator.clone()).chain(
                        m.tasks
                            .iter()
                            .filter(|t| t.status == TaskStatus::Running)
                            .filter_map(|t| t.assignee.clone()),
                    )
                })
                .collect(),
            Err(_) => return,
        };
        for id in participants {
            if self.agent_state(&id) != Some(AgentState::Idle) {
                if let Ok(mut book) = self.missions.book.lock() {
                    book.limit_retry.remove(&id);
                }
                continue;
            }
            let limited = self
                .pty()
                .screen_text(&id)
                .is_some_and(|screen| shows_usage_limit(&screen));
            if !limited {
                continue;
            }
            let due = {
                let Ok(mut book) = self.missions.book.lock() else {
                    return;
                };
                let next = *book.limit_retry.entry(id.clone()).or_insert(now + LIMIT_RETRY_MS);
                if now >= next {
                    book.limit_retry.insert(id.clone(), now + LIMIT_RETRY_MS);
                    true
                } else {
                    false
                }
            };
            if due {
                self.podium_message(
                    &id,
                    String::from("[Podium] Continue where you left off (your usage limit may have reset). If you are orchestrating, start with `podium mission status`."),
                );
            }
        }
    }

    /// File lease check for a worker's edit (PreToolUse). Returns why the
    /// edit is refused, if it is.
    pub(crate) fn lease_violation(&self, worker: &str, file: &str) -> Option<String> {
        let book = self.missions.book.lock().ok()?;
        let (mi, ti) = book.worker_task(worker)?;
        let mission = &book.missions[mi];
        let task = &mission.tasks[ti];
        let path = Path::new(file);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            task.worktree.join(path)
        };
        let Some(relative) = worktree::relative_to(&task.worktree, &absolute) else {
            return Some(format!(
                "Blocked by Podium: {file} is outside your worktree ({}). Work only there.",
                task.worktree.display()
            ));
        };
        let own = task.files.iter().any(|p| worktree::glob_match(p, &relative));
        if own {
            return None;
        }
        let owner = mission.tasks.iter().find(|other| {
            other.id != task.id
                && other.status.active()
                && other.files.iter().any(|p| worktree::glob_match(p, &relative))
        })?;
        Some(format!(
            "Blocked by Podium: {relative} belongs to task {} ({}), which another worker is changing right now. If you really need it, ask your orchestrator: podium ask \"{}\" \"...\"",
            owner.id,
            owner.title,
            self.name_of(&mission.orchestrator)
        ))
    }
}

fn shows_usage_limit(screen: &str) -> bool {
    let tail: String = screen
        .lines()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    ["usage limit", "limit reached", "rate limit", "limit will reset", "resets at"]
        .iter()
        .any(|needle| tail.contains(needle))
}

fn worker_prompt(mission: &Mission, task: &Task, name: &str, orchestrator: &str) -> String {
    let mut out = format!(
        "You are \"{name}\", a worker on the mission \"{}\" run by \"{orchestrator}\" in Podium.\n\nTask {}: {}\n{}\n\nRules:\n- Work only in this directory: {} (git worktree on branch {}). Never touch other checkouts.\n",
        mission.title,
        task.id,
        task.title,
        task.prompt,
        task.worktree.display(),
        task.branch
    );
    if task.files.is_empty() {
        out.push_str("- Your file scope is whatever the task needs.\n");
    } else {
        out.push_str(&format!(
            "- Files you own: {}. Other workers are editing other files at the same time; edits to files owned by another running task are blocked. If you need one, ask: podium ask \"{orchestrator}\" \"...\".\n",
            task.files.join(", ")
        ));
    }
    if let Some(verify) = &mission.verify {
        out.push_str(&format!("- Before finishing, make sure `{verify}` passes here.\n"));
    }
    out.push_str(
        "- Commit your work locally. Never push and never open pull requests.\n- When the task is complete, run: podium done \"<short summary of what changed>\"\n- If you cannot complete it, run: podium fail \"<why>\"\nPodium merges your branch, runs the checks, and messages you if anything needs fixing.",
    );
    out
}

fn describe_mission(mission: &Mission) -> String {
    let (done, total) = mission.counts();
    let mut out = format!(
        "Mission \"{}\" — {:?}, {done}/{total} merged\nIntegration: {} at {}\n",
        mission.title,
        mission.state,
        mission.integration_branch,
        mission.integration_dir.display()
    );
    if let Some(verify) = &mission.verify {
        out.push_str(&format!("Verify: {verify}\n"));
    }
    out.push_str("Tasks:\n");
    if mission.tasks.is_empty() {
        out.push_str("  (none yet)\n");
    }
    for task in &mission.tasks {
        out.push_str(&format!(
            "  {} [{}] {}{}{}{}\n",
            task.id,
            task.status.label(),
            task.title,
            task.worker_name
                .as_ref()
                .filter(|_| task.status.active())
                .map(|w| format!(" — worker {w}"))
                .unwrap_or_default(),
            if task.deps.is_empty() {
                String::new()
            } else {
                format!(" (after {})", task.deps.join(", "))
            },
            if task.attempts > 0 {
                format!(" (attempts: {})", task.attempts)
            } else {
                String::new()
            }
        ));
        if let (TaskStatus::Failed | TaskStatus::NeedsHuman, Some(err)) = (task.status, &task.last_error) {
            let first = err.lines().next().unwrap_or_default();
            out.push_str(&format!("      last problem: {first}\n"));
        }
    }
    out.push_str("Recent:\n");
    for line in mission.log.iter().rev().take(8).collect::<Vec<_>>().into_iter().rev() {
        out.push_str(&format!("  {line}\n"));
    }
    out
}

fn describe_completion(mission: &Mission) -> String {
    let (done, total) = mission.counts();
    let attention: Vec<String> = mission
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Failed | TaskStatus::NeedsHuman))
        .map(|t| format!("{} ({})", t.id, t.title))
        .collect();
    format!(
        "[Podium] Mission \"{}\": every task is settled — {done}/{total} merged into {}{}.\nNow write the report for the user with `podium note create --name \"Mission report\" --file <report.md>` (what was done per task, checks run, what failed and why, how to review: git log {}..{}), then run `podium mission finish`. Do not push.",
        mission.title,
        mission.integration_branch,
        if attention.is_empty() {
            String::new()
        } else {
            format!("; needing attention: {}", attention.join(", "))
        },
        mission.base,
        mission.integration_branch
    )
}

/// Prompt that hands a mission brief (a note) to an orchestrator.
pub fn run_brief_prompt(note: &str) -> String {
    format!(
        "[Podium] Run the brief in note \"{note}\" as a Podium mission, autonomously: the user is away and will review in the morning, so do not wait for them.\n\
1. Read it: podium note read \"{note}\". Explore the repository enough to plan.\n\
2. podium mission start \"<short title>\" --verify \"<command that proves the project still builds and its tests pass>\" --setup \"<dependency install, if worktrees need one>\"\n\
3. Split the work into tasks with disjoint file scopes; give each a self-contained brief (goal, files, acceptance criteria): podium task add \"Title\" --files \"...\" [--deps t1] --file <brief.md>\n\
4. podium task run --all. Podium starts recruits in their own git worktrees, merges each finished branch into the integration branch, runs the verify command, and messages you on every merge and problem.\n\
5. React to those messages: unblock workers (podium check / podium ask), retry or re-plan failed tasks, add tasks that turn out to be needed.\n\
6. When Podium says every task is settled, write the report note and run podium mission finish. Never push and never open pull requests."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_limits() {
        assert_eq!(slug("Fix: Login page (v2)!", 24), "fix-login-page-v2");
        assert_eq!(slug("***", 10), "mission");
        assert!(shows_usage_limit("...\n5-hour limit reached ∙ resets 3am\n"));
        assert!(!shows_usage_limit("all good\n> "));
        assert_eq!(base36(36 * 36 + 1), "101");
    }
}
