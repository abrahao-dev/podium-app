import { useState } from "react";
import { Panel, useReactFlow } from "@xyflow/react";
import { useWorkspaceStore } from "../store/workspace";

/** Preset launch commands. `undefined` = plain shell (PATH-resolved by the
 *  spawned shell, so bare command names work cross-platform). */
const AGENT_PRESETS: { label: string; command?: string }[] = [
  { label: "Shell", command: undefined },
  { label: "Claude Code", command: "claude" },
  { label: "Codex", command: "codex" },
  { label: "OpenCode", command: "opencode" },
];

export function Toolbar() {
  const addTerminal = useWorkspaceStore((s) => s.addTerminal);
  const addNote = useWorkspaceStore((s) => s.addNote);
  const { screenToFlowPosition } = useReactFlow();
  const [menuOpen, setMenuOpen] = useState(false);

  const centerPosition = (offsetX: number, offsetY: number) => {
    const pos = screenToFlowPosition({
      x: window.innerWidth / 2,
      y: window.innerHeight / 2,
    });
    return { x: pos.x + offsetX, y: pos.y + offsetY };
  };

  const spawn = (command?: string) => {
    addTerminal(centerPosition(-320, -200), command);
    setMenuOpen(false);
  };

  return (
    <Panel position="top-center" className="toolbar">
      <div className="toolbar__dropdown">
        <button
          className="toolbar__button"
          onClick={() => setMenuOpen((o) => !o)}
        >
          + Terminal ▾
        </button>
        {menuOpen && (
          <>
            <div
              className="toolbar__backdrop"
              onClick={() => setMenuOpen(false)}
            />
            <div className="toolbar__menu">
              {AGENT_PRESETS.map((preset) => (
                <button
                  key={preset.label}
                  className="toolbar__menu-item"
                  onClick={() => spawn(preset.command)}
                >
                  {preset.label}
                  {preset.command && (
                    <span className="toolbar__menu-cmd">{preset.command}</span>
                  )}
                </button>
              ))}
            </div>
          </>
        )}
      </div>
      <button
        className="toolbar__button"
        onClick={() => addNote(centerPosition(-150, -110))}
      >
        + Note
      </button>
    </Panel>
  );
}
