import {
  Background,
  BackgroundVariant,
  Controls,
  ReactFlow,
  ReactFlowProvider,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { TerminalNode } from "../nodes/TerminalNode";
import { NoteNode } from "../nodes/NoteNode";
import { Toolbar } from "./Toolbar";
import { useWorkspaceStore } from "../store/workspace";

const nodeTypes = { terminal: TerminalNode, note: NoteNode };

function CanvasInner() {
  const nodes = useWorkspaceStore((s) => s.nodes);
  const edges = useWorkspaceStore((s) => s.edges);
  const onNodesChange = useWorkspaceStore((s) => s.onNodesChange);
  const onEdgesChange = useWorkspaceStore((s) => s.onEdgesChange);
  const onConnect = useWorkspaceStore((s) => s.onConnect);
  const removeEdge = useWorkspaceStore((s) => s.removeEdge);

  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      onNodesChange={onNodesChange}
      onEdgesChange={onEdgesChange}
      onConnect={onConnect}
      onEdgeDoubleClick={(_, edge) => removeEdge(edge.id)}
      nodeTypes={nodeTypes}
      minZoom={0.1}
      maxZoom={2}
      proOptions={{ hideAttribution: true }}
      deleteKeyCode={null}
    >
      <Background variant={BackgroundVariant.Dots} gap={24} size={1.5} />
      <Controls />
      <Toolbar />
    </ReactFlow>
  );
}

export function Canvas() {
  return (
    <ReactFlowProvider>
      <CanvasInner />
    </ReactFlowProvider>
  );
}
