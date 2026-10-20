import { useEffect, type RefObject } from "react";
import { useReactFlow } from "@xyflow/react";

const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform);

/** Same wheel-to-zoom curve React Flow uses on the empty pane, so zooming
 *  over a node feels identical to zooming over the background. */
function wheelDelta(e: WheelEvent): number {
  const factor = e.ctrlKey && IS_MAC ? 10 : 1;
  const unit = e.deltaMode === 1 ? 0.05 : e.deltaMode ? 1 : 0.002;
  return -e.deltaY * unit * factor;
}

function isPanGesture(e: MouseEvent): boolean {
  return e.button === 1 || (e.button === 0 && e.altKey);
}

/**
 * Canvas navigation that also works with the cursor over terminals and notes
 * (whose bodies are `nowheel`/`nodrag`, so React Flow ignores them):
 * - Ctrl/Cmd + wheel (or trackpad pinch): zoom around the cursor.
 * - Middle-button drag or Alt + drag: pan.
 * Plain wheel over a node still scrolls its content.
 */
export function useCanvasNavigation(
  wrapperRef: RefObject<HTMLDivElement | null>,
  minZoom: number,
  maxZoom: number,
): void {
  const { getViewport, setViewport } = useReactFlow();

  useEffect(() => {
    const wrapper = wrapperRef.current;
    if (!wrapper) return;

    const onWheel = (e: WheelEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      const target = e.target;
      if (!(target instanceof Element) || !target.closest(".nowheel")) return;
      // Captured before xterm / the note sees it: no scrollback scroll.
      e.preventDefault();
      e.stopPropagation();

      const { x, y, zoom } = getViewport();
      const next = Math.min(
        maxZoom,
        Math.max(minZoom, zoom * Math.pow(2, wheelDelta(e))),
      );
      if (next === zoom) return;
      const bounds = wrapper.getBoundingClientRect();
      const px = e.clientX - bounds.left;
      const py = e.clientY - bounds.top;
      // Keep the flow point under the cursor fixed.
      setViewport({
        x: px - ((px - x) / zoom) * next,
        y: py - ((py - y) / zoom) * next,
        zoom: next,
      });
    };

    let pan: { pointerId: number; lastX: number; lastY: number } | null = null;

    const onPointerDown = (e: PointerEvent) => {
      if (pan || !isPanGesture(e)) return;
      e.preventDefault();
      e.stopPropagation();
      pan = { pointerId: e.pointerId, lastX: e.clientX, lastY: e.clientY };
      wrapper.setPointerCapture(e.pointerId);
      wrapper.classList.add("canvas--panning");
    };

    const onPointerMove = (e: PointerEvent) => {
      if (!pan || e.pointerId !== pan.pointerId) return;
      e.preventDefault();
      e.stopPropagation();
      const dx = e.clientX - pan.lastX;
      const dy = e.clientY - pan.lastY;
      pan.lastX = e.clientX;
      pan.lastY = e.clientY;
      if (dx === 0 && dy === 0) return;
      const viewport = getViewport();
      setViewport({ ...viewport, x: viewport.x + dx, y: viewport.y + dy });
    };

    const endPan = (e: PointerEvent) => {
      if (!pan || e.pointerId !== pan.pointerId) return;
      e.stopPropagation();
      if (wrapper.hasPointerCapture(pan.pointerId)) {
        wrapper.releasePointerCapture(pan.pointerId);
      }
      pan = null;
      wrapper.classList.remove("canvas--panning");
    };

    // Middle click would otherwise start autoscroll (Windows) or paste the
    // primary selection (Linux); Alt+click would move xterm's cursor.
    const swallowMouse = (e: MouseEvent) => {
      if (!isPanGesture(e)) return;
      e.preventDefault();
      e.stopPropagation();
    };

    const capture = { capture: true } as const;
    const activeCapture = { capture: true, passive: false } as const;
    wrapper.addEventListener("wheel", onWheel, activeCapture);
    wrapper.addEventListener("pointerdown", onPointerDown, activeCapture);
    wrapper.addEventListener("pointermove", onPointerMove, activeCapture);
    wrapper.addEventListener("pointerup", endPan, capture);
    wrapper.addEventListener("pointercancel", endPan, capture);
    wrapper.addEventListener("mousedown", swallowMouse, activeCapture);
    wrapper.addEventListener("mouseup", swallowMouse, activeCapture);
    wrapper.addEventListener("auxclick", swallowMouse, activeCapture);

    return () => {
      wrapper.removeEventListener("wheel", onWheel, activeCapture);
      wrapper.removeEventListener("pointerdown", onPointerDown, activeCapture);
      wrapper.removeEventListener("pointermove", onPointerMove, activeCapture);
      wrapper.removeEventListener("pointerup", endPan, capture);
      wrapper.removeEventListener("pointercancel", endPan, capture);
      wrapper.removeEventListener("mousedown", swallowMouse, activeCapture);
      wrapper.removeEventListener("mouseup", swallowMouse, activeCapture);
      wrapper.removeEventListener("auxclick", swallowMouse, activeCapture);
    };
  }, [wrapperRef, getViewport, setViewport, minZoom, maxZoom]);
}
