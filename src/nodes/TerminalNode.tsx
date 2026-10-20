import { memo, useEffect, useMemo, useRef, useState } from "react";
import {
  Handle,
  NodeResizer,
  Position,
  type Edge,
  type NodeProps,
} from "@xyflow/react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import type {
  NoteNodeData,
  PodiumNode,
  TerminalFlowNode,
} from "../store/workspace";
import { useWorkspaceStore } from "../store/workspace";
import { useSettingsStore } from "../store/settings";
import * as pty from "../lib/pty";
import { resumeCommand } from "../lib/agents";
import { enqueueLaunch } from "../lib/launchQueue";
import { registerRenderer } from "../lib/webglBudget";
import {
  reportSpawned,
  useAgentStates,
  useTerminalMemory,
  type AgentState,
} from "../lib/bridge";

type TerminalStatus = "idle" | "starting" | "running" | "exited";

/** Agent states worth a badge in the header. */
const AGENT_BADGES: Partial<Record<AgentState, string>> = {
  working: "working",
  waitingInput: "needs input",
};

const TERMINAL_THEME = {
  background: "#16181d",
  foreground: "#d4d7dd",
  cursor: "#7aa2f7",
  selectionBackground: "#33467c",
};

const CAPTURE_LINES = 20;
/** xterm write-queue backpressure: above HIGH bytes not yet parsed, the
 *  backend holds this terminal's output; below LOW it resumes (restoring the
 *  view from its screen mirror). The PTY itself is never paused. */
const OUTPUT_HIGH_WATER = 4 * 1024 * 1024;
const OUTPUT_LOW_WATER = 512 * 1024;
/** Gives the shell time to run the submitted line and redraw its prompt. */
const CWD_SYNC_DELAY_MS = 500;
/** A line that leaves no identified agent session is checked once more after
 *  this, as agents boot and register their session after the Enter. */
const AGENT_RESYNC_DELAY_MS = 4000;

const IS_MAC = navigator.userAgent.includes("Mac");
const IS_WINDOWS = navigator.userAgent.includes("Windows");
/** Ctrl+V as a raw control byte (what the PTY got before paste was wired). */
const CTRL_V = "\x16";

/**
 * Ctrl+V (Windows) and Ctrl+Shift+V (Windows/Linux) paste from the system
 * clipboard. macOS already pastes with Cmd+V, so Ctrl+V still reaches the PTY.
 */
function isPasteShortcut(e: KeyboardEvent): boolean {
  if (!useSettingsStore.getState().settings.shortcuts.pasteWithCtrlV) {
    return false;
  }
  if (IS_MAC || !e.ctrlKey || e.altKey || e.metaKey || e.code !== "KeyV") {
    return false;
  }
  return e.shiftKey || IS_WINDOWS;
}

/** Ctrl+Enter / Shift+Enter insert a line break in agent prompts. */
function isNewlineShortcut(e: KeyboardEvent): boolean {
  return (
    useSettingsStore.getState().settings.shortcuts.newlineWithModifiedEnter &&
    e.key === "Enter" && (e.ctrlKey || e.shiftKey) && !e.altKey && !e.metaKey
  );
}

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

const KEY_FIELD = "\u001f";
const KEY_RECORD = "\u001e";

/** Outgoing targets of `id` encoded as a primitive, so a zustand selector
 *  can compare it by value. */
function outgoingTargetsKey(
  nodes: PodiumNode[],
  edges: Edge[],
  id: string,
): string {
  const records: string[] = [];
  for (const edge of edges) {
    if (edge.source !== id) continue;
    const node = nodes.find((n) => n.id === edge.target);
    if (node?.type === "terminal") {
      records.push(
        [
          "t",
          node.id,
          node.data.label,
          node.data.allowIncoming === true ? "1" : "0",
        ].join(KEY_FIELD),
      );
    } else if (node?.type === "note") {
      records.push(["n", node.id].join(KEY_FIELD));
    }
  }
  return records.join(KEY_RECORD);
}

function parseTargetsKey(key: string): {
  terminalTargets: ConnectedTerminal[];
  noteTargets: string[];
} {
  const terminalTargets: ConnectedTerminal[] = [];
  const noteTargets: string[] = [];
  if (key === "") return { terminalTargets, noteTargets };
  for (const record of key.split(KEY_RECORD)) {
    const [kind, nodeId, label, allow] = record.split(KEY_FIELD);
    if (kind === "t") {
      terminalTargets.push({ id: nodeId, label, allowIncoming: allow === "1" });
    } else {
      noteTargets.push(nodeId);
    }
  }
  return { terminalTargets, noteTargets };
}

function TerminalNodeComponent({
  id,
  data,
  selected,
}: NodeProps<TerminalFlowNode>) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  /** Command for the next spawn only (e.g. resuming an agent session). */
  const launchCommandRef = useRef<string | undefined>(undefined);
  const fontSize = useSettingsStore((s) => s.settings.terminal.fontSize);
  const restoreAgentSessions = useSettingsStore(
    (s) => s.settings.sessions.restoreAgentSessions,
  );
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
  const agentState = useAgentStates((s) => s[id]);
  const memoryMb = useTerminalMemory((s) => s[id]);
  const badge =
    status === "running" && agentState ? AGENT_BADGES[agentState] : undefined;

  // Outgoing connections drive the footer actions. The selector returns a
  // string so this node only re-renders when its own targets change, not on
  // every drag frame of any node on the canvas.
  const targetsKey = useWorkspaceStore((s) =>
    outgoingTargetsKey(s.nodes, s.edges, id),
  );
  const { terminalTargets, noteTargets } = useMemo(
    () => parseTargetsKey(targetsKey),
    [targetsKey],
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
      fontSize: useSettingsStore.getState().settings.terminal.fontSize,
      fontFamily: "'SF Mono', Menlo, Consolas, 'Courier New', monospace",
      scrollback: 5000,
      theme: TERMINAL_THEME,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);

    // WebGL only while on screen and within the context budget; otherwise
    // the DOM renderer (or nothing, off screen). Output keeps flowing either way.
    let webgl: WebglAddon | null = null;
    const renderer = registerRenderer(
      id,
      () => {
        try {
          const addon = new WebglAddon();
          // A lost context leaves a frozen frame; disposing the addon falls
          // back to the DOM renderer.
          addon.onContextLoss(() => {
            addon.dispose();
            if (webgl === addon) webgl = null;
            renderer.lost();
          });
          term.loadAddon(addon);
          webgl = addon;
          return true;
        } catch {
          return false;
        }
      },
      () => {
        webgl?.dispose();
        webgl = null;
      },
    );
    const visibility = new IntersectionObserver(
      (records) => {
        const record = records[records.length - 1];
        if (record) renderer.setVisible(record.isIntersecting);
      },
      { rootMargin: "200px" },
    );
    visibility.observe(container);
    const onFocus = () => renderer.touch();
    container.addEventListener("focusin", onFocus);

    fit.fit();
    termRef.current = term;
    fitRef.current = fit;

    let pendingBytes = 0;
    let outputPaused = false;
    const onOutput = (bytes: Uint8Array) => {
      pendingBytes += bytes.length;
      term.write(bytes, () => {
        pendingBytes -= bytes.length;
        if (outputPaused && pendingBytes < OUTPUT_LOW_WATER) {
          outputPaused = false;
          void pty.setTerminalOutputPaused(id, false).catch(() => {});
        }
      });
      if (!outputPaused && pendingBytes > OUTPUT_HIGH_WATER) {
        outputPaused = true;
        void pty.setTerminalOutputPaused(id, true).catch(() => {});
      }
    };

    // Exit listener registered before spawn so an early exit is not missed.
    const unlistenExit = pty.onTerminalExit(id, (code) => {
      setStatus("exited");
      term.writeln(`\r\n[process exited${code === null ? "" : ` (${code})`}]`);
    });

    // A resume command applies to this spawn only; restarts run the
    // terminal's own command again.
    const command = launchCommandRef.current ?? dataRef.current.command;
    launchCommandRef.current = undefined;
    pty
      .createTerminal({
        id,
        cwd: dataRef.current.cwd,
        command,
        cols: term.cols,
        rows: term.rows,
      }, onOutput)
      .then(() => {
        setStatus("running");
        reportSpawned(id);
      })
      .catch((err: unknown) => {
        setStatus("exited");
        term.writeln(`Failed to start terminal: ${String(err)}`);
        reportSpawned(id, String(err));
      });

    // The directory and the running agent can only change after a submitted
    // line (`cd`, `claude`, `/exit`...), so they are read once shortly after
    // Enter instead of polled.
    let syncTimer: ReturnType<typeof setTimeout> | undefined;
    const syncState = (allowResync: boolean) => {
      void pty
        .getTerminalCwds([id])
        .then((cwds) => useWorkspaceStore.getState().applyTerminalCwds(cwds))
        .catch(() => {});
      if (!useSettingsStore.getState().settings.sessions.restoreAgentSessions) {
        return;
      }
      void pty
        .getTerminalAgents([id])
        .then((agents) => {
          useWorkspaceStore.getState().applyTerminalAgents([id], agents);
          // The agent may still be booting, or not yet have registered its
          // session: look once more a little later.
          if (allowResync && (agents[id]?.sessionId ?? null) === null) {
            syncTimer = setTimeout(
              () => syncState(false),
              AGENT_RESYNC_DELAY_MS,
            );
          }
        })
        .catch(() => {});
    };

    term.attachCustomKeyEventHandler((e) => {
      // Skipping xterm lets the webview fire a native `paste` on its textarea;
      // xterm then sends the text (bracketed when the app asked for it).
      if (isPasteShortcut(e)) return false;
      if (isNewlineShortcut(e)) {
        // xterm sends a bare \r for any Enter, which submits. A line feed is
        // what Claude Code / Codex read as "new line" (same as Ctrl+J).
        if (e.type === "keydown") {
          e.preventDefault();
          void pty.writeToTerminal(id, "\n").catch(() => {});
        }
        return false;
      }
      return true;
    });

    // Clipboard without text (e.g. a screenshot): forward the raw Ctrl+V so
    // agents that read the clipboard themselves can still attach it.
    const onPaste = (e: ClipboardEvent) => {
      if ((e.clipboardData?.getData("text/plain") ?? "") !== "") return;
      e.preventDefault();
      e.stopPropagation();
      void pty.writeToTerminal(id, CTRL_V).catch(() => {});
    };
    container.addEventListener("paste", onPaste, true);

    const dataSub = term.onData((input) => {
      renderer.touch();
      // Ignore rejections: the PTY may have already exited.
      void pty.writeToTerminal(id, input).catch(() => {});
      if (input.includes("\r")) {
        clearTimeout(syncTimer);
        syncTimer = setTimeout(() => syncState(true), CWD_SYNC_DELAY_MS);
      }
    });
    const resizeSub = term.onResize(({ cols, rows }) => {
      void pty.resizeTerminal(id, cols, rows).catch(() => {});
    });
    const observer = new ResizeObserver(() => fit.fit());
    observer.observe(container);

    return () => {
      clearTimeout(syncTimer);
      container.removeEventListener("paste", onPaste, true);
      container.removeEventListener("focusin", onFocus);
      observer.disconnect();
      visibility.disconnect();
      renderer.dispose();
      dataSub.dispose();
      resizeSub.dispose();
      void unlistenExit.then((fn) => fn());
      termRef.current = null;
      fitRef.current = null;
      term.dispose();
      void pty.killTerminal(id).catch(() => {});
    };
  }, [id, spawnKey]);

  useEffect(() => {
    const term = termRef.current;
    if (!term || term.options.fontSize === fontSize) return;
    term.options.fontSize = fontSize;
    fitRef.current?.fit();
  }, [fontSize]);

  // Clear any pending flash timer on unmount so it never fires setState late.
  useEffect(
    () => () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    },
    [],
  );

  // Only the first start of a restored terminal resumes its agent session.
  const resumeSession =
    spawnKey === 0 && restoreAgentSessions ? data.agentSession : undefined;
  const startedRef = useRef(data.autoStart);

  const start = () => {
    startedRef.current = true;
    if (resumeSession) launchCommandRef.current = resumeCommand(resumeSession);
    setStatus("starting");
    setSpawnKey((k) => k + 1);
  };
  const startRef = useRef(start);
  startRef.current = start;

  // Automatic resume: restored terminals go through the launch queue once
  // settings are known, instead of all spawning on the first frame.
  useEffect(() => {
    if (startedRef.current) return;
    let cancelLaunch: (() => void) | undefined;
    const schedule = () => {
      const { sessions } = useSettingsStore.getState().settings;
      if (sessions.resumeMode !== "auto" || !sessions.restoreAgentSessions) {
        return;
      }
      cancelLaunch = enqueueLaunch(dataRef.current.agentSession !== undefined, () => {
        if (!startedRef.current) startRef.current();
      });
    };
    if (useSettingsStore.getState().loaded) {
      schedule();
      return () => cancelLaunch?.();
    }
    const unsubscribe = useSettingsStore.subscribe((state) => {
      if (!state.loaded) return;
      unsubscribe();
      schedule();
    });
    return () => {
      unsubscribe();
      cancelLaunch?.();
    };
  }, []);

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
    <div
      className={`terminal-node${data.parent ? " terminal-node--recruit" : ""}${
        data.orchestrator ? " terminal-node--orchestrator" : ""
      }`}
    >
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
      <div
        className="terminal-node__header"
        onPointerDown={() => {
          // Hand keyboard focus from the shell to the canvas so node
          // shortcuts (Ctrl+C / Ctrl+V) apply instead of reaching the PTY.
          const active = document.activeElement;
          if (active instanceof HTMLElement && active.closest(".xterm")) {
            active.blur();
          }
        }}
      >
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
        {badge && (
          <span
            className={`terminal-node__agent terminal-node__agent--${agentState}`}
          >
            {badge}
          </span>
        )}
        {status === "running" && memoryMb !== undefined && memoryMb >= 100 && (
          <span className="terminal-node__memory" title="Memory of this terminal and its agent">
            {memoryMb >= 1024 ? `${(memoryMb / 1024).toFixed(1)} GB` : `${memoryMb} MB`}
          </span>
        )}
        <span className="terminal-node__command">
          {(restoreAgentSessions ? data.agentSession?.kind : undefined) ??
            data.command ??
            "shell"}
        </span>
        <button
          className={`terminal-node__crown nodrag ${
            data.orchestrator ? "terminal-node__crown--on" : ""
          }`}
          title={data.orchestrator ? "Orchestrator: on" : "Orchestrator: off"}
          aria-pressed={data.orchestrator === true}
          onClick={(e) => {
            e.stopPropagation();
            updateNodeData(id, { orchestrator: !data.orchestrator });
          }}
        >
          ♛
        </button>
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
              {status !== "idle"
                ? "↻ Restart"
                : resumeSession
                  ? `▶ Resume ${resumeSession.kind}`
                  : `▶ Start ${data.command ?? "shell"}`}
            </button>
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
