import { useState } from "react";
import { useWorkspaceSidebarStore, type WorkspaceProject } from "../store/workspace-sidebar";
import { useWorkspaceStore } from "../store/workspace";

const PROJECT_ICONS = [
  { id: "folder", icon: "📁", label: "Pasta" },
  { id: "app", icon: "📱", label: "App" },
  { id: "web", icon: "🌐", label: "Web" },
  { id: "terminal", icon: "💻", label: "Terminal" },
  { id: "package", icon: "📦", label: "Pacote" },
  { id: "tool", icon: "🔧", label: "Ferramenta" },
  { id: "robot", icon: "🤖", label: "Bot" },
  { id: "code", icon: "📝", label: "Código" },
  { id: "globe", icon: "🌍", label: "Global" },
];

export function Sidebar() {
  const {
    projects,
    selectedProjectId,
    sidebarOpen,
    selectProject,
    addProject,
    removeProject,
    loadWorkspaceData,
  } = useWorkspaceSidebarStore();

  const [showNewProject, setShowNewProject] = useState(false);
  const [newProjectName, setNewProjectName] = useState("");
  const [newProjectIcon, setNewProjectIcon] = useState(PROJECT_ICONS[0].icon);
  const [newProjectPath, setNewProjectPath] = useState("");

  const nodes = useWorkspaceStore((s) => s.nodes);
  const edges = useWorkspaceStore((s) => s.edges);
  const setWorkspace = useWorkspaceStore((s) => s.setWorkspace);

  const getTerminalCount = (project: WorkspaceProject) => {
    return nodes.filter((n) => n.type === "terminal" && n.data?.cwd === project.path).length;
  };

  const handleCreateProject = () => {
    if (newProjectName.trim() && newProjectPath.trim()) {
      addProject({
        name: newProjectName.trim(),
        icon: newProjectIcon,
        path: newProjectPath.trim(),
      });
      setNewProjectName("");
      setNewProjectIcon(PROJECT_ICONS[0].icon);
      setNewProjectPath("");
      setShowNewProject(false);
    }
  };

  const handleSelectProject = (project: WorkspaceProject) => {
    // Save current workspace data to the previously selected project
    if (selectedProjectId) {
      const prevProject = projects.find(p => p.id === selectedProjectId);
      if (prevProject) {
        loadWorkspaceData(selectedProjectId, nodes, edges);
      }
    }
    
    // Load the new project's workspace data
    if (project.workspaceData) {
      setWorkspace(project.workspaceData.nodes, project.workspaceData.edges);
    } else {
      // Empty workspace for new project
      setWorkspace([], []);
    }
    
    selectProject(project.id);
  };

  return (
    <aside className={`sidebar ${sidebarOpen ? "sidebar--open" : ""}`}>
      <div className="sidebar__header">
        <span className="sidebar__title">Projects</span>
      </div>

      <div className="sidebar__search">
        <input type="text" placeholder="Filter..." className="sidebar__search-input" />
      </div>

      <div className="sidebar__content">
        <div className="sidebar__projects-list">
          {projects.map((project) => {
            const count = getTerminalCount(project);
            return (
              <div
                key={project.id}
                className={`sidebar__project ${
                  selectedProjectId === project.id ? "sidebar__project--selected" : ""
                }`}
                onClick={() => handleSelectProject(project)}
              >
                <span className="sidebar__project-icon">{project.icon}</span>
                <span className="sidebar__project-name">{project.name}</span>
                <div className="sidebar__project-meta">
                  {count > 0 && (
                    <span className={`sidebar__badge ${count > 0 ? "sidebar__badge--active" : ""}`}>
                      {count}
                    </span>
                  )}
                </div>
                <button
                  className="sidebar__project-delete"
                  onClick={(e) => {
                    e.stopPropagation();
                    removeProject(project.id);
                  }}
                  title="Remove project"
                >
                  ×
                </button>
              </div>
            );
          })}
        </div>

        {showNewProject ? (
          <div className="sidebar__new-project-form">
            <input
              type="text"
              placeholder="Project name"
              className="sidebar__input"
              value={newProjectName}
              onChange={(e) => setNewProjectName(e.target.value)}
              autoFocus
            />
            <div className="sidebar__icon-picker">
              {PROJECT_ICONS.slice(0, 4).map((opt) => (
                <button
                  key={opt.id}
                  className={`sidebar__icon-btn ${
                    newProjectIcon === opt.icon ? "sidebar__icon-btn--selected" : ""
                  }`}
                  onClick={() => setNewProjectIcon(opt.icon)}
                >
                  {opt.icon}
                </button>
              ))}
            </div>
            <input
              type="text"
              placeholder="Folder path (e.g., C:/Users/...)"
              className="sidebar__input sidebar__input--path"
              value={newProjectPath}
              onChange={(e) => setNewProjectPath(e.target.value)}
            />
            <div className="sidebar__actions">
              <button
                className="sidebar__btn sidebar__btn--primary"
                onClick={handleCreateProject}
              >
                Create
              </button>
              <button
                className="sidebar__btn"
                onClick={() => setShowNewProject(false)}
              >
                Cancel
              </button>
            </div>
          </div>
        ) : (
          <button className="sidebar__add-project" onClick={() => setShowNewProject(true)}>
            + Add Project
          </button>
        )}
      </div>

      <div className="sidebar__footer">
        <button className="sidebar__footer-btn" onClick={() => setShowNewProject(true)}>
          <span>📁</span> New Project
        </button>
      </div>
    </aside>
  );
}