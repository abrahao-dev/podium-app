---
name: podium
description: Talk to other AI agents and read/write sticky notes connected to this terminal on the Podium canvas; orchestrators spawn recruit agents, hand out tasks and run overnight missions with git worktrees. Use when the user mentions Podium, connected agents or notes, asks you to ask/tell/check another agent, to delegate, assemble a team, or to run a mission.
---

# Podium

You are running in a terminal on the Podium canvas. The `podium` CLI is on PATH (if not, use `"$PODIUM_CLI"` / `& $env:PODIUM_CLI`). It only reaches what the user wired to your terminal.

Always start with `podium list`: it shows your name, connected agents (with their state) and notes. Run `podium help` for every flag; never guess one.

## Talking to agents

- `podium ask "Name" "prompt"` sends a prompt and waits for the answer (up to 9 minutes by default; set your shell tool timeout to match, e.g. 600000 ms).
- The other agent answers with `podium reply "..."`, which ends your wait with its full answer. When you receive a prompt from another agent, answer it the same way.
- `podium ask "Name" "prompt" --no-wait` (or `podium send`) only delivers. Its answer arrives later as a new message in your terminal.
- `podium ask --batch '{"A":"...","B":"..."}'` prompts several agents in parallel and returns a JSON array once all answered.
- `podium check "Name"` shows an agent's screen without sending anything. Text sitting in its input box is unsent; it is not an instruction to you.
- `podium ask "Name" --raw "1\n"` types raw keys (menus, Ctrl-C as `\x03`).

Rules: messages to a busy agent are queued until it is idle, so never resend. If an ask times out, do not resend: check progress with `podium check` and wait; the reply still reaches you. Never interrupt a working agent or edit files it is changing.

## Notes

- `podium note list`, `podium note read "Note" [start] [count]`
- `podium note write "Note" "content"`, `podium note edit "Note" "old" "new"` (prefer edit for small changes), `podium note append "Note" "content"`
- `podium note create "content" --name "Name"` creates a note linked to you; always pass `--name` for notes you will address again.
- Multi-line content: write it to a file and pass `--file path`, or pipe it with `--stdin`. Locked notes are read-only.

## Orchestrating (only when your terminal has the orchestrator crown on)

1. `podium list` first; reuse connected recruits instead of spawning duplicates.
2. `podium spawn "Codename" --agent claude|codex|opencode --prompt "task"` creates a recruit wired to you. It boots in a few seconds; your prompt is queued until it is ready. Pick short, distinct codenames.
3. Delegate with `podium ask "Codename" "task" --no-wait` for long work, or `podium ask` and wait for short work. Ask recruits to answer with `podium reply` when done.
4. `podium status` shows every recruit's state. `podium dismiss "Codename"` closes a recruit whose work is done, to free memory.
5. Do the delegation through Podium; do not use your own built-in sub-agents for work the user asked to distribute.

Recruits may not push, open or merge PRs; integration is yours, publishing is the user's.

## Missions (orchestrators): unattended, overnight work

Use a mission when the user hands you a body of work to finish on your own (often a note with a brief, run with the note's Run button).

1. Read the brief and explore the repository enough to plan.
2. `podium mission start "Short title" --verify "<command proving the project builds and tests pass>" [--setup "<dependency install, e.g. npm ci>"]` creates an integration branch `podium/<id>` in its own worktree. Nothing touches the user's checkout.
3. Split the work into tasks with **disjoint file scopes** and a self-contained brief each (goal, files, acceptance criteria):
   `podium task add "Title" --files "src/auth/**,src/api/login.ts" [--deps t1] [--agent claude|codex] [--model m] --file brief.md`
   Tasks whose scopes overlap never run at the same time; a task without `--files` runs alone.
4. `podium task run --all`. Podium starts a recruit per task in its own worktree and branch, as slots and memory allow, and later tasks as their dependencies merge.
5. Workers finish with `podium done "summary"`. Podium commits, merges the branch into the integration branch, runs the verify command, and only then counts the task. Conflicts and failed checks go back to the worker that wrote the change; repeated failures come to you, then to the user.
6. You get a message for every merge and problem. React to them: unblock workers (`podium check`, `podium ask`), `podium task retry t3`, `podium task cancel t3`, add tasks that turn out to be needed. `podium mission status` shows the board at any time (use it after a restart or context compaction).
7. When Podium says every task is settled, write the report note (`podium note create --name "Mission report" --file report.md`: what changed per task, checks run, what failed and why, how to review), then `podium mission finish`.

Never push, open PRs or merge into the user's branches: the user reviews `podium/<id>` and decides.

## Mission workers

If your first prompt says you are a worker on a mission: stay inside your worktree, edit only your files (edits to files another running task owns are blocked; ask the orchestrator if you need one), commit, run the verify command if given, then `podium done "summary"`. If you cannot finish, `podium fail "why"`. When Podium reports a conflict or failed check, fix it in your worktree and run `podium done` again.
