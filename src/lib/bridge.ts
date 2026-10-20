/**
 * Frontend half of the agent bridge. The backend owns the `podium` CLI
 * server and all timing; this side mirrors the canvas graph into it and
 * performs the canvas operations agents request (notes, recruits, wires).
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import {
  useWorkspaceStore,
  type NoteNodeData,
  type PodiumNode,
} from "../store/workspace";
import { useSettingsStore } from "../store/settings";

export type AgentState =
  | "starting"
  | "idle"
  | "working"
  | "waitingInput"
  | "exited";

/** Agent state per terminal id, pushed by the backend. Kept outside the
 *  React Flow nodes so state changes never re-render the canvas. */
export const useAgentStates = create<Record<string, AgentState>>(() => ({}));

const NOTE_NAME_MAX = 60;

/** A note's name: its explicit name, else its first non-empty line without
 *  markdown markers. */
export function noteName(data: NoteNodeData): string {
  if (data.name && data.name.trim() !== "") return data.name.trim();
  const first = data.text
    .split("\n")
    .map((l) => l.replace(/^[\s#>*\-+]+/, "").trim())
    .find((l) => l !== "");
  if (!first) return "Untitled note";
  return first.length > NOTE_NAME_MAX
    ? `${first.slice(0, NOTE_NAME_MAX - 1)}…`
    : first;
}

interface GraphNode {
  id: string;
  kind: "terminal" | "note";
  name: string;
  allowIncoming: boolean;
  orchestrator: boolean;
  parent: string | null;
  locked: boolean;
}

function graphNode(node: PodiumNode): GraphNode {
  if (node.type === "note") {
    return {
      id: node.id,
      kind: "note",
      name: noteName(node.data),
      allowIncoming: false,
      orchestrator: false,
      parent: null,
      locked: node.data.locked === true,
    };
  }
  return {
    id: node.id,
    kind: "terminal",
    name: node.data.label,
    allowIncoming: node.data.allowIncoming === true,
    orchestrator: node.data.orchestrator === true,
    parent: node.data.parent ?? null,
    locked: false,
  };
}

// ---------------------------------------------------------------------------
// Recruit spawn handshake: the request is answered once the PTY exists.
// ---------------------------------------------------------------------------

const pendingSpawns = new Map<
  string,
  { resolve: () => void; reject: (reason: string) => void }
>();

/** Called by a terminal node once its PTY spawn finished (or failed). */
export function reportSpawned(id: string, error?: string): void {
  const pending = pendingSpawns.get(id);
  if (!pending) return;
  pendingSpawns.delete(id);
  if (error === undefined) pending.resolve();
  else pending.reject(error);
}

function waitForSpawn(id: string): Promise<void> {
  return new Promise((resolve, reject) => {
    pendingSpawns.set(id, { resolve, reject });
  });
}

// ---------------------------------------------------------------------------
// Requests from agents
// ---------------------------------------------------------------------------

interface BridgeRequest {
  id: number;
  kind: "note" | "spawn" | "dismiss" | "connect";
  payload: Record<string, unknown>;
}

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function uniqueNoteName(base: string): string {
  const taken = new Set(
    useWorkspaceStore
      .getState()
      .nodes.filter((n) => n.type === "note")
      .map((n) => noteName(n.data as NoteNodeData).toLowerCase()),
  );
  if (!taken.has(base.toLowerCase())) return base;
  let i = 2;
  while (taken.has(`${base} (${i})`.toLowerCase())) i += 1;
  return `${base} (${i})`;
}

function findNote(id: string): NoteNodeData {
  const node = useWorkspaceStore.getState().nodes.find((n) => n.id === id);
  if (node?.type !== "note") throw new Error("note not found");
  return node.data;
}

async function handle(request: BridgeRequest): Promise<unknown> {
  const store = useWorkspaceStore.getState();
  const p = request.payload;
  switch (request.kind) {
    case "note": {
      const op = str(p.op);
      if (op === "read") {
        const data = findNote(str(p.id) ?? "");
        return { text: data.text, name: noteName(data) };
      }
      if (op === "write") {
        const id = str(p.id) ?? "";
        const data = findNote(id);
        if (data.locked) throw new Error("the note is locked");
        store.updateNodeData(id, { text: str(p.text) ?? "" });
        return { name: noteName(findNote(id)) };
      }
      if (op === "create") {
        const owner = str(p.owner) ?? "";
        const text = str(p.text) ?? "";
        const requested = str(p.name)?.trim();
        const name = requested
          ? uniqueNoteName(requested)
          : undefined;
        const id = store.addLinkedNote(owner, text, name);
        return { id, name: noteName(findNote(id)) };
      }
      throw new Error(`unknown note operation ${String(op)}`);
    }
    case "spawn": {
      const id = store.addRecruit({
        parent: str(p.parent) ?? "",
        name: str(p.name) ?? "Recruit",
        command: str(p.command) || undefined,
        cwd: str(p.cwd),
      });
      await waitForSpawn(id);
      return { id };
    }
    case "dismiss": {
      store.removeNode(str(p.id) ?? "");
      return {};
    }
    case "connect": {
      store.connectNodes(str(p.a) ?? "", str(p.b) ?? "");
      return {};
    }
  }
}

function graphKey(nodes: PodiumNode[], edges: { source: string; target: string }[]): string {
  return JSON.stringify([
    nodes.map(graphNode),
    edges.map((e) => [e.source, e.target]),
  ]);
}

let started = false;

/** Wires the canvas to the backend bridge. Call once at startup. */
export function startBridge(): void {
  if (started) return;
  started = true;

  let lastKey = "";
  const syncGraph = () => {
    const { nodes, edges, hydrated } = useWorkspaceStore.getState();
    if (!hydrated) return;
    const key = graphKey(nodes, edges);
    if (key === lastKey) return;
    lastKey = key;
    void invoke("bridge_sync_graph", {
      graph: {
        nodes: nodes.map(graphNode),
        edges: edges.map((e) => [e.source, e.target]),
      },
    }).catch(() => {});
  };
  useWorkspaceStore.subscribe((state, prev) => {
    if (state.nodes !== prev.nodes || state.edges !== prev.edges || state.hydrated !== prev.hydrated) {
      syncGraph();
    }
  });
  syncGraph();

  const syncSettings = () => {
    const { settings, loaded } = useSettingsStore.getState();
    if (!loaded) return;
    void invoke("bridge_sync_settings", {
      settings: settings.orchestration,
    }).catch(() => {});
  };
  useSettingsStore.subscribe((state, prev) => {
    if (
      state.settings.orchestration !== prev.settings.orchestration ||
      state.loaded !== prev.loaded
    ) {
      syncSettings();
    }
  });
  syncSettings();

  void listen<{ id: string; state: AgentState }>("agent-state", (event) => {
    useAgentStates.setState({ [event.payload.id]: event.payload.state });
  });
  void invoke<Record<string, AgentState>>("bridge_agent_states")
    .then((states) => useAgentStates.setState(states))
    .catch(() => {});

  void listen<BridgeRequest>("bridge-request", (event) => {
    const request = event.payload;
    handle(request)
      .then((value) =>
        invoke("bridge_respond", { id: request.id, ok: true, value: value ?? {} }),
      )
      .catch((err: unknown) =>
        invoke("bridge_respond", {
          id: request.id,
          ok: false,
          value: err instanceof Error ? err.message : String(err),
        }),
      )
      .catch(() => {});
  });
}

/** Shell command that starts `agent` wired to the bridge. */
export function agentCommand(agent: string, recruit: boolean): Promise<string> {
  return invoke<string>("bridge_agent_command", { agent, recruit });
}

/** Installs the `podium` skill for Claude Code and Codex. */
export function installAgentSkills(): Promise<void> {
  return invoke("install_agent_skills");
}
