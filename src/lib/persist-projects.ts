import type { WorkspaceProject } from "../store/workspace-sidebar";

const STORAGE_KEY = "podium-projects";

export async function saveProjects(projects: WorkspaceProject[]): Promise<void> {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(projects));
  } catch (error) {
    console.error("Failed to save projects:", error);
  }
}

export async function loadProjects(): Promise<WorkspaceProject[]> {
  try {
    const data = localStorage.getItem(STORAGE_KEY);
    if (!data) return [];
    return JSON.parse(data) as WorkspaceProject[];
  } catch (error) {
    console.error("Failed to load projects:", error);
    return [];
  }
}