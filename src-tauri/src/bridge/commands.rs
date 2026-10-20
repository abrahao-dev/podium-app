//! `podium` subcommands.

use super::agents::{AgentState, AskOutcome, Envelope};
use super::args::Args;
use super::graph::Graph;
use super::launch;
use super::{Caller, Inner, Reply};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Default wait of `podium ask`; agents' shell tools cap commands near here.
const ASK_TIMEOUT: Duration = Duration::from_secs(540);
/// Largest note read in one go without a range.
const NOTE_READ_LINES: usize = 2000;

pub const HELP: &str = "\
podium: work with the agents and notes connected to this terminal on the Podium canvas.

Start with `podium list`. Names are case-insensitive and must be quoted when they contain spaces.
Multi-line text: pass --file <path>, or pipe it with --stdin (PowerShell: `Get-Content x.md -Raw | podium note write \"Plan\" --stdin`).

Agents
  podium list                              who and what you are connected to
  podium status                            state of each connected agent
  podium ask \"Name\" \"prompt\"                 send a prompt and wait for the answer (default up to 9 min)
      --no-wait                            send and return immediately (alias: podium send)
      --timeout <seconds>                  how long to wait
  podium ask --batch '{\"A\":\"..\",\"B\":\"..\"}'  prompt several agents in parallel; waits for all
  podium ask \"Name\" --raw \"text\"             type raw keys (\\n Enter, \\t Tab, \\e Esc, \\x03 Ctrl-C)
  podium reply \"answer\"                      answer whoever asked you (ends their wait)
  podium check \"Name\"                        read an agent's current screen

Notes
  podium note list
  podium note read \"Note\" [start] [count]    with line numbers
  podium note write \"Note\" \"content\"         replace everything
  podium note edit \"Note\" \"old\" \"new\"        replace one exact substring
  podium note append \"Note\" \"content\"
  podium note create [\"content\"] [--name \"Name\"]   new note linked to you

Missions (orchestrators): a task board run by recruits in git worktrees, merged and verified by Podium
  podium mission start \"Title\" [--verify \"npm run build\"] [--setup \"npm ci\"] [--base <branch>]
  podium task add \"Title\" --files \"src/a/**,src/b.ts\" [--deps t1,t2] [--agent claude] [--model m] [--file brief.md]
  podium task run --all                       start everything that can run (or: podium task run t3)
  podium mission status                       the board; also: podium task retry t3 | podium task cancel t3
  podium mission finish                       when every task is settled (podium mission stop aborts)
Workers (recruits running a mission task)
  podium done \"summary\"                       commit and submit your branch for merge
  podium fail \"why\"                           give up on the task

Orchestrators (terminals with the orchestrator switch on)
  podium spawn \"Name\" [--agent claude|codex|opencode|shell] [--dir <path>] [--model <m>] [--role \"..\"] [--prompt \"..\"]
  podium dismiss \"Name\"                      close a recruit you spawned
  podium connect \"A\" \"B\"                     wire two of your recruits / notes together

Rules: never resend a prompt that timed out (its reply still reaches you as a message);
never interrupt an agent that is working; when you hand off a task, the other agent's
reply ends your wait automatically.
";

pub fn run(inner: &Arc<Inner>, caller: &Caller, argv: &[String]) -> Reply {
    let Some((command, rest)) = argv.split_first() else {
        return Reply::ok(HELP);
    };
    let args = Args::parse(rest);
    match command.as_str() {
        "ping" => Reply::ok("pong"),
        "help" | "--help" | "-h" => Reply::ok(HELP),
        "list" => cmd_list(inner, caller),
        "status" => cmd_status(inner, caller),
        "note" | "notes" => cmd_note(inner, caller, &args),
        "ask" => cmd_ask(inner, caller, &args, false),
        "send" => cmd_ask(inner, caller, &args, true),
        "reply" => cmd_reply(inner, caller, &args),
        "check" => cmd_check(inner, caller, &args),
        "spawn" | "recruit" => cmd_spawn(inner, caller, &args),
        "dismiss" => cmd_dismiss(inner, caller, &args),
        "connect" => cmd_connect(inner, caller, &args),
        "hook" => cmd_hook(inner, caller, &args),
        "mission" => cmd_mission(inner, caller, &args),
        "task" | "tasks" => cmd_task(inner, caller, &args),
        "done" => match text_arg(caller, &args, 0) {
            Ok(Some(summary)) if !summary.trim().is_empty() => {
                result(inner.task_done(&caller.id, summary.trim()))
            }
            Ok(_) => Reply::err("usage: podium done \"<summary of what changed>\""),
            Err(e) => Reply::err(e),
        },
        "fail" => match text_arg(caller, &args, 0) {
            Ok(Some(reason)) if !reason.trim().is_empty() => {
                result(inner.task_fail(&caller.id, reason.trim()))
            }
            Ok(_) => Reply::err("usage: podium fail \"<why>\""),
            Err(e) => Reply::err(e),
        },
        other => Reply::err(format!("unknown command '{other}'. Run `podium help`.")),
    }
}

fn result(outcome: Result<String, String>) -> Reply {
    match outcome {
        Ok(text) => Reply::ok(text),
        Err(e) => Reply::err(e),
    }
}

fn list_arg(args: &Args, name: &str) -> Vec<String> {
    args.get(name)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn cmd_mission(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let graph = inner.graph();
    if let Err(r) = require_orchestrator(&graph, caller) {
        return r;
    }
    match args.pos(0).unwrap_or("status") {
        "start" => {
            let Some(title) = args.pos(1).map(str::trim).filter(|t| !t.is_empty()) else {
                return Reply::err("usage: podium mission start \"Title\" [--verify \"cmd\"] [--setup \"cmd\"] [--base <branch>]");
            };
            result(inner.mission_start(
                &caller.id,
                &caller.cwd,
                title,
                args.get("verify").map(str::to_owned),
                args.get("setup").map(str::to_owned),
                args.get("base").map(str::to_owned),
            ))
        }
        "status" => result(inner.mission_status(&caller.id)),
        "stop" => result(inner.mission_stop(&caller.id, false)),
        "finish" => result(inner.mission_stop(&caller.id, true)),
        other => Reply::err(format!("unknown mission command '{other}' (start, status, stop, finish)")),
    }
}

fn cmd_task(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let graph = inner.graph();
    if let Err(r) = require_orchestrator(&graph, caller) {
        return r;
    }
    match args.pos(0).unwrap_or("list") {
        "add" => {
            let Some(title) = args.pos(1).map(str::trim).filter(|t| !t.is_empty()) else {
                return Reply::err("usage: podium task add \"Title\" --files \"src/a/**,b.ts\" [--deps t1,t2] [--agent claude] [--prompt \"...\" | --file brief.md | --stdin]");
            };
            let prompt = match text_arg(caller, args, 2) {
                Ok(text) => text.or_else(|| args.get("prompt").map(str::to_owned)).unwrap_or_default(),
                Err(e) => return Reply::err(e),
            };
            let settings = inner.settings();
            let agent = args.get("agent").unwrap_or(&settings.default_agent).to_lowercase();
            if !matches!(agent.as_str(), "claude" | "codex" | "opencode") {
                return Reply::err("tasks run on claude, codex or opencode");
            }
            result(inner.task_add(
                &caller.id,
                super::mission::NewTask {
                    title: title.to_owned(),
                    prompt: if prompt.trim().is_empty() {
                        title.to_owned()
                    } else {
                        prompt
                    },
                    files: list_arg(args, "files"),
                    deps: list_arg(args, "deps"),
                    agent,
                    model: args.get("model").map(str::to_owned),
                },
            ))
        }
        "list" | "status" => result(inner.mission_status(&caller.id)),
        "run" => result(inner.task_run(&caller.id, args.pos(1).filter(|_| !args.has("all")))),
        "retry" => match args.pos(1) {
            Some(id) => result(inner.task_retry(&caller.id, id)),
            None => Reply::err("usage: podium task retry t3"),
        },
        "cancel" => match args.pos(1) {
            Some(id) => result(inner.task_cancel(&caller.id, id)),
            None => Reply::err("usage: podium task cancel t3"),
        },
        other => Reply::err(format!("unknown task command '{other}' (add, list, run, retry, cancel)")),
    }
}

/// Text argument: `--file`, piped `--stdin`, or the positional args from
/// `from` on, joined by spaces.
fn text_arg(caller: &Caller, args: &Args, from: usize) -> Result<Option<String>, String> {
    if let Some(file) = args.get("file") {
        let path = Path::new(&caller.cwd).join(file);
        return std::fs::read_to_string(&path)
            .map(Some)
            .map_err(|e| format!("cannot read {}: {e}", path.display()));
    }
    if args.has("stdin") {
        return Ok(caller.stdin.clone());
    }
    let rest: Vec<&str> = args.positional.iter().skip(from).map(String::as_str).collect();
    Ok((!rest.is_empty()).then(|| rest.join(" ")))
}

fn state_of(inner: &Inner, id: &str) -> &'static str {
    inner.agent_state(id).map_or("not started", AgentState::label)
}

fn cmd_list(inner: &Arc<Inner>, caller: &Caller) -> Reply {
    let graph = inner.graph();
    let Some(me) = graph.node(&caller.id) else {
        return Reply::err("this terminal is not on the canvas");
    };
    let mut out = format!("You: {}", me.name);
    if me.orchestrator {
        out.push_str(" (orchestrator: you may spawn recruits)");
    }
    out.push('\n');

    let peers = graph.reachable_terminals(&caller.id);
    out.push_str("Agents:\n");
    if peers.is_empty() {
        out.push_str("  (none connected)\n");
    }
    for peer in &peers {
        let mut tags = vec![state_of(inner, &peer.id).to_owned()];
        if peer.parent.as_deref() == Some(caller.id.as_str()) {
            tags.push(String::from("your recruit"));
        }
        if me.parent.as_deref() == Some(peer.id.as_str()) {
            tags.push(String::from("your orchestrator"));
        }
        if !graph.may_message(&caller.id, &peer.id) {
            tags.push(String::from("does not accept prompts"));
        }
        out.push_str(&format!("  - {} ({})\n", peer.name, tags.join(", ")));
    }

    out.push_str("Notes:\n");
    let notes = graph.reachable_notes(&caller.id);
    if notes.is_empty() {
        out.push_str("  (none connected)\n");
    }
    for (depth, note) in notes {
        out.push_str(&"  ".repeat(depth + 1));
        out.push_str(&format!("- {}", note.name));
        if note.locked {
            out.push_str(" (locked: read-only)");
        }
        out.push('\n');
    }
    Reply::ok(out)
}

fn cmd_status(inner: &Arc<Inner>, caller: &Caller) -> Reply {
    let graph = inner.graph();
    let peers = graph.reachable_terminals(&caller.id);
    if peers.is_empty() {
        return Reply::ok("No agents connected.");
    }
    let state = match inner.state.lock() {
        Ok(state) => state,
        Err(e) => return Reply::err(e.to_string()),
    };
    let mut out = String::new();
    for peer in peers {
        match state.agents.entries.get(&peer.id) {
            Some(entry) => {
                let secs = entry.since.elapsed().as_secs();
                out.push_str(&format!(
                    "{}: {} for {}m{:02}s{}{}\n",
                    peer.name,
                    entry.state.label(),
                    secs / 60,
                    secs % 60,
                    entry.kind.as_deref().map(|k| format!(", {k}")).unwrap_or_default(),
                    if entry.inbox.is_empty() {
                        String::new()
                    } else {
                        format!(", {} queued message(s)", entry.inbox.len())
                    },
                ));
            }
            None => out.push_str(&format!("{}: not started\n", peer.name)),
        }
    }
    Reply::ok(out)
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

fn resolve_note<'g>(graph: &'g Graph, caller: &Caller, name: Option<&str>) -> Result<&'g super::graph::Node, Reply> {
    let Some(name) = name else {
        return Err(Reply::err("missing note name. Run `podium note list`."));
    };
    graph.find_note(&caller.id, name).ok_or_else(|| {
        Reply::err(format!(
            "no note named \"{name}\" is connected to you. Run `podium note list`."
        ))
    })
}

fn note_text(inner: &Inner, id: &str) -> Result<String, String> {
    let value = inner.ui_request("note", json!({ "op": "read", "id": id }))?;
    Ok(value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

fn note_write(inner: &Inner, id: &str, text: &str) -> Result<String, String> {
    let value = inner.ui_request("note", json!({ "op": "write", "id": id, "text": text }))?;
    Ok(value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

fn renamed_hint(old: &str, new: &str) -> String {
    if new.is_empty() || new == old {
        String::new()
    } else {
        format!(" The note is now named \"{new}\" (its first line changed).")
    }
}

fn cmd_note(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let graph = inner.graph();
    let op = args.pos(0).unwrap_or("list");
    match op {
        "list" => {
            let notes = graph.reachable_notes(&caller.id);
            if notes.is_empty() {
                return Reply::ok("No notes connected. Create one with: podium note create \"content\" --name \"Name\"");
            }
            let mut out = String::new();
            for (depth, note) in notes {
                out.push_str(&"  ".repeat(depth));
                out.push_str(&format!("- {}{}\n", note.name, if note.locked { " (locked)" } else { "" }));
            }
            Reply::ok(out)
        }
        "read" => {
            let note = match resolve_note(&graph, caller, args.pos(1)) {
                Ok(n) => n,
                Err(r) => return r,
            };
            let text = match note_text(inner, &note.id) {
                Ok(t) => t,
                Err(e) => return Reply::err(e),
            };
            let start = args
                .pos(2)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            let count = args
                .pos(3)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(NOTE_READ_LINES);
            let lines: Vec<&str> = text.lines().collect();
            if lines.is_empty() {
                return Reply::ok(format!("(\"{}\" is empty)", note.name));
            }
            let width = lines.len().to_string().len();
            let mut out = String::new();
            for (i, line) in lines.iter().enumerate().skip(start - 1).take(count) {
                out.push_str(&format!("{:>width$}  {line}\n", i + 1));
            }
            let shown_end = (start - 1 + count).min(lines.len());
            if shown_end < lines.len() {
                out.push_str(&format!(
                    "... {} more lines. Continue with: podium note read \"{}\" {} {}\n",
                    lines.len() - shown_end,
                    note.name,
                    shown_end + 1,
                    count
                ));
            }
            Reply::ok(out)
        }
        "write" | "append" | "edit" => {
            let note = match resolve_note(&graph, caller, args.pos(1)) {
                Ok(n) => n,
                Err(r) => return r,
            };
            if note.locked {
                return Reply::err(format!("\"{}\" is locked (read-only).", note.name));
            }
            let result = if op == "edit" {
                let (Some(old), Some(new)) = (args.pos(2), args.pos(3)) else {
                    return Reply::err("usage: podium note edit \"Note\" \"old text\" \"new text\"");
                };
                note_text(inner, &note.id).and_then(|text| {
                    match text.matches(old).count() {
                        0 => Err(String::from("the old text was not found (it must match exactly)")),
                        1 => note_write(inner, &note.id, &text.replacen(old, new, 1)),
                        n => Err(format!("the old text appears {n} times; include more context so it is unique")),
                    }
                })
            } else {
                let content = match text_arg(caller, args, 2) {
                    Ok(Some(c)) => c,
                    Ok(None) => return Reply::err("missing content (pass it, or use --file / --stdin)"),
                    Err(e) => return Reply::err(e),
                };
                if op == "append" {
                    note_text(inner, &note.id).and_then(|text| {
                        let joined = if text.is_empty() {
                            content
                        } else if text.ends_with('\n') {
                            format!("{text}{content}")
                        } else {
                            format!("{text}\n{content}")
                        };
                        note_write(inner, &note.id, &joined)
                    })
                } else {
                    note_write(inner, &note.id, &content)
                }
            };
            match result {
                Ok(name) => Reply::ok(format!("Updated \"{}\".{}", note.name, renamed_hint(&note.name, &name))),
                Err(e) => Reply::err(e),
            }
        }
        "create" => {
            let content = match text_arg(caller, args, 1) {
                Ok(c) => c.unwrap_or_default(),
                Err(e) => return Reply::err(e),
            };
            let value = inner.ui_request(
                "note",
                json!({ "op": "create", "owner": caller.id, "text": content, "name": args.get("name") }),
            );
            match value {
                Ok(v) => Reply::ok(format!(
                    "Created note \"{}\" linked to you.",
                    v.get("name").and_then(Value::as_str).unwrap_or("Untitled")
                )),
                Err(e) => Reply::err(e),
            }
        }
        other => Reply::err(format!("unknown note command '{other}'")),
    }
}

// ---------------------------------------------------------------------------
// Messaging
// ---------------------------------------------------------------------------

/// Decodes `\n`, `\t`, `\e`, `\r`, `\\` and `\xNN` escapes of raw input.
fn decode_escapes(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\r'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(b) => out.push(char::from(b)),
                    Err(_) => {
                        out.push_str("\\x");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn resolve_target<'g>(graph: &'g Graph, caller: &Caller, name: &str) -> Result<&'g super::graph::Node, String> {
    let node = graph.find_terminal(&caller.id, name).ok_or_else(|| {
        format!("no agent named \"{name}\" is connected to you. Run `podium list`.")
    })?;
    if !graph.may_message(&caller.id, &node.id) {
        return Err(format!(
            "\"{}\" does not accept prompts from connected agents (its incoming switch is off).",
            node.name
        ));
    }
    Ok(node)
}

struct Pending {
    name: String,
    ask_id: u64,
    rx: mpsc::Receiver<AskOutcome>,
    target: String,
}

/// Sends a prompt that expects an answer. If the target is itself waiting on
/// the caller, this *is* the answer: it ends the target's wait instead.
fn start_ask(inner: &Arc<Inner>, caller: &Caller, target: &super::graph::Node, text: &str, expect_reply: bool) -> Result<Option<Pending>, String> {
    let answered = {
        let mut state = inner.state.lock().map_err(|e| e.to_string())?;
        if state.agents.pending_from(&target.id, &caller.id) {
            let reply = text.to_owned();
            let orphaned = state
                .agents
                .resolve(&caller.id, Some(&target.id), false, || AskOutcome::Reply(reply.clone()));
            Some(orphaned)
        } else {
            None
        }
    };
    if let Some(orphaned) = answered {
        inner.notify_orphans(&caller.id, orphaned);
        return Ok(None);
    }
    if !expect_reply {
        inner.enqueue(&target.id, Envelope { from: Some(caller.id.clone()), text: text.to_owned(), raw: false, ask: None });
        return Ok(None);
    }
    let (tx, rx) = mpsc::channel();
    let ask_id = {
        let mut state = inner.state.lock().map_err(|e| e.to_string())?;
        state.agents.new_ask(&caller.id, &target.id, tx)
    };
    inner.enqueue(
        &target.id,
        Envelope {
            from: Some(caller.id.clone()),
            text: text.to_owned(),
            raw: false,
            ask: Some(ask_id),
        },
    );
    Ok(Some(Pending {
        name: target.name.clone(),
        ask_id,
        rx,
        target: target.id.clone(),
    }))
}

fn describe_outcome(inner: &Inner, pending: &Pending, outcome: AskOutcome) -> Reply {
    match outcome {
        AskOutcome::Reply(text) => Reply::ok(text),
        AskOutcome::Finished => {
            let screen = inner.pty().screen_text(&pending.target).unwrap_or_default();
            Reply::ok(format!(
                "\"{}\" finished its turn without an explicit reply. Its screen:\n{screen}",
                pending.name
            ))
        }
        AskOutcome::Exited => Reply::err(format!("\"{}\" exited before replying.", pending.name)),
    }
}

fn timed_out(inner: &Inner, pending: &Pending, waited: Duration) -> Reply {
    if let Ok(mut state) = inner.state.lock() {
        state.agents.detach(pending.ask_id);
    }
    Reply::ok(format!(
        "No reply from \"{}\" after {}s (it is {}). Do NOT resend: its reply will reach you as a new message when it is done. Check progress with: podium check \"{}\"",
        pending.name,
        waited.as_secs(),
        state_of(inner, &pending.target),
        pending.name
    ))
}

fn cmd_ask(inner: &Arc<Inner>, caller: &Caller, args: &Args, no_wait: bool) -> Reply {
    let graph = inner.graph();
    let timeout = args
        .number("timeout")
        .map_or(ASK_TIMEOUT, Duration::from_secs);
    if let Some(batch) = args.get("batch") {
        return cmd_ask_batch(inner, caller, &graph, batch, timeout);
    }
    let Some(name) = args.pos(0) else {
        return Reply::err("usage: podium ask \"Name\" \"prompt\"");
    };
    let target = match resolve_target(&graph, caller, name) {
        Ok(t) => t,
        Err(e) => return Reply::err(e),
    };
    if let Some(raw) = args.get("raw") {
        inner.enqueue(&target.id, Envelope { from: Some(caller.id.clone()), text: decode_escapes(raw), raw: true, ask: None });
        return Reply::ok(format!("Sent raw input to \"{}\".", target.name));
    }
    let text = match text_arg(caller, args, 1) {
        Ok(Some(t)) if !t.trim().is_empty() => t,
        Ok(_) => return Reply::err("missing prompt"),
        Err(e) => return Reply::err(e),
    };
    let no_wait = no_wait || args.has("no-wait");
    let before = inner.agent_state(&target.id);
    match start_ask(inner, caller, target, &text, !no_wait) {
        Err(e) => Reply::err(e),
        Ok(None) => Reply::ok(match before {
            Some(AgentState::Idle) | None => format!("Delivered to \"{}\".", target.name),
            Some(s) => format!(
                "Queued for \"{}\" (it is {}); it is delivered when it is idle.",
                target.name,
                s.label()
            ),
        }),
        Ok(Some(pending)) => match pending.rx.recv_timeout(timeout) {
            Ok(outcome) => describe_outcome(inner, &pending, outcome),
            Err(_) => timed_out(inner, &pending, timeout),
        },
    }
}

fn cmd_ask_batch(inner: &Arc<Inner>, caller: &Caller, graph: &Graph, batch: &str, timeout: Duration) -> Reply {
    let map: serde_json::Map<String, Value> = match serde_json::from_str(batch) {
        Ok(Value::Object(map)) => map,
        _ => return Reply::err("--batch expects a JSON object: {\"Agent\": \"prompt\", ...}"),
    };
    let mut results = Vec::new();
    let mut waits = Vec::new();
    for (name, prompt) in &map {
        let Some(prompt) = prompt.as_str() else {
            results.push(json!({ "name": name, "error": "prompt must be a string" }));
            continue;
        };
        match resolve_target(graph, caller, name).and_then(|t| start_ask(inner, caller, t, prompt, true)) {
            Ok(Some(pending)) => waits.push(pending),
            Ok(None) => results.push(json!({ "name": name, "output": "delivered" })),
            Err(e) => results.push(json!({ "name": name, "error": e })),
        }
    }
    let deadline = std::time::Instant::now() + timeout;
    for pending in waits {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let reply = match pending.rx.recv_timeout(left) {
            Ok(outcome) => describe_outcome(inner, &pending, outcome),
            Err(_) => timed_out(inner, &pending, timeout),
        };
        if reply.exit == 0 {
            results.push(json!({ "name": pending.name, "output": reply.stdout.trim_end() }));
        } else {
            results.push(json!({ "name": pending.name, "error": reply.stderr.trim_end() }));
        }
    }
    Reply::ok(serde_json::to_string_pretty(&results).unwrap_or_default())
}

fn cmd_reply(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let text = match text_arg(caller, args, 0) {
        Ok(Some(t)) if !t.trim().is_empty() => t,
        Ok(_) => return Reply::err("usage: podium reply \"answer\" (or --file / --stdin)"),
        Err(e) => return Reply::err(e),
    };
    let (askers, orphaned, last_sender) = {
        let mut state = match inner.state.lock() {
            Ok(s) => s,
            Err(e) => return Reply::err(e.to_string()),
        };
        let askers: Vec<String> = state
            .agents
            .asks
            .iter()
            .filter(|a| a.to == caller.id)
            .map(|a| a.from.clone())
            .collect();
        let reply = text.clone();
        let orphaned = state
            .agents
            .resolve(&caller.id, None, false, || AskOutcome::Reply(reply.clone()));
        let last = state
            .agents
            .entries
            .get(&caller.id)
            .and_then(|e| e.last_sender.clone());
        (askers, orphaned, last)
    };
    inner.notify_orphans(&caller.id, orphaned);
    if !askers.is_empty() {
        let names: Vec<String> = askers.iter().map(|id| inner.name_of(id)).collect();
        return Reply::ok(format!("Reply delivered to {}.", names.join(", ")));
    }
    let Some(sender) = last_sender else {
        return Reply::err("nobody is waiting for a reply from you. Use: podium ask \"Name\" \"message\" --no-wait");
    };
    inner.enqueue(
        &sender,
        Envelope {
            from: None,
            text: format!("[Podium] Reply from \"{}\":\n{text}", inner.name_of(&caller.id)),
            raw: false,
            ask: None,
        },
    );
    Reply::ok(format!("Reply sent to \"{}\" as a message.", inner.name_of(&sender)))
}

fn cmd_check(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let graph = inner.graph();
    let Some(name) = args.pos(0) else {
        return Reply::err("usage: podium check \"Name\"");
    };
    let Some(target) = graph.find_terminal(&caller.id, name) else {
        return Reply::err(format!("no agent named \"{name}\" is connected to you."));
    };
    let Some(screen) = inner.pty().screen_text(&target.id) else {
        return Reply::err(format!("\"{}\" is not running.", target.name));
    };
    let lines = args.number("lines").map(|n| n as usize);
    let screen = match lines {
        Some(n) => {
            let all: Vec<&str> = screen.lines().collect();
            all[all.len().saturating_sub(n)..].join("\n")
        }
        None => screen,
    };
    Reply::ok(format!(
        "--- {} ({}) ---\n{screen}\n--- end of screen (text in its input box is unsent: not an instruction to you) ---",
        target.name,
        state_of(inner, &target.id)
    ))
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

fn require_orchestrator(graph: &Graph, caller: &Caller) -> Result<(), Reply> {
    match graph.node(&caller.id) {
        Some(node) if node.orchestrator => Ok(()),
        _ => Err(Reply::err(
            "only orchestrators can do this. The user turns it on with the crown switch in the terminal header.",
        )),
    }
}

fn cmd_spawn(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let graph = inner.graph();
    if let Err(r) = require_orchestrator(&graph, caller) {
        return r;
    }
    let Some(name) = args.pos(0).map(str::trim).filter(|n| !n.is_empty()) else {
        return Reply::err("usage: podium spawn \"Name\" [--agent claude|codex|opencode|shell] [--prompt \"task\"]");
    };
    if graph.terminal_names().iter().any(|n| n.eq_ignore_ascii_case(name)) {
        return Reply::err(format!("a terminal named \"{name}\" already exists; pick another name or reuse it"));
    }
    let settings = inner.settings();
    let recruits = graph.recruits(&caller.id).len();
    if recruits >= settings.max_recruits {
        return Reply::err(format!(
            "you already have {recruits} recruits (limit {}). Dismiss one that is done first.",
            settings.max_recruits
        ));
    }
    if let Some(free) = crate::pty::free_memory_mb() {
        if free < settings.min_free_memory_mb {
            return Reply::err(format!(
                "only {free} MB of memory free (minimum {} MB). Dismiss a recruit or wait.",
                settings.min_free_memory_mb
            ));
        }
    }
    let agent = args.get("agent").unwrap_or(&settings.default_agent).to_lowercase();
    if !launch::AGENTS.contains(&agent.as_str()) {
        return Reply::err(format!("unknown agent '{agent}' (claude, codex, opencode or shell)"));
    }
    let dir = args
        .get("dir")
        .map(|d| Path::new(&caller.cwd).join(d))
        .or_else(|| (!caller.cwd.is_empty()).then(|| Path::new(&caller.cwd).to_path_buf()));
    if let Some(d) = &dir {
        if !d.is_dir() {
            return Reply::err(format!("directory not found: {}", d.display()));
        }
    }
    let command = launch::agent_command(&agent, &settings, inner.claude_settings.as_deref(), true, args.get("model"));
    let task = match text_arg(caller, args, 1) {
        Ok(t) => t.or_else(|| args.get("prompt").map(str::to_owned)),
        Err(e) => return Reply::err(e),
    };
    let created = inner.ui_request(
        "spawn",
        json!({
            "parent": caller.id,
            "name": name,
            "command": command,
            "agent": agent,
            "cwd": dir.as_ref().map(|d| d.to_string_lossy().into_owned()),
        }),
    );
    let id = match created {
        Ok(v) => match v.get("id").and_then(Value::as_str) {
            Some(id) => id.to_owned(),
            None => return Reply::err("the canvas did not create the terminal"),
        },
        Err(e) => return Reply::err(e),
    };
    if let Ok(mut state) = inner.state.lock() {
        if let Some(entry) = state.agents.entries.get_mut(&id) {
            entry.kind = Some(agent.clone());
        }
    }
    let mut first = String::new();
    if let Some(role) = args.get("role") {
        first.push_str(&format!("Your role: {role}\n\n"));
    }
    first.push_str(&format!(
        "You are \"{name}\", a recruit of \"{}\" on the Podium canvas. Run `podium help` to see how to talk to it; report results with `podium reply` when it asks you something.",
        inner.name_of(&caller.id)
    ));
    if let Some(task) = task.filter(|t| !t.trim().is_empty()) {
        first.push_str("\n\nTask:\n");
        first.push_str(&task);
    }
    if agent != "shell" {
        inner.enqueue(&id, Envelope { from: Some(caller.id.clone()), text: first, raw: false, ask: None });
    }
    Reply::ok(format!(
        "Spawned \"{name}\" ({agent}). It boots in a few seconds; prompts sent now are queued until it is ready. Ask it with: podium ask \"{name}\" \"...\""
    ))
}

fn cmd_dismiss(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let graph = inner.graph();
    let Some(name) = args.pos(0) else {
        return Reply::err("usage: podium dismiss \"Name\"");
    };
    let Some(target) = graph.find_terminal(&caller.id, name) else {
        return Reply::err(format!("no agent named \"{name}\" is connected to you."));
    };
    if target.parent.as_deref() != Some(caller.id.as_str()) {
        return Reply::err(format!("\"{}\" is not your recruit.", target.name));
    }
    match inner.ui_request("dismiss", json!({ "id": target.id })) {
        Ok(_) => Reply::ok(format!("Dismissed \"{}\".", target.name)),
        Err(e) => Reply::err(e),
    }
}

fn cmd_connect(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let graph = inner.graph();
    if let Err(r) = require_orchestrator(&graph, caller) {
        return r;
    }
    let (Some(a), Some(b)) = (args.pos(0), args.pos(1)) else {
        return Reply::err("usage: podium connect \"A\" \"B\"");
    };
    let team = |name: &str| -> Option<String> {
        if graph.node(&caller.id).is_some_and(|n| n.name.eq_ignore_ascii_case(name.trim())) {
            return Some(caller.id.clone());
        }
        graph
            .find_terminal(&caller.id, name)
            .filter(|n| n.parent.as_deref() == Some(caller.id.as_str()))
            .or_else(|| graph.find_note(&caller.id, name))
            .map(|n| n.id.clone())
    };
    let (Some(a_id), Some(b_id)) = (team(a), team(b)) else {
        return Reply::err("both ends must be you, one of your recruits, or a note you can reach");
    };
    match inner.ui_request("connect", json!({ "a": a_id, "b": b_id })) {
        Ok(_) => Reply::ok(format!("Connected \"{a}\" and \"{b}\".")),
        Err(e) => Reply::err(e),
    }
}

// ---------------------------------------------------------------------------
// Agent hooks
// ---------------------------------------------------------------------------

fn cmd_hook(inner: &Arc<Inner>, caller: &Caller, args: &Args) -> Reply {
    let payload: Value = caller
        .stdin
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let set = |state: AgentState, kind: &str| {
        if let Ok(mut s) = inner.state.lock() {
            if let Some(entry) = s.agents.entries.get_mut(&caller.id) {
                entry.kind = Some(kind.to_owned());
            }
        }
        inner.set_agent_state(&caller.id, state, true);
    };
    match args.pos(0).unwrap_or_default() {
        "claude-session-start" => set(AgentState::Idle, "claude"),
        "claude-prompt" | "claude-posttool" => set(AgentState::Working, "claude"),
        "claude-stop" => set(AgentState::Idle, "claude"),
        "claude-notification" => {
            let kind = payload.get("notification_type").and_then(Value::as_str).unwrap_or_default();
            let message = payload.get("message").and_then(Value::as_str).unwrap_or_default().to_lowercase();
            if kind == "permission_prompt" || (kind.is_empty() && message.contains("permission")) {
                set(AgentState::WaitingInput, "claude");
            } else if kind == "idle_prompt" || message.contains("waiting for your input") {
                set(AgentState::Idle, "claude");
            }
        }
        "claude-pretool" => {
            if let Some(block) = guard_tool(inner, caller, &payload) {
                return Reply {
                    stdout: String::new(),
                    stderr: format!("{block}\n"),
                    exit: 2,
                };
            }
            set(AgentState::Working, "claude");
        }
        "codex-notify" => {
            // Codex passes its JSON payload as the last argument.
            let event: Value = args
                .positional
                .last()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            if event.get("type").and_then(Value::as_str) == Some("agent-turn-complete") {
                set(AgentState::Idle, "codex");
            }
        }
        _ => {}
    }
    Reply::silent()
}

/// Commands recruits may never run: integration and publishing stay with
/// the orchestrator and, ultimately, the user.
fn guard_tool(inner: &Arc<Inner>, caller: &Caller, payload: &Value) -> Option<String> {
    let tool = payload.get("tool_name").and_then(Value::as_str).unwrap_or_default();
    if matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
        let file = payload
            .pointer("/tool_input/file_path")
            .or_else(|| payload.pointer("/tool_input/notebook_path"))
            .and_then(Value::as_str)?;
        return inner.lease_violation(&caller.id, file);
    }
    let graph = inner.graph();
    let recruit = graph.node(&caller.id).is_some_and(|n| n.parent.is_some());
    if !recruit || !inner.settings().guard_git {
        return None;
    }
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    let command = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let squashed = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let blocked = ["git push", "gh pr create", "gh pr merge", "gh release"]
        .iter()
        .find(|pattern| squashed.contains(*pattern))?;
    Some(format!(
        "Blocked by Podium: recruits may not run `{blocked}`. Commit your work locally and report back; integration and publishing are done by the orchestrator after the user reviews."
    ))
}
