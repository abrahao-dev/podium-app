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
import { findFreeSpot, type Rect } from "../canvas/placement";

export interface TerminalNodeData extends Record<string, unknown> {
  label: string;
  command?: string;
  cwd?: string;
  /** True only for nodes created this session — restored nodes require a
   *  click to respawn (never auto-run agents on load). */
  autoStart: boolean;
  /** Opt-in: connected agents may write handoff prompts into this terminal. */
  allowIncoming?: boolean;
}

export interface NoteNodeData extends Record<string, unknown> {
  text: string;
}

export type TerminalFlowNode = Node<TerminalNodeData, "terminal">;
export type NoteFlowNode = Node<NoteNodeData, "note">;
export type PodiumNode = TerminalFlowNode | NoteFlowNode;

interface WorkspaceState {
  nodes: PodiumNode[];
  edges: Edge[];
  hydrated: boolean;
  onNodesChange: (changes: NodeChange<PodiumNode>[]) => void;
  onEdgesChange: (changes: EdgeChange[]) => void;
  onConnect: (connection: Connection) => void;
  removeEdge: (id: string) => void;
  addTerminal: (position: XYPosition, command?: string, label?: string) => void;
  addNote: (position: XYPosition) => void;
  removeNode: (id: string) => void;
  updateNodeData: (
    id: string,
    data: Partial<TerminalNodeData & NoteNodeData>,
  ) => void;
  setWorkspace: (nodes: PodiumNode[], edges: Edge[]) => void;
  /** Applies shell directories by terminal id; no-op when nothing changed. */
  applyTerminalCwds: (cwds: Record<string, string>) => void;
  /** Spawns a plain shell the size of each source terminal in the nearest
   *  free spot around it, and selects the new terminals. */
  pasteTerminals: (sources: TerminalClip[]) => void;
}

/** Copied terminal: its id plus its rect at copy time, used if the source
 *  has been closed before pasting. */
export interface TerminalClip {
  id: string;
  rect: Rect;
}

export function nodeRect(node: PodiumNode): Rect {
  return {
    x: node.position.x,
    y: node.position.y,
    width: node.width ?? node.measured?.width ?? 640,
    height: node.height ?? node.measured?.height ?? 400,
  };
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

  addTerminal: (position, command, label) =>
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

  applyTerminalCwds: (cwds) =>
    set((state) => {
      let changed = false;
      const nodes = state.nodes.map((n) => {
        const cwd = cwds[n.id];
        if (n.type !== "terminal" || cwd === undefined || n.data.cwd === cwd) {
          return n;
        }
        changed = true;
        return { ...n, data: { ...n.data, cwd } };
      });
      return changed ? { nodes } : state;
    }),

  pasteTerminals: (sources) =>
    set((state) => {
      const occupied = state.nodes.map(nodeRect);
      const created: TerminalFlowNode[] = [];
      for (const clip of sources) {
        const source = state.nodes.find((n) => n.id === clip.id);
        const rect = source ? nodeRect(source) : clip.rect;
        const position = findFreeSpot(rect, occupied);
        occupied.push({ ...rect, ...position });
        created.push({
          id: crypto.randomUUID(),
          type: "terminal",
          position,
          data: {
            label: nextLabel([...state.nodes, ...created]),
            autoStart: true,
          },
          dragHandle: ".terminal-node__header",
          width: rect.width,
          height: rect.height,
          selected: true,
        });
      }
      if (created.length === 0) return state;
      return {
        nodes: [
          ...state.nodes.map((n) =>
            n.selected ? ({ ...n, selected: false } as PodiumNode) : n,
          ),
          ...created,
        ],
      };
    }),
}));
