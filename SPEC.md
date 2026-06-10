# Symphony vNext Service Specification

Status: Rust/OpenCode-only vNext contract

Purpose: Define the active Symphony runtime that orchestrates project work through the Rust
`symphony-vnext` binary and OpenCode ACP sessions.

## Runtime Contract

- The active service implementation is the Rust crate at `crates/symphony-vnext`.
- OpenCode ACP is the only implementation runner. The runtime launches `/usr/local/bin/opencode acp`
  from an isolated per-issue worktree.
- Codex runner integration is not an active runtime path.
- The removed Elixir implementation is not a compatibility target.
- Configuration is loaded from a root multiproject YAML file such as
  `config/symphony.projects.yml`.
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

## Issue Lifecycle

Rust vNext uses this executable lifecycle:

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
states in vNext. If the Rust runtime sees them in the active queue, it parks the issue in
`Need Owner Input` with typed evidence instead of preserving hidden compatibility aliases.

## OpenCode Handoff

OpenCode completion is accepted only from structured git-closure evidence:

- Matching session id.
- Passing eval results.
- Changed-file evidence.
- Git metadata with branch, commit SHA, and worktree path.
- Optional PR URL.
- Risk summary.

Successful handoffs move the issue to `Done`, persist git closure metadata, and remove the completed
per-issue worktree. Eval failures stay in the OpenCode repair loop until they pass or hit the
configured repeated-fingerprint policy.

## Multiproject Runtime

The root config contains one or more projects with:

- Linear team/project/milestone identity.
- Repository path and branch/worktree policy.
- OpenCode command, args, agent, model, optional effort, and permission policy.
- Eval defaults.
- Per-project concurrency.

Only enabled projects are reconciled. Work is ordered by Linear priority, then identifier, then
issue id. Running sessions consume project capacity and survive restarts through the SQLite runtime
store.

## Dashboard/API Projection

The Rust runtime exposes stable read-model builders for:

- `/api/dashboard`
- `/api/projects/{project_id}`
- `/api/projects/{project_id}/issues/{issue_id}`

These projections report project activity, parked and terminal counts, runner health, capacity,
cleanup state, Linear state, vNext lifecycle state, blocker/failure details, OpenCode session
metadata, eval results, git refs, worktree paths, token/cost counters, and display status.

## Operator Validation

Local verification:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Host-dependent live OpenCode smoke:

```bash
SYMPHONY_VNEXT_LIVE_OPENCODE_ACP=1 cargo test -p symphony-vnext --test bootstrap \
  installed_opencode_acp_supports_ndjson_config_options_without_prompting -- --nocapture
```

Live cutover verification:

```bash
cargo build --release -p symphony-vnext
cargo run -p symphony-vnext -- validate-config --config config/symphony.projects.yml
cargo run -p symphony-vnext -- daemon --config config/symphony.projects.yml --database /var/lib/symphony-vnext/runtime.sqlite3
/usr/local/bin/opencode acp
systemctl status symphony-vnext.service
curl -fsS http://127.0.0.1:4110/api/dashboard
curl -fsS http://127.0.0.1:4110/api/projects/symphony
```

The live commands require host service access, Linear credentials, OpenCode availability, and the
operator-approved systemd deployment. Continuous daemon mode reads `LINEAR_API_KEY` from the host
environment file `/home/agent/.symphony/env/linear.env` before starting the poll loop.
