import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface CreateTerminalOptions {
  id: string;
  cwd?: string;
  command?: string;
  cols: number;
  rows: number;
}

/** Spawns the PTY. Raw output bytes arrive through `onOutput` (coalesced to
 *  at most one message per frame); pass them straight to xterm so its decoder
 *  handles split UTF-8. */
export function createTerminal(
  opts: CreateTerminalOptions,
  onOutput: (data: Uint8Array) => void,
): Promise<void> {
  const channel = new Channel<ArrayBuffer>();
  channel.onmessage = (buffer) => onOutput(new Uint8Array(buffer));
  return invoke("create_terminal", {
    id: opts.id,
    cwd: opts.cwd ?? null,
    command: opts.command ?? null,
    cols: opts.cols,
    rows: opts.rows,
    onOutput: channel,
  });
}

/** Tells the backend whether the webview is visible; output is restored from
 *  the backend's screen mirror when it shows again. */
export function setUiVisible(visible: boolean): Promise<void> {
  return invoke("set_ui_visible", { visible });
}

/** Backpressure: hold this terminal's output while xterm catches up. */
export function setTerminalOutputPaused(
  id: string,
  paused: boolean,
): Promise<void> {
  return invoke("set_terminal_output_paused", { id, paused });
}

export function writeToTerminal(id: string, data: string): Promise<void> {
  return invoke("write_to_terminal", { id, data });
}

export function resizeTerminal(
  id: string,
  cols: number,
  rows: number,
): Promise<void> {
  return invoke("resize_terminal", { id, cols, rows });
}

/** Shell's current directory per terminal id; unknown / not running ids are omitted. */
export function getTerminalCwds(ids: string[]): Promise<Record<string, string>> {
  return invoke<Record<string, string>>("get_terminal_cwds", { ids });
}

export type AgentKind = "claude" | "codex" | "opencode";

export interface AgentSession {
  kind: AgentKind;
  /** Agent's own session id; null → resume the last session in the terminal's cwd. */
  sessionId: string | null;
}

/** Agent CLI running in each terminal's shell; ids without one are omitted. */
export function getTerminalAgents(
  ids: string[],
): Promise<Record<string, AgentSession>> {
  return invoke<Record<string, AgentSession>>("get_terminal_agents", { ids });
}

export function killTerminal(id: string): Promise<void> {
  return invoke("kill_terminal", { id });
}

export function onTerminalExit(
  id: string,
  cb: (exitCode: number | null) => void,
): Promise<UnlistenFn> {
  return listen<number | null>(`terminal-exit:${id}`, (event) => {
    cb(event.payload);
  });
}
