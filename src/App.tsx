import { useEffect } from "react";
import { Canvas } from "./canvas/Canvas";
import { useWorkspaceStore, type PodiumNode } from "./store/workspace";
import { loadWorkspace, saveWorkspace } from "./lib/persist";
import "./App.css";

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

    return () => {
      unsubscribe();
      clearInterval(interval);
      clearTimeout(timer);
    };
  }, []);

  return (
    <div className="app">
      <Canvas />
    </div>
  );
}

export default App;
