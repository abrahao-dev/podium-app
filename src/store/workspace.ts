import { create } from "zustand";
import {
  addEdge,
  applyNodeChanges,
  type Connection,
  type Edge,
  type EdgeChange,
  applyEdgeChanges,
  MarkerType,
  type Node,
  type NodeChange,
  type XYPosition,
} from "@xyflow/react";

export interface TerminalNodeData extends Record<string, unknown> {
  label: string;
  command?: string;
  cwd?: string;
  /** True only for nodes created this session — restored nodes require a
   *  click to spawn (never auto-run agents on load). */
  autoStart: boolean;
  /** Opt-in: connected agents may write handoff prompts into this terminal. */
  allowIncoming?: boolean;
}

export interface NoteNodeData extends Record<string, unknown> {
  text: string;
}

export interface BrowserNodeData extends Record<string, unknown> {
  url: string;
  label?: string;
}

export type TerminalFlowNode = Node<TerminalNodeData, "terminal">;
export type NoteFlowNode = Node<NoteNodeData, "note">;
export type BrowserFlowNode = Node<BrowserNodeData, "browser">;
export type PodiumNode = TerminalFlowNode | NoteFlowNode | BrowserFlowNode;

interface WorkspaceState {
  nodes: PodiumNode[];
  edges: Edge[];
  hydrated: boolean;
  onNodesChange: (changes: NodeChange<PodiumNode>[]) => void;
  onEdgesChange: (changes: EdgeChange[]) => void;
  onConnect: (connection: Connection) => void;
  removeEdge: (id: string) => void;
  addTerminal: (position: XYPosition, command?: string, label?: string, role?: string, cwd?: string) => void;
  addNote: (position: XYPosition) => void;
  addBrowser: (position: XYPosition, url?: string) => void;
  removeNode: (id: string) => void;
  updateNodeData: (
    id: string,
    data: Partial<TerminalNodeData & NoteNodeData & BrowserNodeData>,
  ) => void;
  setWorkspace: (nodes: PodiumNode[], edges: Edge[]) => void;
}

function nextLabel(nodes: PodiumNode[]): string {
  const used = new Set(
    nodes
      .filter((n) => n.type === "terminal")
      .map((n) => (n.data as TerminalNodeData).label),
  );
  let i = 1;
  while (used.has(`Terminal ${i}`)) i += 1;
  return `Terminal ${i}`;
}

export const useWorkspaceStore = create<WorkspaceState>((set) => ({
  nodes: [],
  edges: [],
  hydrated: false,

  onNodesChange: (changes) =>
    set((state) => ({ nodes: applyNodeChanges(changes, state.nodes) })),

  onEdgesChange: (changes) =>
    set((state) => ({ edges: applyEdgeChanges(changes, state.edges) })),

  onConnect: (connection) =>
    set((state) => {
      if (connection.source === connection.target) return state;
      return {
        edges: addEdge(
          {
            ...connection,
            animated: true,
            markerEnd: { type: MarkerType.ArrowClosed },
          },
          state.edges,
        ),
      };
    }),

  removeEdge: (id) =>
    set((state) => ({ edges: state.edges.filter((e) => e.id !== id) })),

  addTerminal: (position, command, label, role, cwd) =>
    set((state) => ({
      nodes: [
        ...state.nodes,
        {
          id: crypto.randomUUID(),
          type: "terminal",
          position,
          data: {
            label: label ?? nextLabel(state.nodes),
            autoStart: true,
            command,
            role,
            cwd,
          },
          dragHandle: ".terminal-node__header",
          width: 640,
          height: 400,
        },
      ],
    })),

  addNote: (position) =>
    set((state) => ({
      nodes: [
        ...state.nodes,
        {
          id: crypto.randomUUID(),
          type: "note",
          position,
          data: { text: "" },
          dragHandle: ".note-node__header",
          width: 300,
          height: 220,
        },
      ],
    })),

  addBrowser: (position, url) =>
    set((state) => ({
      nodes: [
        ...state.nodes,
        {
          id: crypto.randomUUID(),
          type: "browser",
          position,
          data: { url: url ?? "about:blank" },
          dragHandle: ".browser-node__header",
          width: 600,
          height: 450,
        },
      ],
    })),

  removeNode: (id) =>
    set((state) => ({
      nodes: state.nodes.filter((n) => n.id !== id),
      edges: state.edges.filter((e) => e.source !== id && e.target !== id),
    })),

  updateNodeData: (id, data) =>
    set((state) => ({
      nodes: state.nodes.map((n) =>
        n.id === id ? ({ ...n, data: { ...n.data, ...data } } as PodiumNode) : n,
      ),
    })),

  setWorkspace: (nodes, edges) => set({ nodes, edges, hydrated: true }),
}));
