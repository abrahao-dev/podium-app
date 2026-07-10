import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface CreateTerminalOptions {
  id: string;
  cwd?: string;
  command?: string;
  cols: number;
  rows: number;
}

export function createTerminal(opts: CreateTerminalOptions): Promise<void> {
  return invoke("create_terminal", {
    id: opts.id,
    cwd: opts.cwd ?? null,
    command: opts.command ?? null,
    cols: opts.cols,
    rows: opts.rows,
  });
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

export function killTerminal(id: string): Promise<void> {
  return invoke("kill_terminal", { id });
}

/** Raw PTY bytes; pass straight to xterm so its decoder handles split UTF-8. */
export function onTerminalOutput(
  id: string,
  cb: (data: Uint8Array) => void,
): Promise<UnlistenFn> {
  return listen<number[]>(`terminal-output:${id}`, (event) => {
    cb(new Uint8Array(event.payload));
  });
}

export function onTerminalExit(
  id: string,
  cb: (exitCode: number | null) => void,
): Promise<UnlistenFn> {
  return listen<number | null>(`terminal-exit:${id}`, (event) => {
    cb(event.payload);
  });
}
