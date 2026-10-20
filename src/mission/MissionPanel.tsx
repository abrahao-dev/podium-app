import { useMemo, useState } from "react";
import { Panel } from "@xyflow/react";
import { useMissions, type Mission, type TaskStatus } from "./missions";

const STATUS_LABEL: Record<TaskStatus, string> = {
  pending: "pending",
  ready: "queued",
  starting: "starting",
  running: "running",
  merging: "merging",
  done: "merged",
  failed: "failed",
  needsHuman: "needs you",
  cancelled: "cancelled",
};

/** Missions shown: running ones, plus the latest finished one. */
function visibleMissions(all: Record<string, Mission>): Mission[] {
  const missions = Object.values(all).sort((a, b) => b.createdMs - a.createdMs);
  const live = missions.filter(
    (m) => m.state === "preparing" || m.state === "active",
  );
  if (live.length > 0) return live;
  return missions.slice(0, 1);
}

function MissionCard({ mission }: { mission: Mission }) {
  const done = mission.tasks.filter((t) => t.status === "done").length;
  const total = mission.tasks.length;
  return (
    <section className="mission">
      <header className="mission__header">
        <span className="mission__title" title={mission.integrationBranch}>
          {mission.title}
        </span>
        <span className={`mission__state mission__state--${mission.state}`}>
          {mission.state}
        </span>
      </header>
      <div className="mission__progress" aria-label={`${done} of ${total} merged`}>
        <div
          className="mission__progress-bar"
          style={{ width: total === 0 ? 0 : `${(done / total) * 100}%` }}
        />
      </div>
      <ul className="mission__tasks">
        {mission.tasks.map((task) => (
          <li
            key={task.id}
            className="mission__task"
            title={task.lastError ?? task.summary ?? task.title}
          >
            <span className="mission__task-id">{task.id}</span>
            <span className="mission__task-title">{task.title}</span>
            <span className={`mission__pill mission__pill--${task.status}`}>
              {STATUS_LABEL[task.status]}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function MissionPanel() {
  const all = useMissions();
  const missions = useMemo(() => visibleMissions(all), [all]);
  const [open, setOpen] = useState(true);
  if (missions.length === 0) return null;
  const live = missions.some(
    (m) => m.state === "preparing" || m.state === "active",
  );

  return (
    <Panel position="bottom-right" className="missions nowheel">
      <button
        type="button"
        className="missions__toggle"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <span
          className={`missions__dot${live ? " missions__dot--live" : ""}`}
        />
        Missions
        <span className="missions__chevron">{open ? "▾" : "▴"}</span>
      </button>
      {open && (
        <div className="missions__list">
          {missions.map((mission) => (
            <MissionCard key={mission.id} mission={mission} />
          ))}
        </div>
      )}
    </Panel>
  );
}
