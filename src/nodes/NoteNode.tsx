import { memo, useState } from "react";
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

function NoteNodeComponent({ id, data, selected }: NodeProps<NoteFlowNode>) {
  const [editing, setEditing] = useState(data.text === "");
  const [renaming, setRenaming] = useState(false);
  const updateNodeData = useWorkspaceStore((s) => s.updateNodeData);
  const removeNode = useWorkspaceStore((s) => s.removeNode);

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
