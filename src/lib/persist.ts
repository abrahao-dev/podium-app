import { invoke } from "@tauri-apps/api/core";
import type { Edge } from "@xyflow/react";
import {
  nodeRect,
  type NoteNodeData,
  type PodiumNode,
  type TerminalNodeData,
} from "../store/workspace";
import type { AgentKind, AgentSession } from "./pty";

const WORKSPACE_NAME = "default";

interface SavedNode {
  id: string;
  type: "terminal" | "note";
  x: number;
  y: number;
  width: number;
  height: number;
  label?: string;
  command?: string;
  cwd?: string;
  allowIncoming?: boolean;
  agentSession?: AgentSession;
  text?: string;
}

const AGENT_KINDS: readonly AgentKind[] = ["claude", "codex", "opencode"];

/** The file is user-editable; anything malformed is dropped. */
function parseAgentSession(raw: unknown): AgentSession | undefined {
  if (typeof raw !== "object" || raw === null) return undefined;
  const { kind, sessionId } = raw as Record<string, unknown>;
  if (!AGENT_KINDS.includes(kind as AgentKind)) return undefined;
  return {
    kind: kind as AgentKind,
    sessionId: typeof sessionId === "string" ? sessionId : null,
  };
}

interface SavedWorkspace {
  version: 1;
  nodes: SavedNode[];
  edges: Edge[];
}

function toSaved(node: PodiumNode): SavedNode {
  const rect = nodeRect(node);
  const base = {
    id: node.id,
    type: node.type ?? "terminal",
    x: rect.x,
    y: rect.y,
    width: rect.width,
    height: rect.height,
  };
  if (node.type === "note") {
    return { ...base, text: (node.data as NoteNodeData).text };
  }
  const data = node.data as TerminalNodeData;
  return {
    ...base,
    label: data.label,
    command: data.command,
    cwd: data.cwd,
    allowIncoming: data.allowIncoming,
    agentSession: data.agentSession,
  };
}

function fromSaved(saved: SavedNode): PodiumNode {
  // Whole pixels: fractional positions blur terminal text at 100% zoom.
  const common = {
    id: saved.id,
    position: { x: Math.round(saved.x), y: Math.round(saved.y) },
    width: Math.round(saved.width),
    height: Math.round(saved.height),
  };
  if (saved.type === "note") {
    return {
      ...common,
      type: "note",
      dragHandle: ".note-node__header",
      data: { text: saved.text ?? "" },
    };
  }
  return {
    ...common,
    type: "terminal",
    dragHandle: ".terminal-node__header",
    data: {
      label: saved.label ?? "Terminal",
      command: saved.command,
      cwd: saved.cwd,
      allowIncoming: saved.allowIncoming,
      agentSession: parseAgentSession(saved.agentSession),
      // Restored terminals start from their overlay or the launch queue.
      autoStart: false,
    },
  };
}

export async function saveWorkspace(
  nodes: PodiumNode[],
  edges: Edge[],
): Promise<void> {
  const payload: SavedWorkspace = {
    version: 1,
    nodes: nodes.map(toSaved),
    edges,
  };
  await invoke("save_workspace", {
    name: WORKSPACE_NAME,
    data: JSON.stringify(payload, null, 2),
  });
}

export async function loadWorkspace(): Promise<{
  nodes: PodiumNode[];
  edges: Edge[];
} | null> {
  const raw = await invoke<string | null>("load_workspace", {
    name: WORKSPACE_NAME,
  });
  if (raw === null) return null;
  try {
    const parsed = JSON.parse(raw) as SavedWorkspace;
    return { nodes: parsed.nodes.map(fromSaved), edges: parsed.edges ?? [] };
  } catch {
    return null;
  }
}
