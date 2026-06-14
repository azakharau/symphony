---
name: debug
description:
  Investigate stuck runs and execution failures by tracing the Rust Symphony
  service, OpenCode ACP sessions, worktrees, and dashboard/API state; use when
  runs stall, retry repeatedly, or fail unexpectedly.
---

# Debug

## Goals

- Find why a run is stuck, retrying, or failing.
- Correlate a Linear issue to the Rust Symphony runtime state, OpenCode ACP
  session metadata, and the per-issue worktree.
- Read service logs, dashboard/API state, and workspace evidence in the right
  order to isolate root cause.

## Runtime Model

- Primary runtime: Rust `symphony` service from `crates/symphony`.
- Process manager: systemd user service, usually
  `openai-symphony-symphony.service`.
- Implementation runner: OpenCode ACP (`opencode acp`) launched from an
  isolated per-issue git worktree.
- Runtime state: SQLite database configured for the service, plus OpenCode
  session records and per-issue worktree contents.
- Operator surface: dashboard/API endpoints such as `/api/dashboard` and
  `/api/projects/<project>`.

## Correlation Keys

- `issue_identifier`: human ticket key (example: `MT-625`).
- `issue_id`: Linear UUID (stable internal ID).
- `session_id`: OpenCode ACP session identifier recorded by Symphony.
- `worktree_path`: per-issue git worktree used as the ACP process `cwd`.
- `project`: Symphony project key from `config/symphony.projects.toml`.

Use these fields as join keys across journal logs, dashboard/API responses,
SQLite-backed runtime state, and the worktree.

## Quick Triage (Stuck Run)

1. Confirm service health with systemd.
2. Check the dashboard/API for the project and issue state.
3. Search recent journal lines for `issue_identifier` first, then `issue_id` if
   needed.
4. Extract the `session_id` and `worktree_path` from matching lines or API
   state.
5. Trace that `session_id` across ACP launch, prompt, streaming, completion,
   failure, and stall/retry handling.
6. Decide class of failure: service stopped, config/store failure, Linear/API
   issue, OpenCode ACP startup/resume failure, turn failure, worktree/git
   failure, evaluator/review loop, or scheduler capacity/backoff.

## Commands

```bash
# 1) Check the user service state
systemctl --user status openai-symphony-symphony.service

# 2) Follow live runtime logs
journalctl --user-unit openai-symphony-symphony.service -f

# 3) Read recent service logs without following
journalctl --user-unit openai-symphony-symphony.service --since "2 hours ago" --no-pager

# 4) Check dashboard/API state
curl -fsS http://127.0.0.1:4115/api/dashboard
curl -fsS http://127.0.0.1:4115/api/projects/symphony

# 5) Narrow logs by ticket key or Linear UUID
journalctl --user-unit openai-symphony-symphony.service --since "24 hours ago" --no-pager \
  | rg -n "issue_identifier=MT-625|MT-625"
journalctl --user-unit openai-symphony-symphony.service --since "24 hours ago" --no-pager \
  | rg -n "issue_id=<linear-uuid>"

# 6) Pull OpenCode ACP session identifiers and worktree paths from the slice
journalctl --user-unit openai-symphony-symphony.service --since "24 hours ago" --no-pager \
  | rg -o "session_id=[^ ;]+|worktree_path=[^ ;]+" | sort -u

# 7) Trace one OpenCode ACP session end-to-end
journalctl --user-unit openai-symphony-symphony.service --since "24 hours ago" --no-pager \
  | rg -n "session_id=<opencode-acp-session-id>"

# 8) Focus on stuck/retry/failure signals
journalctl --user-unit openai-symphony-symphony.service --since "24 hours ago" --no-pager \
  | rg -n "stalled|retry|backoff|turn_timeout|turn_failed|turn_cancelled|ACP|opencode|worktree|evaluator|review"
```

## Investigation Flow

1. Establish whether the service is running:
   - `active (running)` means continue with issue/session triage.
   - Restart only inside an operator-approved safe restart window.
   - If the unit is missing or failed, inspect the full journal for config,
     database, environment, or binary path errors.
2. Check operator API state:
   - Use `/api/dashboard` for global scheduler and capacity symptoms.
   - Use `/api/projects/<project>` for project-specific issues, active runs,
     worktree paths, branch names, and recorded OpenCode session metadata.
3. Locate the issue slice:
   - Search by `issue_identifier=<KEY>` or the plain ticket key.
   - If noise is high, add `issue_id=<UUID>`.
4. Establish timeline:
   - Find worktree creation or reuse.
   - Find OpenCode ACP process launch.
   - Follow `initialize`, `session/new` or `session/resume`, config option
     application, prompt send, stream events, and terminal result.
5. Classify the problem:
   - Service/config/store: systemd failure, missing env file, invalid project
     config, database open/migration error.
   - Linear/API: missing issue, status transition failure, rate limit, auth
     error.
   - Worktree/git: branch checkout, dirty worktree, missing path, push/pull
     conflict, cleanup blocker.
   - OpenCode ACP: subprocess launch failure, initialize failure, resume
     mismatch, prompt/stream timeout, terminal turn error.
   - Review/evaluator loop: implementation completed but review or evaluator
     requires repair.
   - Scheduler/backoff: capacity limit, retry window, parked issue, or repeated
     stall recovery.
6. Validate scope:
   - Check whether failures are isolated to one issue/session or repeating
     across multiple tickets/projects.
7. Capture evidence:
   - Save key journal/API lines with timestamps, `issue_identifier`, `issue_id`,
     `session_id`, `worktree_path`, and failing stage.
   - Record probable root cause and the exact next operator action.

## Reading OpenCode ACP Session Logs

OpenCode ACP diagnostics are emitted through the Rust Symphony service journal
and keyed by `session_id` when available. Read them as a lifecycle:

1. Worktree selected or created for the issue.
2. ACP subprocess launched with the worktree as `cwd`.
3. `initialize` response received.
4. `session/new` for a fresh run or `session/resume` for a persisted run.
5. `session/set_config_option` entries for mode/model/effort.
6. `session/prompt` sent.
7. Stream/progress events.
8. Terminal result, evaluator/review handoff, retry/backoff, or failure.

For one specific session investigation, keep the trace narrow:

1. Capture one `session_id` and `worktree_path` for the ticket.
2. Build a timestamped journal slice for only that session:
   - `journalctl --user-unit openai-symphony-symphony.service --since "24 hours ago" --no-pager | rg -n "session_id=<opencode-acp-session-id>"`
3. Mark the exact failing stage:
   - Startup failure before `initialize`.
   - Session creation/resume/config failure before prompt.
   - Turn/runtime failure after prompt or stream events.
   - Review/evaluator repair loop after implementation output.
   - Stall recovery or scheduler backoff.
4. Pair findings with `issue_identifier`, `issue_id`, and `worktree_path` from
   nearby lines or API output to avoid mixing concurrent retries.

## Notes

- Prefer `rg` over `grep` for speed when filtering journal or exported logs.
- Do not restart or stop user services unless the operator explicitly approves a
  safe restart window.
- Treat the dashboard/API and systemd journal as primary runtime evidence; local
  files under a per-issue worktree are supporting evidence for code state.
- If a log line lacks the fields needed to join issue, session, and worktree
  state, capture the missing field as an observability gap in the debug result.
