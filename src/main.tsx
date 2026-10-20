import ReactDOM from "react-dom/client";
import App from "./App";
import { loadSettings } from "./store/settings";

// Suppress the webview's native context menu (Reload / Inspect / AutoFill) —
// right-click is reserved for canvas interactions.
window.addEventListener("contextmenu", (e) => e.preventDefault());

void loadSettings();

// No StrictMode: terminal effects spawn real PTY processes; double-invoked
// effects would create/kill each session twice.
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <App />,
);
