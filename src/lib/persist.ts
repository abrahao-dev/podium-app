import { invoke } from "@tauri-apps/api/core";
import type { Edge } from "@xyflow/react";
import type {
  BrowserNodeData,
  NoteNodeData,
  PodiumNode,
  TerminalNodeData,
} from "../store/workspace";

const WORKSPACE_NAME = "default";

interface SavedNode {
  id: string;
  type: "terminal" | "note" | "browser";
  x: number;
  y: number;
  width: number;
  height: number;
  label?: string;
  command?: string;
  cwd?: string;
  allowIncoming?: boolean;
  text?: string;
  url?: string;
}

interface SavedWorkspace {
  version: 1;
  nodes: SavedNode[];
  edges: Edge[];
}

function toSaved(node: PodiumNode): SavedNode {
  const base = {
    id: node.id,
    type: node.type ?? "terminal",
    x: node.position.x,
    y: node.position.y,
    width: node.width ?? node.measured?.width ?? 640,
    height: node.height ?? node.measured?.height ?? 400,
  };
  if (node.type === "note") {
    return { ...base, text: (node.data as NoteNodeData).text };
  }
  if (node.type === "browser") {
    return {
      ...base,
      url: (node.data as BrowserNodeData).url,
      label: (node.data as BrowserNodeData).label,
    };
  }
  const data = node.data as TerminalNodeData;
  return {
    ...base,
    label: data.label,
    command: data.command,
    cwd: data.cwd,
    allowIncoming: data.allowIncoming,
  };
}

function fromSaved(saved: SavedNode): PodiumNode {
  const common = {
    id: saved.id,
    position: { x: saved.x, y: saved.y },
    width: saved.width,
    height: saved.height,
  };
  if (saved.type === "note") {
    return {
      ...common,
      type: "note",
      dragHandle: ".note-node__header",
      data: { text: saved.text ?? "" },
    };
  }
  if (saved.type === "browser") {
    return {
      ...common,
      type: "browser",
      dragHandle: ".browser-node__header",
      data: { url: saved.url ?? "about:blank", label: saved.label },
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
      // Restored terminals must never auto-run their command.
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
