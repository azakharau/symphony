# Symphony Service Specification

Status: Rust/OpenCode-only Symphony contract

Purpose: Define the active Symphony runtime that orchestrates project work through the Rust
`symphony` binary and OpenCode ACP sessions.

## Runtime Contract

- The active service implementation is the Rust crate at `crates/symphony`.
- OpenCode ACP is the only implementation runner. The runtime launches `/usr/local/bin/opencode acp`
  from an isolated per-issue worktree.
- Codex runner integration is not an active runtime path.
- The removed Elixir implementation is not a compatibility target.
- Configuration is loaded from a root multiproject TOML file. Configuration contains generic
  project/runtime policy only; it must not encode active milestone, active issue, or lifecycle
  anchors.
- Per-project policy may point at a repository `WORKFLOW.md`, but lifecycle ownership is enforced by
  the Rust runtime and OpenCode handoff contract, not by legacy workflow-local state aliases.

## OpenCode ACP Launch Contract

- `opencode.command` and `opencode.args` are explicit project config fields; the checked-in
  Symphony project config sets `/usr/local/bin/opencode` with `["acp"]`.
- `opencode acp` is an ACP subprocess transport: Symphony writes one JSON-RPC object per line to
  stdin and reads newline-delimited JSON responses/events from stdout.
- The launch sequence is deterministic: create the per-issue git worktree, start OpenCode with that
  worktree as `cwd`, send `initialize`, send `session/new`, apply `session/set_config_option` for
  `mode`, `model`, and optional `effort`, then send `session/prompt`.
- `session/new` response `configOptions` is the runtime source of truth for configurable model,
  mode, and effort selectors. Symphony must not assume that putting `model` in `session/new` applies
  the selected model.
- The default checked-in OpenCode policy for this project is `agent: build`, `model:
  openai/gpt-5.5`, `effort: high`, and unattended permission rejection.
- OpenCode sessions run only inside `branch.worktree_root/<issue identifier>`. Completion cleanup is
  allowed only when the handoff worktree exactly matches the active session worktree.
- OpenCode ACP process identity is runtime state. If Symphony observes an `In Progress` issue with
  a persisted session id but no live OpenCode ACP process, it starts a new ACP transport and calls
  `session/resume` for that session id. It must not call `session/new` or replay the original task
  prompt for restart recovery.
- OpenCode activity and cost telemetry are derived from the OpenCode SQLite session tree:
  the root ACP session plus direct child sessions count as one issue execution. Symphony reads
  messages, parts, todos, token counters, cost, active agent/model, and subagent count from that
  persisted tree, so missing incremental ACP stream events do not produce false silence.

## Mnemesh Evidence Workspace Contract

- Each enabled project must provide `[projects.mnemesh].workspace_root` in the root TOML config.
- The workspace root is the canonical project root, for example `/home/agent/proj/mnemesh`, not the
  isolated per-issue OpenCode worktree.
- Symphony passes that global workspace root into every OpenCode task prompt. OpenCode must use it
  for Mnemesh MCP calls, observations, claims, evidence, verification, and handoff records.
- Symphony must not start an OpenCode runner when the Mnemesh workspace root is missing from project
  config. It parks the issue in `Need Owner Input` with `provider_blocker` evidence instead.
- OpenCode must not create or register a separate Mnemesh workspace for an issue worktree. If the
  global project workspace is unavailable, OpenCode stops with a provider blocker rather than
  continuing with local degraded evidence.

## Issue Lifecycle

Symphony uses this executable lifecycle:

- `Backlog`: planning inventory only. Existing runtime rows may be reconciled, but backlog issues are
  not dispatched.
- `Todo`: queued executable work. Nonterminal blockers keep the issue blocked; otherwise the runtime
  moves eligible work to `In Progress` when capacity is available.
- `In Progress`: OpenCode-owned implementation session. Symphony records the ACP session, observes
  handoff evidence, and keeps repair loops in this state until closure or a typed blocker appears.
- `Need Owner Input`: parked state for owner questions, provider blockers, malformed handoffs, and
  repeated identical eval failures.
- Terminal states: `Done`, `Canceled`, `Cancelled`, `Closed`, and `Duplicate`.

Legacy steward states such as `Preparing`, `In Review`, and `RCA Required` are not executable runtime
states in Symphony. If the Rust runtime sees them in the active queue, it parks the issue in
`Need Owner Input` with typed evidence instead of preserving hidden compatibility aliases.

## OpenCode Handoff

OpenCode completion is accepted only from structured git-closure evidence:

- Matching session id.
- Passing eval results.
- Changed-file evidence.
- Git metadata with branch, pushed commit SHA, and worktree path.
- Optional PR URL.
- Risk summary.

Successful handoffs move the issue to `Done` only after Symphony verifies that the issue worktree
commit is pushed and integrated into the configured base branch. Linear comments alone are not
closure evidence for any task that changes tracked repository state, including documentation or audit
artifacts. After verified closure, Symphony persists git metadata and removes the completed per-issue
worktree immediately. Eval failures stay in the OpenCode repair loop until they pass or hit the
configured repeated-fingerprint policy.

## Multiproject Runtime

The root config contains one or more projects with:

- Linear team/project identity.
- Repository path and branch/worktree policy.
- Mnemesh project evidence workspace root.
- OpenCode command, args, agent, model, optional effort, and permission policy.
- Eval defaults.
- Per-project concurrency.
- Optional root `opencode_storage` with the OpenCode SQLite database path and local archive root.

Only enabled projects are reconciled. Work is ordered by Linear priority, then identifier, then
issue id. Running sessions consume project capacity and survive restarts through the SQLite runtime
store.

## Runtime Cleanup

Runtime cleanup removes stale completed runtime rows after the configured retention window. If an
issue has a recorded OpenCode session, cleanup first exports the OpenCode root+subagent session tree
to a local archive under `opencode_storage.archive_root`, then deletes the corresponding OpenCode
SQLite rows and finally removes the runtime session row. Raw transcript payloads may be retained in
the local archive for operator debugging, but committed benchmark/report artifacts must use derived
metrics only and must not commit raw prompt, response, or transcript bodies.

## Dashboard/API Projection

The Rust runtime exposes stable read-model builders for:

- `/api/dashboard`
- `/api/projects/{project_id}`
- `/api/projects/{project_id}/issues/{issue_id}`

These projections report project activity, parked and terminal counts, runner health, capacity,
cleanup state, Linear state, Symphony lifecycle state, blocker/failure details, OpenCode session
metadata, eval results, git refs, worktree paths, token/cost counters, subagent count/activity, and
display status.

## Operator Validation

Local verification:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Host-dependent live OpenCode smoke:

```bash
SYMPHONY_LIVE_OPENCODE_ACP=1 cargo test -p symphony --test bootstrap \
  installed_opencode_acp_supports_ndjson_config_options_without_prompting -- --nocapture
```

Live cutover verification:

```bash
cargo build --release -p symphony
cargo run -p symphony -- validate-config --config config/symphony.projects.toml
cargo run -p symphony -- daemon --config config/symphony.projects.toml --database /home/agent/.symphony/symphony/runtime.sqlite3
/usr/local/bin/opencode acp
systemctl --user status symphony.service
curl -fsS http://127.0.0.1:4115/api/dashboard
curl -fsS http://127.0.0.1:4115/api/projects/symphony
curl -fsS http://127.0.0.1:4115/api/projects/mnemesh
```

The live commands require host service access, Linear credentials, OpenCode availability, and the
operator-approved systemd deployment. Continuous daemon mode reads `LINEAR_API_KEY` from the host
environment file `/home/agent/.symphony/env/linear.env` before starting the poll loop.
