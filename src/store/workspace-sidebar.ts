import { create } from "zustand";

export interface WorkspaceProject {
  id: string;
  name: string;
  icon: string;
  path: string;
  workspaceData?: { nodes: any[]; edges: any[] };
}

interface WorkspaceSidebarState {
  projects: WorkspaceProject[];
  selectedProjectId: string | null;
  sidebarOpen: boolean;
  addProject: (project: Omit<WorkspaceProject, "id">) => void;
  updateProject: (projectId: string, updates: Partial<WorkspaceProject>) => void;
  removeProject: (projectId: string) => void;
  selectProject: (projectId: string | null) => void;
  toggleSidebar: () => void;
  setProjects: (projects: WorkspaceProject[]) => void;
  loadWorkspaceData: (projectId: string, nodes: any[], edges: any[]) => void;
}

export const useWorkspaceSidebarStore = create<WorkspaceSidebarState>((set) => ({
  projects: [],
  selectedProjectId: null,
  sidebarOpen: true,

  addProject: (project) =>
    set((state) => ({
      projects: [
        ...state.projects,
        { ...project, id: crypto.randomUUID() },
      ],
    })),

  updateProject: (projectId, updates) =>
    set((state) => ({
      projects: state.projects.map((p) =>
        p.id === projectId ? { ...p, ...updates } : p
      ),
    })),

  removeProject: (projectId) =>
    set((state) => ({
      projects: state.projects.filter((p) => p.id !== projectId),
      selectedProjectId:
        state.selectedProjectId === projectId ? null : state.selectedProjectId,
    })),

  selectProject: (projectId) => set({ selectedProjectId: projectId }),

  toggleSidebar: () => set((state) => ({ sidebarOpen: !state.sidebarOpen })),

  setProjects: (projects) => set({ projects }),

  loadWorkspaceData: (projectId, nodes, edges) =>
    set((state) => ({
      projects: state.projects.map((p) =>
        p.id === projectId ? { ...p, workspaceData: { nodes, edges } } : p
      ),
    })),
}));