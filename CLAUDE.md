# Podium

Infinite spatial canvas for terminal windows running AI coding agents (Claude Code, Codex, OpenCode, plain shells) plus markdown sticky notes. Terminals connect to terminals (agent-to-agent prompt piping via PTY stdin) and to notes (output capture). No built-in AI — orchestrates external CLI agents the user has installed.

Primary target: **Windows**. Development on macOS. Also supported: **Linux** (X11/Wayland via WebKitGTK). All three must build and run at all times.

## Stack (fixed — do not substitute)

- Tauri 2 (Rust backend) + React 18 + TypeScript strict + Vite
- xterm.js (`@xterm/xterm` + `@xterm/addon-fit` + `@xterm/addon-webgl`)
- `portable-pty` (Rust) — ConPTY on Windows, openpty on Unix
- zustand (frontend state)
- React Flow (`@xyflow/react`) for canvas/nodes/edges
- Persistence: plain JSON in Tauri `app_data_dir` (no database)

Keep deps minimal. Never update pinned versions once installed.

## PTY IPC contract

Rust side: `src-tauri/src/pty.rs` — **all platform-specific code lives in this module only**.

Tauri commands (frontend wrapper: `src/lib/pty.ts`):

| Command | Args | Notes |
|---|---|---|
| `create_terminal` | `id, cwd?, command?, cols, rows` | Spawns default shell; if `command` given, writes `command + "\r"` into stdin after spawn (PATH resolution via shell, robust cross-platform) |
| `write_to_terminal` | `id, data` | Raw bytes to PTY stdin. **Also powers agent-to-agent connections** |
| `resize_terminal` | `id, cols, rows` | |
| `kill_terminal` | `id` | |

Events (Rust → frontend):
- `terminal-output:{id}` — payload `Vec<u8>` (raw bytes; frontend feeds Uint8Array straight to xterm so its decoder handles split UTF-8)
- `terminal-exit:{id}` — payload `Option<u32>` exit code. Session self-removes from the manager map on EOF.

One dedicated reader thread per session, 4KB reads.

Default shell: `$SHELL` on Unix; if unset, first existing of `/bin/bash` → `/bin/zsh` → `/bin/sh` (bash is ubiquitous on Linux, zsh the macOS default). Windows prefers `pwsh.exe` on PATH, else `powershell.exe`.

## Cross-platform rules

- All paths via Tauri path APIs / Rust `PathBuf`. Never concatenate separators manually.
- Never assume login shell env on Windows; agent commands resolve PATH through the spawned shell.
- Write `\r` (not `\n`) to submit input to PTYs on both platforms.
- Ctrl+C, arrows, etc. pass through xterm.js to the PTY untouched.
- No `React.StrictMode`: terminal mount effects spawn real PTYs; double-invoke would spawn/kill twice.
- CI builds on `windows-latest` + `macos-latest` + `ubuntu-latest` (`.github/workflows/ci.yml`); keep Rust platform-conditional code inside `pty.rs`. Linux needs WebKitGTK dev libs — see the workflow's apt step.

## Frontend layout

```
src/
├── main.tsx / App.tsx
├── lib/pty.ts            # IPC wrapper + event subscriptions
├── store/workspace.ts    # zustand: nodes (React Flow state)
├── canvas/Canvas.tsx     # React Flow wrapper
└── nodes/TerminalNode.tsx  # xterm instance per node; drag via header only
```

TerminalNode: header = label + command + status dot (starting/running/exited); body = xterm (`nodrag nowheel`); NodeResizer when selected; ResizeObserver → fit addon → `resize_terminal`.

Pending performance rule (Phase 2+): only xterm instances visible in viewport and above ~40% zoom render live; others buffer data behind a lightweight placeholder.

## Phase status

- [x] **Phase 1 — Skeleton + one working terminal.** Full PTY round-trip (typing, colors, resize), `claude` launchable in the terminal.
- [x] **Phase 2 — Multiple terminals + notes + persistence.** Toolbar spawn, drag/resize/rename/close, markdown notes (dependency-free React renderer — no HTML injection), workspace JSON via `save_workspace`/`load_workspace` commands (`src-tauri/src/workspace.rs`), debounced auto-save + 10s flush. Restored terminals show Start overlay; never auto-run.
- [x] **Phase 3 — Connections.** Handles (terminal: left target + right source; note: left target), animated arrow edges, double-click edge deletes. Footer prompt input → connected terminals' stdin + `\r`. Capture → Note appends last 20 lines timestamped.
- [x] **Phase 4 — Agent-to-agent handoff.** Header toggle ⊘/⇥ = `allowIncoming` (persisted). Delegate wraps source's last output in handoff template, writes to allowed targets + `\r`. No output parsing heuristics.
- [ ] Phase 5 — Windows pass (ConPTY quirks).

UI notes: webview native context menu suppressed globally (main.tsx). `deleteKeyCode` disabled — nodes close via × only.

Non-goals for MVP: floors/cloning, embedded browsers, scheduled prompts, on-device AI, SSH, licensing, multi-workspace polish, search, custom canvas engine, auto-updates.

## Quality bar

- TypeScript strict, no `any`.
- `cargo clippy` clean.
- Every phase ends with the app launching + deliverable demonstrable.
- Conventional commit per phase.
