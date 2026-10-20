import type { AgentSession } from "./pty";

/** Session ids come from files the agents write; anything outside this set is
 *  dropped instead of being typed into a shell. */
const SAFE_SESSION_ID = /^[A-Za-z0-9_-]{1,128}$/;

/** Shell command that reopens an agent session, or its "continue the last
 *  session in this directory" form when the id is unknown. */
export function resumeCommand(session: AgentSession): string {
  const id =
    session.sessionId !== null && SAFE_SESSION_ID.test(session.sessionId)
      ? session.sessionId
      : null;
  switch (session.kind) {
    case "claude":
      return id ? `claude --resume ${id}` : "claude --continue";
    case "codex":
      return id ? `codex resume ${id}` : "codex resume --last";
    case "opencode":
      return id ? `opencode --session ${id}` : "opencode --continue";
  }
}

export function sameAgentSession(
  a: AgentSession | undefined,
  b: AgentSession | undefined,
): boolean {
  if (a === undefined || b === undefined) return a === b;
  return a.kind === b.kind && a.sessionId === b.sessionId;
}
