import { memo, useState } from "react";
import {
  Handle,
  NodeResizer,
  Position,
  type NodeProps,
} from "@xyflow/react";
import type { BrowserFlowNode } from "../store/workspace";
import { useWorkspaceStore } from "../store/workspace";

function BrowserNodeComponent({ id, data, selected }: NodeProps<BrowserFlowNode>) {
  const [url, setUrl] = useState(data.url);
  const [loaded, setLoaded] = useState(data.url !== "about:blank");
  const updateNodeData = useWorkspaceStore((s) => s.updateNodeData);
  const removeNode = useWorkspaceStore((s) => s.removeNode);

  const normalizeUrl = (input: string): string => {
    const trimmed = input.trim();
    if (trimmed === "") return "about:blank";
    if (/^https?:\/\//i.test(trimmed)) return trimmed;
    return `https://${trimmed}`;
  };

  const navigate = () => {
    const target = normalizeUrl(url);
    setUrl(target);
    setLoaded(target !== "about:blank");
    updateNodeData(id, { url: target });
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") navigate();
  };

  const refresh = () => {
    setUrl((prev) => prev === "about:blank" ? "about:blank" : prev);
    setLoaded(true);
  };

  return (
    <div className="browser-node">
      <NodeResizer
        isVisible={selected}
        minWidth={360}
        minHeight={220}
        lineClassName="terminal-node__resize-line"
        handleClassName="terminal-node__resize-handle"
      />
      <Handle
        type="target"
        position={Position.Left}
        className="podium-handle"
      />
      <div className="browser-node__header">
        <span className="browser-node__label">
          {data.label ?? "Browser"}
        </span>
        <div className="browser-node__url-bar">
          <input
            className="browser-node__url-input nodrag"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Enter URL..."
          />
          <button
            className="browser-node__go nodrag"
            onClick={navigate}
          >
            Go
          </button>
          <button
            className="browser-node__refresh nodrag"
            onClick={refresh}
            title="Reload"
          >
            ↻
          </button>
        </div>
        <button
          className="node-close"
          title="Close browser"
          onClick={(e) => {
            e.stopPropagation();
            removeNode(id);
          }}
        >
          ×
        </button>
      </div>
      <div className="browser-node__body">
        {loaded ? (
          <iframe
            className="browser-node__iframe"
            src={url}
            title={data.label ?? "Browser"}
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
          />
        ) : (
          <div className="browser-node__placeholder">
            <p>Enter a URL and press Go to load a page</p>
          </div>
        )}
      </div>
    </div>
  );
}

export const BrowserNode = memo(BrowserNodeComponent);
