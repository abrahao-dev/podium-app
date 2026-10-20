import { memo, useEffect, useRef, useState } from "react";
import {
  Handle,
  NodeResizer,
  Position,
  type NodeProps,
} from "@xyflow/react";
import type { NoteFlowNode } from "../store/workspace";
import { useWorkspaceStore } from "../store/workspace";
import { renderMarkdown } from "../lib/markdown";
import { noteName } from "../lib/bridge";
import { runNoteAsMission } from "../mission/missions";

function NoteNodeComponent({ id, data, selected }: NodeProps<NoteFlowNode>) {
  const [editing, setEditing] = useState(data.text === "");
  const [renaming, setRenaming] = useState(false);
  const updateNodeData = useWorkspaceStore((s) => s.updateNodeData);
  const removeNode = useWorkspaceStore((s) => s.removeNode);
  // Wired straight to an orchestrator: the note can be run as a mission.
  const hasOrchestrator = useWorkspaceStore((s) =>
    s.edges.some((e) => {
      const other = e.source === id ? e.target : e.target === id ? e.source : null;
      if (other === null) return false;
      const node = s.nodes.find((n) => n.id === other);
      return node?.type === "terminal" && node.data.orchestrator === true;
    }),
  );
  const [runState, setRunState] = useState<string | null>(null);
  const runTimer = useRef<ReturnType<typeof setTimeout>>();
  useEffect(() => () => clearTimeout(runTimer.current), []);

  const run = () => {
    setRunState("…");
    runNoteAsMission(id)
      .then((name) => setRunState(`→ ${name}`))
      .catch((err: unknown) => setRunState(String(err)))
      .finally(() => {
        clearTimeout(runTimer.current);
        runTimer.current = setTimeout(() => setRunState(null), 2500);
      });
  };

  const commitName = (value: string) => {
    const name = value.trim();
    updateNodeData(id, { name: name === "" ? undefined : name });
    setRenaming(false);
  };

  return (
    <div className="note-node" onDoubleClick={() => setEditing((e) => !e)}>
      <NodeResizer
        isVisible={selected}
        minWidth={200}
        minHeight={120}
        lineClassName="terminal-node__resize-line"
        handleClassName="terminal-node__resize-handle"
      />
      <Handle
        type="target"
        position={Position.Left}
        className="podium-handle"
      />
      <Handle
        type="source"
        position={Position.Right}
        className="podium-handle"
      />
      <div className="note-node__header">
        {renaming ? (
          <input
            className="note-node__name-input nodrag"
            defaultValue={data.name ?? ""}
            placeholder={noteName({ ...data, name: undefined })}
            autoFocus
            onDoubleClick={(e) => e.stopPropagation()}
            onBlur={(e) => commitName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitName(e.currentTarget.value);
              if (e.key === "Escape") setRenaming(false);
            }}
          />
        ) : (
          <span
            className="note-node__title"
            title="Double-click to rename"
            onDoubleClick={(e) => {
              e.stopPropagation();
              setRenaming(true);
            }}
          >
            {noteName(data)}
          </span>
        )}
        {hasOrchestrator && (
          <button
            className="note-node__run nodrag"
            title="Run as mission"
            disabled={data.text.trim() === "" || runState === "…"}
            onClick={(e) => {
              e.stopPropagation();
              run();
            }}
            onDoubleClick={(e) => e.stopPropagation()}
          >
            {runState ?? "▶ Run"}
          </button>
        )}
        <button
          className={`note-node__lock nodrag${
            data.locked ? " note-node__lock--on" : ""
          }`}
          title={data.locked ? "Agents: read-only" : "Agents: read and write"}
          aria-pressed={data.locked === true}
          onClick={(e) => {
            e.stopPropagation();
            updateNodeData(id, { locked: !data.locked });
          }}
          onDoubleClick={(e) => e.stopPropagation()}
        >
          {data.locked ? "🔒" : "🔓"}
        </button>
        <button
          className="node-close"
          title="Delete note"
          onClick={(e) => {
            e.stopPropagation();
            removeNode(id);
          }}
        >
          ×
        </button>
      </div>
      {editing ? (
        <textarea
          className="note-node__editor nodrag nowheel"
          value={data.text}
          placeholder="Write markdown… double-click to preview"
          autoFocus
          onChange={(e) => updateNodeData(id, { text: e.target.value })}
          onDoubleClick={(e) => e.stopPropagation()}
          onBlur={() => {
            if (data.text !== "") setEditing(false);
          }}
        />
      ) : (
        <div className="note-node__preview nodrag nowheel">
          {data.text === "" ? (
            <p className="note-node__hint">Double-click to edit</p>
          ) : (
            renderMarkdown(data.text)
          )}
        </div>
      )}
    </div>
  );
}

export const NoteNode = memo(NoteNodeComponent);
