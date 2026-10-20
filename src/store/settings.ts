import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export type ResumeMode = "manual" | "auto";

export type Autonomy = "full" | "edits" | "ask";
export type AgentName = "claude" | "codex" | "opencode" | "shell";
/** Claude Code model alias for recruits; "" = the CLI's default. */
export type RecruitModel = "" | "opus" | "sonnet" | "haiku";

export interface OrchestrationSettings {
  autonomy: Autonomy;
  maxRecruits: number;
  minFreeMemoryMb: number;
  defaultAgent: AgentName;
  stuckMinutes: number;
  guardGit: boolean;
  recruitModel: RecruitModel;
  /** Install and update the `podium` skill for Claude Code and Codex. */
  syncSkills: boolean;
}

export interface PodiumSettings {
  sessions: { restoreAgentSessions: boolean; resumeMode: ResumeMode };
  terminal: { fontSize: number };
  shortcuts: { pasteWithCtrlV: boolean; newlineWithModifiedEnter: boolean };
  orchestration: OrchestrationSettings;
}

export const DEFAULT_SETTINGS: PodiumSettings = {
  sessions: { restoreAgentSessions: true, resumeMode: "manual" },
  terminal: { fontSize: 13 },
  shortcuts: { pasteWithCtrlV: true, newlineWithModifiedEnter: true },
  orchestration: {
    autonomy: "full",
    maxRecruits: 6,
    minFreeMemoryMb: 3000,
    defaultAgent: "claude",
    stuckMinutes: 15,
    guardGit: true,
    recruitModel: "sonnet",
    syncSkills: true,
  },
};

export const AGENT_NAMES: readonly AgentName[] = ["claude", "codex", "opencode", "shell"];
const AUTONOMY_LEVELS: readonly Autonomy[] = ["full", "edits", "ask"];
const RECRUIT_MODELS: readonly RecruitModel[] = ["", "opus", "sonnet", "haiku"];

export const FONT_SIZE_MIN = 10;
export const FONT_SIZE_MAX = 20;

const SETTINGS_VERSION = 1;
const SAVE_DEBOUNCE_MS = 300;

interface SettingsState {
  settings: PodiumSettings;
  loaded: boolean;
  update<K extends keyof PodiumSettings>(
    category: K,
    patch: Partial<PodiumSettings[K]>,
  ): void;
}

export const useSettingsStore = create<SettingsState>()((set) => ({
  settings: DEFAULT_SETTINGS,
  loaded: false,
  update: (category, patch) =>
    set((state) => ({
      settings: {
        ...state.settings,
        [category]: { ...state.settings[category], ...patch },
      },
    })),
}));

export function clampFontSize(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_SETTINGS.terminal.fontSize;
  return Math.min(FONT_SIZE_MAX, Math.max(FONT_SIZE_MIN, Math.round(value)));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readBoolean(
  source: Record<string, unknown>,
  key: string,
  fallback: boolean,
): boolean {
  const value = source[key];
  return typeof value === "boolean" ? value : fallback;
}

function readNumber(
  source: Record<string, unknown>,
  key: string,
  fallback: number,
  min: number,
  max: number,
): number {
  const value = source[key];
  if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, Math.round(value)));
}

function readCategory(
  root: Record<string, unknown>,
  key: keyof PodiumSettings,
): Record<string, unknown> {
  const value = root[key];
  return isRecord(value) ? value : {};
}

/** Deep-merges persisted JSON onto the defaults; anything missing or
 *  malformed falls back to its default value. */
function parseSettings(raw: unknown): PodiumSettings {
  const root = isRecord(raw) ? raw : {};
  const sessions = readCategory(root, "sessions");
  const terminal = readCategory(root, "terminal");
  const shortcuts = readCategory(root, "shortcuts");
  const orchestration = readCategory(root, "orchestration");
  const defaults = DEFAULT_SETTINGS;
  const autonomy = orchestration.autonomy;
  const defaultAgent = orchestration.defaultAgent;
  const recruitModel = orchestration.recruitModel;

  const resumeMode = sessions.resumeMode;
  const fontSize = terminal.fontSize;

  return {
    sessions: {
      restoreAgentSessions: readBoolean(
        sessions,
        "restoreAgentSessions",
        defaults.sessions.restoreAgentSessions,
      ),
      resumeMode:
        resumeMode === "manual" || resumeMode === "auto"
          ? resumeMode
          : defaults.sessions.resumeMode,
    },
    terminal: {
      fontSize:
        typeof fontSize === "number"
          ? clampFontSize(fontSize)
          : defaults.terminal.fontSize,
    },
    shortcuts: {
      pasteWithCtrlV: readBoolean(
        shortcuts,
        "pasteWithCtrlV",
        defaults.shortcuts.pasteWithCtrlV,
      ),
      newlineWithModifiedEnter: readBoolean(
        shortcuts,
        "newlineWithModifiedEnter",
        defaults.shortcuts.newlineWithModifiedEnter,
      ),
    },
    orchestration: {
      autonomy: AUTONOMY_LEVELS.includes(autonomy as Autonomy)
        ? (autonomy as Autonomy)
        : defaults.orchestration.autonomy,
      maxRecruits: readNumber(
        orchestration,
        "maxRecruits",
        defaults.orchestration.maxRecruits,
        1,
        12,
      ),
      minFreeMemoryMb: readNumber(
        orchestration,
        "minFreeMemoryMb",
        defaults.orchestration.minFreeMemoryMb,
        0,
        65536,
      ),
      defaultAgent: AGENT_NAMES.includes(defaultAgent as AgentName)
        ? (defaultAgent as AgentName)
        : defaults.orchestration.defaultAgent,
      stuckMinutes: readNumber(
        orchestration,
        "stuckMinutes",
        defaults.orchestration.stuckMinutes,
        2,
        240,
      ),
      guardGit: readBoolean(
        orchestration,
        "guardGit",
        defaults.orchestration.guardGit,
      ),
      recruitModel: RECRUIT_MODELS.includes(recruitModel as RecruitModel)
        ? (recruitModel as RecruitModel)
        : defaults.orchestration.recruitModel,
      syncSkills: readBoolean(
        orchestration,
        "syncSkills",
        defaults.orchestration.syncSkills,
      ),
    },
  };
}

let loadPromise: Promise<void> | null = null;

/** Loads persisted settings once. Never throws: any failure (no Tauri
 *  runtime, unreadable file, invalid JSON) leaves the defaults in place. */
export function loadSettings(): Promise<void> {
  if (!loadPromise) {
    loadPromise = (async () => {
      let settings = DEFAULT_SETTINGS;
      try {
        const data = await invoke<string | null>("load_settings");
        if (data) settings = parseSettings(JSON.parse(data) as unknown);
      } catch (err) {
        console.error("Failed to load settings:", err);
      }
      useSettingsStore.setState({ settings, loaded: true });
    })();
  }
  return loadPromise;
}

let saveTimer: ReturnType<typeof setTimeout> | null = null;

function persist(settings: PodiumSettings): void {
  const data = JSON.stringify({ version: SETTINGS_VERSION, ...settings }, null, 2);
  invoke("save_settings", { data }).catch((err: unknown) => {
    console.error("Failed to save settings:", err);
  });
}

useSettingsStore.subscribe((state, prev) => {
  // Skip the initial load transition and anything before it.
  if (!state.loaded || !prev.loaded || state.settings === prev.settings) return;
  if (saveTimer !== null) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = null;
    persist(useSettingsStore.getState().settings);
  }, SAVE_DEBOUNCE_MS);
});

// Flush a pending save if the window closes within the debounce interval.
window.addEventListener("beforeunload", () => {
  if (saveTimer === null) return;
  clearTimeout(saveTimer);
  saveTimer = null;
  persist(useSettingsStore.getState().settings);
});
