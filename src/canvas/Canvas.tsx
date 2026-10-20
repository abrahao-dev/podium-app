import { useRef } from "react";
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
import { useTerminalClipboard } from "./useTerminalClipboard";
import { useCanvasNavigation } from "./useCanvasNavigation";
import { useCrispViewport } from "./useCrispViewport";
import { SettingsButton } from "../settings/SettingsButton";
import { MissionPanel } from "../mission/MissionPanel";
import { useWorkspaceStore } from "../store/workspace";

const nodeTypes = { terminal: TerminalNode, note: NoteNode };

const MIN_ZOOM = 0.1;
const MAX_ZOOM = 2;
/** Whole-pixel node positions/sizes keep terminal text crisp at 100%. */
const PIXEL_GRID: [number, number] = [1, 1];

function CanvasInner() {
  const nodes = useWorkspaceStore((s) => s.nodes);
  const edges = useWorkspaceStore((s) => s.edges);
  const onNodesChange = useWorkspaceStore((s) => s.onNodesChange);
  const onEdgesChange = useWorkspaceStore((s) => s.onEdgesChange);
  const onConnect = useWorkspaceStore((s) => s.onConnect);
  const removeEdge = useWorkspaceStore((s) => s.removeEdge);
  const wrapperRef = useRef<HTMLDivElement>(null);
  useTerminalClipboard();
  useCanvasNavigation(wrapperRef, MIN_ZOOM, MAX_ZOOM);
  useCrispViewport(wrapperRef);

  return (
    <ReactFlow
      ref={wrapperRef}
      nodes={nodes}
      edges={edges}
      onNodesChange={onNodesChange}
      onEdgesChange={onEdgesChange}
      onConnect={onConnect}
      onEdgeDoubleClick={(_, edge) => removeEdge(edge.id)}
      nodeTypes={nodeTypes}
      minZoom={MIN_ZOOM}
      maxZoom={MAX_ZOOM}
      proOptions={{ hideAttribution: true }}
      deleteKeyCode={null}
      snapToGrid
      snapGrid={PIXEL_GRID}
    >
      <Background variant={BackgroundVariant.Dots} gap={24} size={1.5} />
      <Controls />
      <Toolbar />
      <SettingsButton />
      <MissionPanel />
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
