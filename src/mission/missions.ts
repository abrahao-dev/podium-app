import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";

export type TaskStatus =
  | "pending"
  | "ready"
  | "starting"
  | "running"
  | "merging"
  | "done"
  | "failed"
  | "needsHuman"
  | "cancelled";

export type MissionState = "preparing" | "active" | "stopped" | "completed";

export interface MissionTask {
  id: string;
  title: string;
  status: TaskStatus;
  workerName: string | null;
  attempts: number;
  summary: string | null;
  lastError: string | null;
  deps: string[];
  files: string[];
}

export interface Mission {
  id: string;
  title: string;
  orchestrator: string;
  integrationBranch: string;
  state: MissionState;
  createdMs: number;
  tasks: MissionTask[];
  log: string[];
}

/** Missions by id, pushed by the backend on every change. */
export const useMissions = create<Record<string, Mission>>(() => ({}));

let started = false;

export function startMissionSync(): void {
  if (started) return;
  started = true;
  void listen<Mission>("mission-update", (event) => {
    useMissions.setState({ [event.payload.id]: event.payload });
  });
  void invoke<Mission[]>("bridge_missions")
    .then((missions) =>
      useMissions.setState(
        Object.fromEntries(missions.map((m) => [m.id, m])),
      ),
    )
    .catch(() => {});
}

/** Hands a note's brief to the orchestrator wired to it; resolves to the
 *  orchestrator's name. */
export function runNoteAsMission(noteId: string): Promise<string> {
  return invoke<string>("bridge_run_note", { noteId });
}
