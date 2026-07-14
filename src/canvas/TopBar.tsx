import { useWorkspaceSidebarStore } from "../store/workspace-sidebar";

export function TopBar() {
  const { sidebarOpen, toggleSidebar, selectedProjectId, projects } = useWorkspaceSidebarStore();

  const selectedProject = projects.find((p) => p.id === selectedProjectId);

  return (
    <div className="top-bar">
      <button
        className="top-bar__btn"
        onClick={toggleSidebar}
        title={sidebarOpen ? "Close sidebar" : "Open sidebar"}
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          {sidebarOpen ? (
            <path d="M18 6L6 18M6 6l12 12" />
          ) : (
            <path d="M4 6h16M4 12h16M4 18h16" />
          )}
        </svg>
      </button>
      <div className="top-bar__separator" />
      <div className="top-bar__workspace">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="2" />
          <path d="M3 9h18M9 21V9" />
        </svg>
        <span>{selectedProject?.name ?? "My Workspace"}</span>
      </div>
    </div>
  );
}