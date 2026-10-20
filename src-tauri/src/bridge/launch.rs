//! How agents are launched so they report to the bridge: Claude Code gets a
//! settings file with Podium's hooks, Codex a `notify` program, and recruits
//! the autonomy flags chosen in the settings.

use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct OrchestrationSettings {
    /// "full" | "edits" | "ask": how much recruits may do without asking.
    pub autonomy: String,
    /// Recruits alive at once per orchestrator.
    pub max_recruits: usize,
    /// New recruits wait while free memory is below this.
    pub min_free_memory_mb: u64,
    /// Agent used when `podium spawn` names none.
    pub default_agent: String,
    /// A recruit working or waiting this long without output is reported.
    pub stuck_minutes: u64,
    /// Recruits may not push, open or merge PRs.
    pub guard_git: bool,
}

impl Default for OrchestrationSettings {
    fn default() -> Self {
        OrchestrationSettings {
            autonomy: String::from("full"),
            max_recruits: 6,
            min_free_memory_mb: 1500,
            default_agent: String::from("claude"),
            stuck_minutes: 15,
            guard_git: true,
        }
    }
}

fn hook(event: &str) -> serde_json::Value {
    json!([{ "hooks": [{ "type": "command", "command": format!("podium hook {event}") }] }])
}

/// Writes the Claude Code settings layered on top of the user's own (hooks
/// merge; nothing of the user's is replaced) and returns its path.
pub fn write_claude_settings(app_data: &Path) -> Result<PathBuf, String> {
    let dir = app_data.join("agents");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("claude-settings.json");
    let settings = json!({
        // Recruits started with --dangerously-skip-permissions must not stop
        // on the one-time confirmation screen in the middle of the night.
        "skipDangerousModePermissionPrompt": true,
        "permissions": { "allow": ["Bash(podium:*)", "Bash(podium *)"] },
        "hooks": {
            "SessionStart": hook("claude-session-start"),
            "UserPromptSubmit": hook("claude-prompt"),
            "PreToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "podium hook claude-pretool" }] }],
            "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "podium hook claude-posttool" }] }],
            "Notification": hook("claude-notification"),
            "Stop": hook("claude-stop"),
        }
    });
    let body = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(path)
}

fn shell_quote(value: &str) -> String {
    if cfg!(windows) {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

/// Command line typed into a fresh shell to start `agent`. On Windows the
/// shell wraps `claude` / `codex` itself (see the PowerShell prompt hook), so
/// hooks apply even when the user types the command by hand.
pub fn agent_command(
    agent: &str,
    settings: &OrchestrationSettings,
    claude_settings: Option<&Path>,
    recruit: bool,
    model: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    match agent {
        "claude" => {
            parts.push(String::from("claude"));
            if !cfg!(windows) {
                if let Some(path) = claude_settings {
                    parts.push(String::from("--settings"));
                    parts.push(shell_quote(&path.to_string_lossy()));
                }
            }
            if recruit {
                match settings.autonomy.as_str() {
                    "full" => parts.push(String::from("--dangerously-skip-permissions")),
                    "edits" => parts.push(String::from("--permission-mode acceptEdits")),
                    _ => {}
                }
            }
        }
        "codex" => {
            parts.push(String::from("codex"));
            if !cfg!(windows) {
                parts.push(String::from("-c"));
                parts.push(shell_quote("notify=['podium','hook','codex-notify']"));
            }
            if recruit {
                match settings.autonomy.as_str() {
                    "full" => parts.push(String::from("--dangerously-bypass-approvals-and-sandbox")),
                    "edits" => parts.push(String::from("--full-auto")),
                    _ => {}
                }
            }
        }
        "opencode" => parts.push(String::from("opencode")),
        _ => return String::new(),
    }
    if let Some(model) = model.filter(|m| is_safe_token(m)) {
        parts.push(String::from("--model"));
        parts.push(model.to_owned());
    }
    parts.join(" ")
}

/// Model names are typed into a shell: allow only plain identifiers.
fn is_safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '['| ']'))
}

pub const AGENTS: &[&str] = &["claude", "codex", "opencode", "shell"];

/// The `podium` skill: tells Claude Code and Codex how to use the CLI.
const SKILL: &str = include_str!("../../resources/podium-skill.md");

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Writes the skill into the user's Claude Code and Codex skill folders.
#[tauri::command]
pub fn install_agent_skills() -> Result<(), String> {
    let home = env_dir("USERPROFILE")
        .or_else(|| env_dir("HOME"))
        .ok_or_else(|| String::from("home directory not found"))?;
    let roots = [
        env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
        env_dir("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
    ];
    for root in roots {
        let dir = root.join("skills").join("podium");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(dir.join("SKILL.md"), SKILL).map_err(|e| e.to_string())?;
    }
    Ok(())
}
