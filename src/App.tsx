import { useEffect } from "react";
import { Canvas } from "./canvas/Canvas";
import { useWorkspaceStore, type PodiumNode } from "./store/workspace";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { loadWorkspace, saveWorkspace } from "./lib/persist";
import { getTerminalCwds } from "./lib/pty";
import "./App.css";

const CLOSE_SAVE_TIMEOUT_MS = 2000;

const firstRunNodes: PodiumNode[] = [
  {
    id: crypto.randomUUID(),
    type: "terminal",
    position: { x: 120, y: 80 },
    data: { label: "Terminal 1", autoStart: true },
    dragHandle: ".terminal-node__header",
    width: 720,
    height: 460,
  },
];

function App() {
  useEffect(() => {
    let cancelled = false;
    void loadWorkspace().then((saved) => {
      if (cancelled) return;
      const { setWorkspace } = useWorkspaceStore.getState();
      if (saved) setWorkspace(saved.nodes, saved.edges);
      else setWorkspace(firstRunNodes, []);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // Auto-save: debounced on change + safety flush every 10s.
  useEffect(() => {
    let dirty = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const flush = () => {
      const state = useWorkspaceStore.getState();
      if (!state.hydrated || !dirty) return;
      dirty = false;
      void saveWorkspace(state.nodes, state.edges);
    };

    const unsubscribe = useWorkspaceStore.subscribe((state, prev) => {
      if (!state.hydrated || !prev.hydrated) return;
      if (state.nodes === prev.nodes && state.edges === prev.edges) return;
      dirty = true;
      clearTimeout(timer);
      timer = setTimeout(flush, 1500);
    });
    const interval = setInterval(flush, 10_000);
    // Best-effort save if the window closes inside the debounce window.
    window.addEventListener("beforeunload", flush);

    // On close, capture every running shell's directory and save before the
    // window is destroyed. Bounded so a stuck call can never block closing.
    const unlistenClose = getCurrentWindow().onCloseRequested(async () => {
      const state = useWorkspaceStore.getState();
      if (!state.hydrated) return;
      const ids = state.nodes
        .filter((n) => n.type === "terminal")
        .map((n) => n.id);
      const save = (async () => {
        if (ids.length > 0) {
          state.applyTerminalCwds(await getTerminalCwds(ids));
        }
        clearTimeout(timer);
        dirty = false;
        const latest = useWorkspaceStore.getState();
        await saveWorkspace(latest.nodes, latest.edges);
      })();
      await Promise.race([
        save.catch(() => {}),
        new Promise((resolve) => setTimeout(resolve, CLOSE_SAVE_TIMEOUT_MS)),
      ]);
    });

    return () => {
      unsubscribe();
      clearInterval(interval);
      clearTimeout(timer);
      window.removeEventListener("beforeunload", flush);
      void unlistenClose.then((fn) => fn());
    };
  }, []);

  return (
    <div className="app">
      <Canvas />
    </div>
  );
}

export default App;
