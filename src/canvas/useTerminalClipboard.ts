import { useEffect } from "react";
import {
  nodeRect,
  useWorkspaceStore,
  type TerminalClip,
} from "../store/workspace";

/** Focus targets that own Ctrl+C / Ctrl+V themselves (shell input, fields). */
const TEXT_TARGETS =
  ".xterm, input, textarea, select, [contenteditable=''], [contenteditable='true']";

/**
 * Ctrl/Cmd+C on selected terminals (focus outside the shell) copies them;
 * Ctrl/Cmd+V spawns a plain shell of the same size next to each one.
 */
export function useTerminalClipboard(): void {
  useEffect(() => {
    let clipboard: TerminalClip[] = [];

    const onKeyDown = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey) return;
      if (e.code !== "KeyC" && e.code !== "KeyV") return;
      const target = e.target;
      if (target instanceof Element && target.closest(TEXT_TARGETS)) return;

      const { nodes, pasteTerminals } = useWorkspaceStore.getState();
      if (e.code === "KeyC") {
        // Leave regular text copying (e.g. from a note preview) alone.
        if ((window.getSelection()?.toString() ?? "") !== "") return;
        const selected = nodes.filter(
          (n) => n.selected === true && n.type === "terminal",
        );
        if (selected.length === 0) return;
        e.preventDefault();
        clipboard = selected.map((n) => ({ id: n.id, rect: nodeRect(n) }));
        return;
      }

      if (clipboard.length === 0) return;
      e.preventDefault();
      if (!e.repeat) pasteTerminals(clipboard);
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}
