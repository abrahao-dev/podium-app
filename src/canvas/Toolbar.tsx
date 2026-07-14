import { useState } from "react";
import { Panel, useReactFlow } from "@xyflow/react";
import { useWorkspaceStore } from "../store/workspace";
import { useWorkspaceSidebarStore } from "../store/workspace-sidebar";

const TerminalIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <polyline points="4 17 10 11 4 5" />
    <line x1="12" y1="19" x2="20" y2="19" />
  </svg>
);

const NoteIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
    <polyline points="14 2 14 8 20 8" />
    <line x1="16" y1="13" x2="8" y2="13" />
    <line x1="16" y1="17" x2="8" y2="17" />
    <polyline points="10 9 9 9 8 9" />
  </svg>
);

const BrowserIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
    <line x1="3" y1="9" x2="21" y2="9" />
    <line x1="9" y1="21" x2="9" y2="9" />
  </svg>
);

const ChevronDown = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <polyline points="6 9 12 15 18 9" />
  </svg>
);

export function Toolbar() {
  const addTerminal = useWorkspaceStore((s) => s.addTerminal);
  const addNote = useWorkspaceStore((s) => s.addNote);
  const addBrowser = useWorkspaceStore((s) => s.addBrowser);
  const { screenToFlowPosition } = useReactFlow();
  const selectedProjectId = useWorkspaceSidebarStore((s) => s.selectedProjectId);
  const projects = useWorkspaceSidebarStore((s) => s.projects);
  const [terminalMenuOpen, setTerminalMenuOpen] = useState(false);
  const [noteMenuOpen, setNoteMenuOpen] = useState(false);
  const [browserMenuOpen, setBrowserMenuOpen] = useState(false);

  const selectedProject = projects.find((p) => p.id === selectedProjectId);

  const centerPosition = (offsetX: number, offsetY: number) => {
    const pos = screenToFlowPosition({
      x: window.innerWidth / 2,
      y: window.innerHeight / 2,
    });
    return { x: pos.x + offsetX, y: pos.y + offsetY };
  };

  const spawn = (command?: string) => {
    addTerminal(
      centerPosition(-320, -200),
      command,
      undefined,
      selectedProject?.path
    );
    setTerminalMenuOpen(false);
  };

  return (
    <Panel position="bottom-center" className="toolbar">
      {terminalMenuOpen && (
        <>
          <div className="toolbar__backdrop" onClick={() => setTerminalMenuOpen(false)} />
          <div className="toolbar__menu toolbar__menu--terminal">
            <button
              className="toolbar__menu-item"
              onClick={() => spawn(undefined)}
            >
              Plain Shell
            </button>
            <button
              className="toolbar__menu-item"
              onClick={() => spawn("claude")}
            >
              Claude Code
            </button>
            <button
              className="toolbar__menu-item"
              onClick={() => spawn("codex")}
            >
              Codex
            </button>
            <button
              className="toolbar__menu-item"
              onClick={() => spawn("opencode")}
            >
              OpenCode
            </button>
          </div>
        </>
      )}
      {noteMenuOpen && (
        <>
          <div className="toolbar__backdrop" onClick={() => setNoteMenuOpen(false)} />
          <div className="toolbar__menu toolbar__menu--note">
            <button className="toolbar__menu-item" onClick={() => { addNote(centerPosition(-150, -110)); setNoteMenuOpen(false); }}>
              Markdown Note
            </button>
          </div>
        </>
      )}
      {browserMenuOpen && (
        <>
          <div className="toolbar__backdrop" onClick={() => setBrowserMenuOpen(false)} />
          <div className="toolbar__menu toolbar__menu--browser">
            <button className="toolbar__menu-item" onClick={() => { addBrowser(centerPosition(-300, -225)); setBrowserMenuOpen(false); }}>
              Browser Window
            </button>
          </div>
        </>
      )}
      <div className="toolbar__group">
        <button
          className={`toolbar__button ${terminalMenuOpen ? "toolbar__button--active" : ""}`}
          onClick={() => setTerminalMenuOpen((o) => !o)}
          title="Add Terminal"
        >
          <TerminalIcon />
          <ChevronDown />
        </button>
      </div>
      <div className="toolbar__group">
        <button
          className={`toolbar__button ${noteMenuOpen ? "toolbar__button--active" : ""}`}
          onClick={() => setNoteMenuOpen((o) => !o)}
          title="Add Note"
        >
          <NoteIcon />
          <ChevronDown />
        </button>
      </div>
      <div className="toolbar__group">
        <button
          className={`toolbar__button ${browserMenuOpen ? "toolbar__button--active" : ""}`}
          onClick={() => setBrowserMenuOpen((o) => !o)}
          title="Add Browser"
        >
          <BrowserIcon />
          <ChevronDown />
        </button>
      </div>
    </Panel>
  );
}