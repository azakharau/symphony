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
- OpenCode activity and billing telemetry are derived from the OpenCode SQLite session tree:
  the root ACP session plus direct child sessions count as one issue execution. Symphony reads
  messages, parts, todos, token counters, billing counters, active agent/model, and subagent count from that
  persisted tree, so missing incremental ACP stream events do not produce false silence.

## Mnemesh Evidence Workspace Contract

- Each enabled project must provide `[projects.mnemesh].workspace_root` in the root TOML config.
- The workspace root is the canonical project root, for example `/home/agent/proj/mnemesh`, not the
  isolated per-issue OpenCode worktree.
- Symphony passes that global workspace root into every OpenCode task prompt. OpenCode must use it
  for Mnemesh MCP calls, observations, claims, evidence, verification, and handoff records.
- Symphony must not start an OpenCode runner when the Mnemesh workspace root is missing from project
  config. It records typed `provider_blocker` evidence without classifying the blocker as
  `Need Owner Input`.
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
- `Need Owner Input`: parked state only for real owner/product/permission questions that require a
  human decision before OpenCode can continue.
- Terminal states: `Done`, `Canceled`, `Cancelled`, `Closed`, and `Duplicate`.

Legacy steward states such as `Preparing`, `In Review`, and `RCA Required` are not executable runtime
states in Symphony. If the Rust runtime sees them in the active queue, it parks the issue with typed
evidence instead of preserving hidden compatibility aliases.

Runtime triage keeps owner questions separate from system failures and self-reference bugs:

- Provider or infrastructure blockers are typed provider evidence, not owner input.
- Eval failures stay in the OpenCode repair loop until they pass or hit the typed repeated-fingerprint
  policy.
- Runtime/tooling defects such as missing or malformed handoff sidecars, stale OpenCode
  process/session evidence, git closure mismatches, cleanup failures, prompt-policy regressions, or
  evaluator contract failures route to bounded repair, typed runtime-defect blocked evidence, or an
  auto-created Symphony self-reference bug.
- Self-reference issues are `SYM-*` bugs about Symphony runtime, prompt, evaluator, worktree,
  Mnemesh, Linear, or git-closure behavior. The defect taxonomy is: runtime defect,
  orchestration defect, OpenCode implementation failure, product issue blocker, provider/infra
  blocker, owner question, eval failure, and cleanup failure. Classifier, model, and evaluator output
  is advisory only; only the deterministic runtime policy and Linear writer may create or mutate Linear
  issues.
- Symphony creates a self-reference bug for reproducible runtime/tooling defects, failed deterministic
  invariants, or broken generated handoff/prompt contracts. It does not create one for owner, product,
  permission, or acceptance-criteria questions; those use `Need Owner Input` only when a human decision
  is actually required.
- Auto-created P0 self-reference bugs may enter executable `Todo` when a bounded repair can run and
  the runtime cannot safely advance or close active work. P1 degraded project paths and P2 non-blocking
  hardening or follow-up default to `Backlog` unless explicitly escalated by hard policy.
- If an active `SYM-*` issue exposes a Symphony defect that would make Symphony wait on or requeue the
  same active issue, Symphony must not create a self-deadlock; it parks the active issue with typed
  runtime-defect/provider evidence and creates or links a separate self-reference bug instead.
- Runtime/tooling defects must not be requeued to executable `Todo` as product work.

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
worktree immediately. Eval failures stay in the OpenCode repair loop until they pass or hit the typed
repeated-fingerprint policy.

## Multiproject Runtime

The root config contains one or more projects with:

- Linear team/project identity.
- Repository path and branch/worktree policy.
- Mnemesh project evidence workspace root.
- OpenCode command, args, agent, model, optional effort, and permission policy.
- Eval defaults.
- Per-project concurrency.
- Optional root `opencode_storage` with the OpenCode SQLite database path and local archive root.

Only enabled projects are reconciled. Work is ordered by explicit Linear blockers/dependencies, then
project milestone ordering, then Linear priority and stable issue identifiers. Description text may
explain sequencing intent, but it is not executable ordering authority. Running sessions consume
project capacity and survive restarts through the SQLite runtime store.

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
metadata, eval results, git refs, worktree paths, token and billing counters, subagent count/activity, and
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
