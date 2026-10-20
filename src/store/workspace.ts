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
import { sameAgentSession } from "../lib/agents";
import type { AgentSession } from "../lib/pty";

export interface TerminalNodeData extends Record<string, unknown> {
  label: string;
  command?: string;
  cwd?: string;
  /** True only for nodes created this session — restored nodes require a
   *  click to respawn (never auto-run agents on load). */
  autoStart: boolean;
  /** Opt-in: connected agents may write handoff prompts into this terminal. */
  allowIncoming?: boolean;
  /** Agent CLI last seen running in this terminal, resumed on next launch. */
  agentSession?: AgentSession;
  /** May spawn and dismiss recruits through the `podium` CLI. */
  orchestrator?: boolean;
  /** Orchestrator terminal that spawned this one. */
  parent?: string;
}

export interface NoteNodeData extends Record<string, unknown> {
  text: string;
  /** Stable name set by the user or an agent; otherwise the first line. */
  name?: string;
  /** Agents may read but not change the note. */
  locked?: boolean;
}

/** Recruit terminals open smaller than user-made ones. */
export const RECRUIT_SIZE = { width: 560, height: 340 };

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
  addTerminal: (
    position: XYPosition,
    command?: string,
    label?: string,
    extra?: Partial<TerminalNodeData>,
  ) => void;
  addNote: (position: XYPosition) => void;
  removeNode: (id: string) => void;
  updateNodeData: (
    id: string,
    data: Partial<TerminalNodeData & NoteNodeData>,
  ) => void;
  setWorkspace: (nodes: PodiumNode[], edges: Edge[]) => void;
  /** Applies shell directories by terminal id; no-op when nothing changed. */
  applyTerminalCwds: (cwds: Record<string, string>) => void;
  /** Records the agent running in each queried terminal; a queried id absent
   *  from `agents` has no agent anymore. No-op when nothing changed. */
  applyTerminalAgents: (
    ids: string[],
    agents: Record<string, AgentSession>,
  ) => void;
  /** Spawns a plain shell the size of each source terminal in the nearest
   *  free spot around it, and selects the new terminals. */
  pasteTerminals: (sources: TerminalClip[]) => void;
  /** Recruit terminal next to its orchestrator, wired to it. Returns its id. */
  addRecruit: (recruit: {
    parent: string;
    name: string;
    command?: string;
    cwd?: string;
  }) => string;
  /** Note next to `owner`, wired to it. Returns its id. */
  addLinkedNote: (owner: string, text: string, name?: string) => string;
  connectNodes: (a: string, b: string) => void;
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

function edgeBetween(source: string, target: string): Edge {
  return {
    id: `xy-edge__${source}-${target}`,
    source,
    target,
    animated: true,
    markerEnd: { type: MarkerType.ArrowClosed },
  };
}

export const useWorkspaceStore = create<WorkspaceState>((set, get) => ({
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

  addTerminal: (position, command, label, extra) =>
    set((state) => ({
      nodes: [
        ...state.nodes,
        {
          id: crypto.randomUUID(),
          type: "terminal",
          position,
          data: {
            ...extra,
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

  applyTerminalAgents: (ids, agents) =>
    set((state) => {
      const queried = new Set(ids);
      let changed = false;
      const nodes = state.nodes.map((n) => {
        if (n.type !== "terminal" || !queried.has(n.id)) return n;
        const agentSession = agents[n.id];
        if (sameAgentSession(n.data.agentSession, agentSession)) return n;
        changed = true;
        return { ...n, data: { ...n.data, agentSession } };
      });
      return changed ? { nodes } : state;
    }),

  addRecruit: ({ parent, name, command, cwd }) => {
    const id = crypto.randomUUID();
    set((state) => {
      const owner = state.nodes.find((n) => n.id === parent);
      const occupied = state.nodes.map(nodeRect);
      const anchor: Rect = owner
        ? { ...nodeRect(owner), ...RECRUIT_SIZE }
        : { x: 0, y: 0, ...RECRUIT_SIZE };
      // Recruits gather to the right of their orchestrator.
      const position = findFreeSpot(
        owner ? { ...anchor, x: anchor.x + nodeRect(owner).width + 40 } : anchor,
        occupied,
      );
      const node: TerminalFlowNode = {
        id,
        type: "terminal",
        position,
        data: {
          label: name,
          autoStart: true,
          command,
          cwd,
          parent,
          allowIncoming: true,
        },
        dragHandle: ".terminal-node__header",
        ...RECRUIT_SIZE,
      };
      return {
        nodes: [...state.nodes, node],
        edges: [...state.edges, edgeBetween(parent, id)],
      };
    });
    return id;
  },

  addLinkedNote: (owner, text, name) => {
    const id = crypto.randomUUID();
    set((state) => {
      const anchorNode = state.nodes.find((n) => n.id === owner);
      const size = { width: 300, height: 220 };
      const base: Rect = anchorNode
        ? { ...nodeRect(anchorNode), ...size }
        : { x: 0, y: 0, ...size };
      // Notes appear to the left of the terminal that created them.
      const position = findFreeSpot(
        { ...base, x: base.x - size.width - 40 },
        state.nodes.map(nodeRect),
      );
      const node: NoteFlowNode = {
        id,
        type: "note",
        position,
        data: { text, name },
        dragHandle: ".note-node__header",
        ...size,
      };
      return {
        nodes: [...state.nodes, node],
        edges: [...state.edges, edgeBetween(id, owner)],
      };
    });
    return id;
  },

  connectNodes: (a, b) => {
    const { edges } = get();
    const exists = edges.some(
      (e) =>
        (e.source === a && e.target === b) ||
        (e.source === b && e.target === a),
    );
    if (exists || a === b) return;
    set((state) => ({ edges: [...state.edges, edgeBetween(a, b)] }));
  },

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
