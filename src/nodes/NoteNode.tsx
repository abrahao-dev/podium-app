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

function NoteNodeComponent({ id, data, selected }: NodeProps<NoteFlowNode>) {
  const [editing, setEditing] = useState(data.text === "");
  const updateNodeData = useWorkspaceStore((s) => s.updateNodeData);
  const removeNode = useWorkspaceStore((s) => s.removeNode);

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
      <div className="note-node__header">
        <span className="note-node__title">Note</span>
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
