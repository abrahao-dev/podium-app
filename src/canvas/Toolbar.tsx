import { useState } from "react";
import { Panel, useReactFlow } from "@xyflow/react";
import { useWorkspaceStore } from "../store/workspace";
import { agentCommand } from "../lib/bridge";

/** Launch presets. Agent commands are resolved by the backend so agents
 *  report their state to the bridge; `agent: undefined` = plain shell. */
const AGENT_PRESETS: {
  label: string;
  agent?: string;
  hint?: string;
  orchestrator?: boolean;
}[] = [
  { label: "Shell" },
  { label: "Claude Code", agent: "claude", hint: "claude" },
  { label: "Codex", agent: "codex", hint: "codex" },
  { label: "OpenCode", agent: "opencode", hint: "opencode" },
  { label: "Orchestrator", agent: "claude", hint: "♛ claude", orchestrator: true },
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
    // Whole pixels keep terminal text crisp at 100% zoom.
    return {
      x: Math.round(pos.x + offsetX),
      y: Math.round(pos.y + offsetY),
    };
  };

  const spawn = (preset: (typeof AGENT_PRESETS)[number]) => {
    setMenuOpen(false);
    const position = centerPosition(-320, -200);
    const extra = preset.orchestrator
      ? { orchestrator: true, allowIncoming: true }
      : undefined;
    const label = preset.orchestrator ? "Orchestrator" : undefined;
    if (!preset.agent) {
      addTerminal(position, undefined, label, extra);
      return;
    }
    const fallback = preset.agent;
    void agentCommand(preset.agent, false)
      .catch(() => fallback)
      .then((command) => addTerminal(position, command || fallback, label, extra));
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
                  onClick={() => spawn(preset)}
                >
                  {preset.label}
                  {preset.hint && (
                    <span className="toolbar__menu-cmd">{preset.hint}</span>
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
