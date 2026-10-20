import { useEffect, useRef, type RefObject } from "react";
import { useOnViewportChange, useReactFlow, type Viewport } from "@xyflow/react";

/** Zoom levels this close to 100% settle on exactly 100%. */
const SNAP_RANGE = 0.06;
/** Quiet time after the last viewport change before snapping. */
const SETTLE_MS = 180;

/** Viewport at exactly 100% around screen point (cx, cy), on whole pixels. */
function crispAt(vp: Viewport, cx: number, cy: number): Viewport {
  return {
    x: Math.round(cx - (cx - vp.x) / vp.zoom),
    y: Math.round(cy - (cy - vp.y) / vp.zoom),
    zoom: 1,
  };
}

/**
 * Terminals are bitmaps drawn at 100%; any other zoom, or a fractional
 * translation at 100%, resamples them and blurs the text. Once the viewport
 * settles near 100% it is snapped to exactly 100% on whole pixels, and
 * Ctrl/Cmd+0 jumps there directly.
 */
export function useCrispViewport(
  wrapperRef: RefObject<HTMLDivElement | null>,
): void {
  const { getViewport, setViewport } = useReactFlow();
  const timer = useRef<ReturnType<typeof setTimeout>>();
  const snapRef = useRef<() => void>(() => {});

  useEffect(() => {
    const snap = () => {
      const bounds = wrapperRef.current?.getBoundingClientRect();
      if (!bounds) return;
      const vp = getViewport();
      const next = crispAt(vp, bounds.width / 2, bounds.height / 2);
      if (next.x === vp.x && next.y === vp.y && next.zoom === vp.zoom) return;
      void setViewport(next);
    };

    const onKeyDown = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey) return;
      if (e.code !== "Digit0" && e.code !== "Numpad0") return;
      // Captured so the shell under focus doesn't receive it.
      e.preventDefault();
      e.stopPropagation();
      snap();
    };

    window.addEventListener("keydown", onKeyDown, true);
    snapRef.current = snap;
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      clearTimeout(timer.current);
    };
  }, [wrapperRef, getViewport, setViewport]);

  useOnViewportChange({
    onChange: (vp) => {
      clearTimeout(timer.current);
      if (Math.abs(vp.zoom - 1) > SNAP_RANGE) return;
      timer.current = setTimeout(() => snapRef.current(), SETTLE_MS);
    },
  });
}
