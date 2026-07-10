import { Panel, useReactFlow } from "@xyflow/react";
import { useWorkspaceStore } from "../store/workspace";

export function Toolbar() {
  const addTerminal = useWorkspaceStore((s) => s.addTerminal);
  const addNote = useWorkspaceStore((s) => s.addNote);
  const { screenToFlowPosition } = useReactFlow();

  const centerPosition = (offsetX: number, offsetY: number) => {
    const pos = screenToFlowPosition({
      x: window.innerWidth / 2,
      y: window.innerHeight / 2,
    });
    return { x: pos.x + offsetX, y: pos.y + offsetY };
  };

  return (
    <Panel position="top-center" className="toolbar">
      <button
        className="toolbar__button"
        onClick={() => addTerminal(centerPosition(-320, -200))}
      >
        + Terminal
      </button>
      <button
        className="toolbar__button"
        onClick={() => addNote(centerPosition(-150, -110))}
      >
        + Note
      </button>
    </Panel>
  );
}
