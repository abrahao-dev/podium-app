import { useEffect, useState } from "react";
import { useReactFlow } from "@xyflow/react";

export function ZoomControls() {
  const { zoomTo, getZoom, fitView } = useReactFlow();
  const [zoom, setZoom] = useState(1);

  useEffect(() => {
    const updateZoom = () => {
      const current = getZoom();
      if (current !== null) setZoom(current);
    };
    updateZoom();
    const interval = setInterval(updateZoom, 100);
    return () => clearInterval(interval);
  }, [getZoom]);

  const handleZoomIn = () => {
    const next = Math.min(zoom + 0.1, 2);
    zoomTo(next, { duration: 150 });
  };

  const handleZoomOut = () => {
    const next = Math.max(zoom - 0.1, 0.1);
    zoomTo(next, { duration: 150 });
  };

  const currentZoom = Math.round(zoom * 100);

  return (
    <div className="zoom-controls">
      <button
        className="zoom-controls__btn"
        onClick={handleZoomOut}
        title="Zoom out"
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <line x1="5" y1="12" x2="19" y2="12" />
        </svg>
      </button>
      <span className="zoom-controls__value">{currentZoom}%</span>
      <button
        className="zoom-controls__btn"
        onClick={handleZoomIn}
        title="Zoom in"
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <line x1="12" y1="5" x2="12" y2="19" />
          <line x1="5" y1="12" x2="19" y2="12" />
        </svg>
      </button>
      <div className="zoom-controls__separator" />
      <button
        className="zoom-controls__btn"
        onClick={() => fitView({ padding: 0.2, duration: 300 })}
        title="Fit view"
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M8 3H5a2 2 0 0 0-2 2v3" />
          <path d="M16 3h3a2 2 0 0 1 2 2v3" />
          <path d="M8 21H5a2 2 0 0 1-2-2v-3" />
          <path d="M16 21h3a2 2 0 0 0 2-2v-3" />
        </svg>
      </button>
    </div>
  );
}