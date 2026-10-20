/**
 * Restored terminals are started one at a time instead of all in the first
 * frame: every start spawns a shell, and resuming an agent is a heavy process
 * launch on top of it.
 */
const FIRST_LAUNCH_DELAY_MS = 800;
const SHELL_GAP_MS = 250;
const AGENT_GAP_MS = 1200;

interface LaunchJob {
  heavy: boolean;
  run: () => void;
}

const queue: LaunchJob[] = [];
let timer: ReturnType<typeof setTimeout> | undefined;
let nextAt: number | null = null;

function pump(): void {
  if (timer !== undefined || queue.length === 0) return;
  nextAt ??= performance.now() + FIRST_LAUNCH_DELAY_MS;
  timer = setTimeout(
    () => {
      timer = undefined;
      const job = queue.shift();
      if (job) {
        job.run();
        nextAt = performance.now() + (job.heavy ? AGENT_GAP_MS : SHELL_GAP_MS);
      }
      pump();
    },
    Math.max(0, nextAt - performance.now()),
  );
}

/** Queues a launch; the returned function cancels it if it hasn't run yet. */
export function enqueueLaunch(heavy: boolean, run: () => void): () => void {
  const job: LaunchJob = { heavy, run };
  queue.push(job);
  pump();
  return () => {
    const index = queue.indexOf(job);
    if (index !== -1) queue.splice(index, 1);
  };
}
