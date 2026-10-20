import ReactDOM from "react-dom/client";
import App from "./App";
import { loadSettings } from "./store/settings";
import { setUiVisible } from "./lib/pty";

// Suppress the webview's native context menu (Reload / Inspect / AutoFill) —
// right-click is reserved for canvas interactions.
window.addEventListener("contextmenu", (e) => e.preventDefault());

void loadSettings();

// E2E hooks for the dev build (driven over CDP); stripped from production.
if (import.meta.env.DEV) {
  void import("./store/workspace").then(({ useWorkspaceStore }) => {
    Object.assign(window, { __podium: { workspace: useWorkspaceStore } });
  });
}

// Hidden webviews throttle timers, so xterm would fall behind busy agents;
// the backend holds output meanwhile and restores every screen on return.
document.addEventListener("visibilitychange", () => {
  void setUiVisible(!document.hidden).catch(() => {});
});

// No StrictMode: terminal effects spawn real PTY processes; double-invoked
// effects would create/kill each session twice.
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <App />,
);
