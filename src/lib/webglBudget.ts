/**
 * WebGL renderer budget. The webview caps live WebGL contexts (~16) and
 * silently drops the oldest beyond that, and every context holds GPU memory.
 * Only terminals on screen get one, most recently used first; the rest use
 * xterm's DOM renderer. Terminals off screen keep running and keep parsing
 * output — they just don't paint.
 */
const MAX_WEBGL_CONTEXTS = 8;

interface RendererEntry {
  visible: boolean;
  lastUsed: number;
  active: boolean;
  attach: () => boolean;
  detach: () => void;
}

const entries = new Map<string, RendererEntry>();
let scheduled = false;

function rebalance(): void {
  scheduled = false;
  const ranked = [...entries.values()]
    .filter((e) => e.visible)
    .sort((a, b) => b.lastUsed - a.lastUsed);
  const keep = new Set(ranked.slice(0, MAX_WEBGL_CONTEXTS));
  // Release before acquiring so the live count never exceeds the budget.
  for (const entry of entries.values()) {
    if (entry.active && !keep.has(entry)) {
      entry.detach();
      entry.active = false;
    }
  }
  for (const entry of keep) {
    if (!entry.active) entry.active = entry.attach();
  }
}

function schedule(): void {
  if (scheduled) return;
  scheduled = true;
  queueMicrotask(rebalance);
}

export interface RendererHandle {
  setVisible: (visible: boolean) => void;
  /** Marks the terminal as recently used (focus, input). */
  touch: () => void;
  /** The context was lost: fall back to DOM until the next rebalance. */
  lost: () => void;
  dispose: () => void;
}

/** `attach` loads a WebGL addon and returns whether it succeeded. */
export function registerRenderer(
  id: string,
  attach: () => boolean,
  detach: () => void,
): RendererHandle {
  const entry: RendererEntry = {
    visible: false,
    lastUsed: performance.now(),
    active: false,
    attach,
    detach,
  };
  entries.set(id, entry);
  return {
    setVisible: (visible) => {
      if (entry.visible === visible) return;
      entry.visible = visible;
      schedule();
    },
    touch: () => {
      entry.lastUsed = performance.now();
      if (!entry.active && entry.visible) schedule();
    },
    lost: () => {
      entry.active = false;
    },
    dispose: () => {
      if (entry.active) entry.detach();
      entries.delete(id);
      schedule();
    },
  };
}
