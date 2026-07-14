import { useEffect } from "react";
import { Canvas } from "./canvas/Canvas";
import { useWorkspaceStore, type PodiumNode } from "./store/workspace";
import { useWorkspaceSidebarStore } from "./store/workspace-sidebar";
import { loadWorkspace, saveWorkspace } from "./lib/persist";
import { loadProjects, saveProjects } from "./lib/persist-projects";
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
  const { selectedProjectId, loadWorkspaceData, setProjects } = useWorkspaceSidebarStore();

  useEffect(() => {
    let cancelled = false;
    
    // Load projects first
    void loadProjects().then((loadedProjects) => {
      if (cancelled) return;
      setProjects(loadedProjects);
      
      // Then load workspace
      void loadWorkspace().then((saved) => {
        if (cancelled) return;
        const { setWorkspace } = useWorkspaceStore.getState();
        if (saved) setWorkspace(saved.nodes, saved.edges);
        else setWorkspace(firstRunNodes, []);
      });
    });
    
    return () => {
      cancelled = true;
    };
  }, [setProjects]);

  // Auto-save: debounced on change + safety flush every 10s.
  // Also saves workspace data to the selected project.
  useEffect(() => {
    let dirty = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const flush = () => {
      const state = useWorkspaceStore.getState();
      const sidebarState = useWorkspaceSidebarStore.getState();
      
      if (!state.hydrated || !dirty) return;
      dirty = false;
      
      // Save main workspace file
      void saveWorkspace(state.nodes, state.edges);
      
      // Save workspace data to the selected project
      if (selectedProjectId) {
        loadWorkspaceData(selectedProjectId, state.nodes, state.edges);
      }
      
      // Save projects (with their workspaceData) to localStorage
      void saveProjects(sidebarState.projects);
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

    return () => {
      unsubscribe();
      clearInterval(interval);
      clearTimeout(timer);
      window.removeEventListener("beforeunload", flush);
    };
  }, [selectedProjectId, loadWorkspaceData]);

  return (
    <div className="app">
      <Canvas />
    </div>
  );
}

export default App;
