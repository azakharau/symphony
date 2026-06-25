import type { IssueDetail, RunnerSession } from "@/src/types";

const DISPLAY_ACTIVE_STAGES = new Set(["starting", "running", "eval", "review", "handoff", "silent"]);

export function currentRunnerSession(issue: IssueDetail): RunnerSession | undefined {
  const preferredSessionId = issue.preferred_runner_session_id;
  if (preferredSessionId) {
    const preferred = issue.runner_sessions.find((session) => session.runner_session_id === preferredSessionId);
    if (preferred) return preferred;
  }

  return issue.runner_sessions.find(isActiveForDisplay) ?? issue.runner_sessions[0];
}

function isActiveForDisplay(session: RunnerSession): boolean {
  return session.lifecycle_stage === "running" && DISPLAY_ACTIVE_STAGES.has(session.current_stage);
}
