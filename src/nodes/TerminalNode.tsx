import { memo, useEffect, useMemo, useRef, useState } from "react";
import {
  Handle,
  NodeResizer,
  Position,
  type NodeProps,
} from "@xyflow/react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import type {
  NoteNodeData,
  TerminalFlowNode,
  TerminalNodeData,
} from "../store/workspace";
import { useWorkspaceStore } from "../store/workspace";
import * as pty from "../lib/pty";

type TerminalStatus = "idle" | "starting" | "running" | "exited";

const TERMINAL_THEME = {
  background: "#16181d",
  foreground: "#d4d7dd",
  cursor: "#7aa2f7",
  selectionBackground: "#33467c",
};

const CAPTURE_LINES = 20;

/** Last non-empty lines of the terminal buffer (scrollback included). */
function lastLines(term: Terminal, max: number): string {
  const buffer = term.buffer.active;
  const lines: string[] = [];
  let end = buffer.length - 1;
  while (end >= 0) {
    const line = buffer.getLine(end)?.translateToString(true) ?? "";
    if (line.trim() !== "") break;
    end -= 1;
  }
  for (let i = end; i >= 0 && lines.length < max; i -= 1) {
    lines.unshift(buffer.getLine(i)?.translateToString(true) ?? "");
  }
  return lines.join("\n").trimEnd();
}

interface ConnectedTerminal {
  id: string;
  label: string;
  allowIncoming: boolean;
}

function TerminalNodeComponent({
  id,
  data,
  selected,
}: NodeProps<TerminalFlowNode>) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const [status, setStatus] = useState<TerminalStatus>(
    data.autoStart ? "starting" : "idle",
  );
  const [spawnKey, setSpawnKey] = useState(data.autoStart ? 1 : 0);
  const [renaming, setRenaming] = useState(false);
  const [prompt, setPrompt] = useState("");
  const [flash, setFlash] = useState<string | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout>>();
  const updateNodeData = useWorkspaceStore((s) => s.updateNodeData);
  const removeNode = useWorkspaceStore((s) => s.removeNode);

  // Outgoing connections drive the footer actions. Derived via useMemo —
  // zustand selectors must return referentially stable snapshots.
  const edges = useWorkspaceStore((s) => s.edges);
  const nodes = useWorkspaceStore((s) => s.nodes);
  const terminalTargets = useMemo<ConnectedTerminal[]>(
    () =>
      edges
        .filter((e) => e.source === id)
        .map((e) => nodes.find((n) => n.id === e.target))
        .filter((n): n is TerminalFlowNode => n?.type === "terminal")
        .map((n) => ({
          id: n.id,
          label: (n.data as TerminalNodeData).label,
          allowIncoming: (n.data as TerminalNodeData).allowIncoming === true,
        })),
    [edges, nodes, id],
  );
  const noteTargets = useMemo<string[]>(
    () =>
      edges
        .filter((e) => e.source === id)
        .flatMap((e) => {
          const n = nodes.find((node) => node.id === e.target);
          return n?.type === "note" ? [n.id] : [];
        }),
    [edges, nodes, id],
  );

  // Spawn-time options read via ref so renames/edits don't respawn the PTY.
  const dataRef = useRef(data);
  dataRef.current = data;

  useEffect(() => {
    if (spawnKey === 0) return;
    const container = containerRef.current;
    if (!container) return;

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      fontFamily: "'SF Mono', Menlo, Consolas, 'Courier New', monospace",
      scrollback: 5000,
      theme: TERMINAL_THEME,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    try {
      term.loadAddon(new WebglAddon());
    } catch {
      // WebGL unavailable — canvas renderer fallback is fine.
    }
    fit.fit();
    termRef.current = term;

    // Listeners registered before spawn so no early output is dropped.
    const unlistenOutput = pty.onTerminalOutput(id, (bytes) => {
      term.write(bytes);
    });
    const unlistenExit = pty.onTerminalExit(id, (code) => {
      setStatus("exited");
      term.writeln(`\r\n[process exited${code === null ? "" : ` (${code})`}]`);
    });

    pty
      .createTerminal({
        id,
        cwd: dataRef.current.cwd,
        command: dataRef.current.command,
        cols: term.cols,
        rows: term.rows,
      })
      .then(() => setStatus("running"))
      .catch((err: unknown) => {
        setStatus("exited");
        term.writeln(`Failed to start terminal: ${String(err)}`);
      });

    const dataSub = term.onData((input) => {
      // Ignore rejections: the PTY may have already exited.
      void pty.writeToTerminal(id, input).catch(() => {});
    });
    const resizeSub = term.onResize(({ cols, rows }) => {
      void pty.resizeTerminal(id, cols, rows).catch(() => {});
    });
    const observer = new ResizeObserver(() => fit.fit());
    observer.observe(container);

    return () => {
      observer.disconnect();
      dataSub.dispose();
      resizeSub.dispose();
      void unlistenOutput.then((fn) => fn());
      void unlistenExit.then((fn) => fn());
      termRef.current = null;
      term.dispose();
      void pty.killTerminal(id).catch(() => {});
    };
  }, [id, spawnKey]);

  // Clear any pending flash timer on unmount so it never fires setState late.
  useEffect(
    () => () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    },
    [],
  );

  const start = () => {
    setStatus("starting");
    setSpawnKey((k) => k + 1);
  };

  const commitLabel = (value: string) => {
    updateNodeData(id, {
      label: value.trim() === "" ? data.label : value.trim(),
    });
    setRenaming(false);
  };

  const showFlash = (message: string) => {
    setFlash(message);
    if (flashTimer.current) clearTimeout(flashTimer.current);
    flashTimer.current = setTimeout(() => setFlash(null), 2000);
  };

  /** Write to each target, reporting which succeeded vs. weren't running. */
  const writeToTargets = async (
    targets: ConnectedTerminal[],
    payload: string,
    verb: string,
  ): Promise<boolean> => {
    const results = await Promise.allSettled(
      targets.map((t) => pty.writeToTerminal(t.id, payload)),
    );
    const failed = targets.filter((_, i) => results[i].status === "rejected");
    const sent = targets.filter((_, i) => results[i].status === "fulfilled");
    if (failed.length > 0) {
      showFlash(`not running: ${failed.map((t) => t.label).join(", ")}`);
    } else {
      showFlash(`${verb} → ${sent.map((t) => t.label).join(", ")}`);
    }
    return sent.length > 0;
  };

  const sendPrompt = () => {
    const text = prompt.trim();
    if (text === "" || terminalTargets.length === 0) return;
    // \r submits — never \n.
    void writeToTargets(terminalTargets, `${text}\r`, "sent").then((ok) => {
      if (ok) setPrompt("");
    });
  };

  const delegate = () => {
    const term = termRef.current;
    if (!term) return;
    const output = lastLines(term, CAPTURE_LINES);
    if (output === "") return;
    const allowed = terminalTargets.filter((t) => t.allowIncoming);
    if (allowed.length === 0) return;
    const handoff = `You received the following handoff from another agent (${data.label}):\n${output}\n`;
    void writeToTargets(allowed, `${handoff}\r`, "delegated");
  };

  const captureToNote = () => {
    const term = termRef.current;
    if (!term || noteTargets.length === 0) return;
    const output = lastLines(term, CAPTURE_LINES);
    if (output === "") return;
    const stamp = new Date().toLocaleString();
    const { nodes, updateNodeData: update } = useWorkspaceStore.getState();
    for (const noteId of noteTargets) {
      const note = nodes.find((n) => n.id === noteId);
      if (!note) continue;
      const prev = (note.data as NoteNodeData).text;
      const block = `### ${data.label} — ${stamp}\n\`\`\`\n${output}\n\`\`\``;
      update(noteId, { text: prev === "" ? block : `${prev}\n\n${block}` });
    }
    showFlash("captured → note");
  };

  const delegateTargets = terminalTargets.filter((t) => t.allowIncoming);

  return (
    <div className="terminal-node">
      <NodeResizer
        isVisible={selected}
        minWidth={360}
        minHeight={220}
        lineClassName="terminal-node__resize-line"
        handleClassName="terminal-node__resize-handle"
      />
      <Handle
        type="target"
        position={Position.Left}
        className="podium-handle"
      />
      <Handle
        type="source"
        position={Position.Right}
        className="podium-handle"
      />
      <div className="terminal-node__header">
        <span
          className={`terminal-node__status terminal-node__status--${status}`}
        />
        {renaming ? (
          <input
            className="terminal-node__label-input nodrag"
            defaultValue={data.label}
            autoFocus
            onBlur={(e) => commitLabel(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitLabel(e.currentTarget.value);
              if (e.key === "Escape") setRenaming(false);
            }}
          />
        ) : (
          <span
            className="terminal-node__label"
            title="Double-click to rename"
            onDoubleClick={() => setRenaming(true)}
          >
            {data.label}
          </span>
        )}
        <span className="terminal-node__command">
          {data.command ?? "shell"}
        </span>
        <button
          className={`terminal-node__lock nodrag ${
            data.allowIncoming ? "terminal-node__lock--open" : ""
          }`}
          title={
            data.allowIncoming
              ? "Connected agents may prompt this terminal (click to disallow)"
              : "Connected agents may NOT prompt this terminal (click to allow)"
          }
          onClick={(e) => {
            e.stopPropagation();
            updateNodeData(id, { allowIncoming: !data.allowIncoming });
          }}
        >
          {data.allowIncoming ? "⇥" : "⊘"}
        </button>
        <button
          className="node-close"
          title="Close terminal"
          onClick={(e) => {
            e.stopPropagation();
            removeNode(id);
          }}
        >
          ×
        </button>
      </div>
      <div className="terminal-node__body-wrap">
        <div
          ref={containerRef}
          className="terminal-node__body nodrag nowheel"
        />
        {(status === "idle" || (status === "exited" && spawnKey > 0)) && (
          <div className="terminal-node__overlay nodrag">
            <button className="terminal-node__start" onClick={start}>
              {status === "idle"
                ? `▶ Start ${data.command ?? "shell"}`
                : "↻ Restart"}
            </button>
            {status === "idle" && (
              <p className="terminal-node__overlay-hint">
                Restored terminals never auto-run
              </p>
            )}
          </div>
        )}
        {flash && <div className="terminal-node__flash">{flash}</div>}
      </div>
      {(terminalTargets.length > 0 || noteTargets.length > 0) && (
        <div className="terminal-node__footer nodrag">
          {terminalTargets.length > 0 && (
            <>
              <input
                className="terminal-node__prompt"
                placeholder={`Prompt → ${terminalTargets
                  .map((t) => t.label)
                  .join(", ")}`}
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") sendPrompt();
                }}
              />
              <button
                className="terminal-node__action"
                title="Send prompt to connected terminals"
                onClick={sendPrompt}
                disabled={prompt.trim() === ""}
              >
                Send
              </button>
              <button
                className="terminal-node__action"
                title={
                  delegateTargets.length > 0
                    ? `Hand last output to ${delegateTargets
                        .map((t) => t.label)
                        .join(", ")}`
                    : "Target terminal must enable incoming prompts (⊘ → ⇥)"
                }
                onClick={delegate}
                disabled={delegateTargets.length === 0 || status !== "running"}
              >
                Delegate
              </button>
            </>
          )}
          {noteTargets.length > 0 && (
            <button
              className="terminal-node__action"
              title="Append last output to connected note"
              onClick={captureToNote}
              disabled={status === "idle"}
            >
              Capture → Note
            </button>
          )}
        </div>
      )}
    </div>
  );
}

export const TerminalNode = memo(TerminalNodeComponent);
