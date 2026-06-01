# Multiproject Symphony Orchestrator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current one-service-per-project runtime with one BEAM daemon, one dashboard/API, and supervised project workers driven by `/home/agent/.symphony/config/projects.yml`.

**Architecture:** Add a root project registry/config layer above the existing per-project `WORKFLOW.md` runtime. Keep the current orchestrator state machine as the per-project worker, but remove global singleton dependencies from project-scoped runtime paths by passing a `ProjectContext` or resolving through project-scoped process names. Treat both Codex and OpenCode as first-class runner integrations behind a shared runner adapter contract, with comparable lifecycle, session identity, observability, resume/reuse, and project-scoped configuration. Use OTP supervisors and registries for isolation; do not write a custom process manager.

**Tech Stack:** Elixir 1.19 / OTP 28, Phoenix/Bandit dashboard, Ecto changesets for config parsing, existing Linear/Codex/OpenCode adapters.

---

## Discovery Record

Current checkout:
- Repo: `/home/agent/proj/symphony`
- Elixir app: `/home/agent/proj/symphony/elixir`
- Branch: `agent-server/opencode-runner-extension`
- HEAD: `f310ec27edd53437750d89abd61761b29be2c86d`
- Branch status: ahead of `fork/agent-server/opencode-runner-extension` by 1 commit.
- Dirty files before this plan: `elixir/lib/symphony_elixir/agent_runner.ex`, `elixir/lib/symphony_elixir/orchestrator.ex`, `elixir/test/support/live_e2e_docker/docker-compose.yml`, `elixir/test/support/live_e2e_docker/live_worker_entrypoint.sh`, `elixir/test/symphony_elixir/opencode_runner_test.exs`, `elixir/test/symphony_elixir/workspace_and_config_test.exs`.

Prior multiproject worktree search:
- `git worktree list` found only:
  - `/home/agent/proj/symphony` on `agent-server/opencode-runner-extension`
  - `/home/agent/.symphony/worktrees/opencode-acp-first-class-runner` on `agent-server/opencode-acp-first-class-runner`
- `/home/agent/.symphony/worktrees` contains only `opencode-acp-first-class-runner`.
- `git branch -a` has no multiproject branch.
- `git stash list` is empty.
- `git reflog --all` and `git log --all --grep` found only `Add Linear project milestone dispatch` for project/milestone terms.
- `git fsck --unreachable` found three unreachable commits, all OpenCode ACP related, not multiproject.
- `/root` local search found no matching Symphony/project/multi directories.
- SSH aliases `agent-vps` and `agent-vps-root` are not configured in `/home/agent/.ssh/config` and did not resolve in this session.
- Direct `root@192.168.10.207` timed out on port 22 with `ConnectTimeout=5`.

Current runtime:
- Four systemd services exist and are active:
  - `openai-symphony-mnemesh.service` on port `4111`, workflow `/home/agent/proj/mnemesh/WORKFLOW.md`
  - `openai-symphony-nervure.service` on port `4112`, workflow `/home/agent/proj/nervure/WORKFLOW.md`
  - `openai-symphony-neryva.service` on port `4113`, workflow `/home/agent/proj/neryva/WORKFLOW.md`
  - `openai-symphony-agent-forge.service` on port `4114`, workflow `/home/agent/proj/neryva-agent-forge/WORKFLOW.md`
- `mnemesh` currently has an active OpenCode process for `NER-46`.
- `agent-forge` currently has an active synthetic milestone planning run.
- Do not stop or disable old services until all current runs finish and cutover is explicitly approved.

Validation run during discovery:
- `mix specs.check` passed.
- `mix test test/symphony_elixir/workspace_and_config_test.exs` passed: `65 tests, 0 failures`.
- `mix test test/symphony_elixir/opencode_runner_test.exs` passed: `14 tests, 0 failures`.

## Current Architecture Facts

- `SymphonyElixir.Application` starts one global `WorkflowStore`, one global `Orchestrator`, one global `TaskSupervisor`, one `HttpServer`, and one `StatusDashboard`.
- `SymphonyElixir.Workflow.workflow_file_path/0` reads a single app env path or defaults to `File.cwd!/WORKFLOW.md`.
- `SymphonyElixir.WorkflowStore` is globally named as `SymphonyElixir.WorkflowStore`.
- `SymphonyElixir.Orchestrator` can accept a custom name, but its runtime calls global `Config.settings!/0`, global `Tracker`, global `TaskSupervisor`, and global dashboard notification paths.
- `SymphonyElixir.Tracker.adapter/0` chooses the adapter from global `Config.settings!().tracker.kind`.
- `SymphonyElixir.AgentRunner` uses global `Config`, `Tracker`, `Workspace`, `PromptBuilder`, and runner config.
- OpenCode currently exists as `SymphonyElixir.OpenCode.Runner`, but the orchestration surface treats it as a branch inside `AgentRunner` rather than as a first-class runtime integration equivalent to Codex.
- `SymphonyElixirWeb.Router` currently exposes aggregate-looking routes that are actually single-orchestrator routes:
  - `GET /api/v1/state`
  - `POST /api/v1/refresh`
  - `GET /api/v1/:issue_identifier`
- `SymphonyElixirWeb.Presenter.state_payload/2` snapshots one orchestrator and returns single-project running/retrying/blocked lists.
- Existing smart-polling code already has project-milestone gating and owner-input pulse behavior inside the orchestrator. Preserve it by keeping one orchestrator state machine per project.

## Architecture Decision

Use one root supervisor tree:

```text
SymphonyElixir.Supervisor
  Phoenix.PubSub
  SymphonyElixir.RootConfigStore
  SymphonyElixir.ProjectRegistry
  DynamicSupervisor name: SymphonyElixir.ProjectSupervisor
    per enabled project:
      Supervisor id: {:project, project_id}
        WorkflowStore name: via(project_id, :workflow_store)
        Task.Supervisor name: via(project_id, :task_supervisor)
        Orchestrator name: via(project_id, :orchestrator)
        StatusDashboard.ProjectSink optional, no terminal rendering by default
  SymphonyElixir.HttpServer
  SymphonyElixir.StatusDashboard aggregate renderer
```

Root config is only for daemon/dashboard/project enablement:

```yaml
server:
  host: 127.0.0.1
  port: 4110

projects:
  - id: mnemesh
    name: Mnemesh
    enabled: true
    workflow_path: /home/agent/proj/mnemesh/WORKFLOW.md
    dashboard_order: 10
```

Per-project `WORKFLOW.md` remains the authority for tracker, states, runners, prompts, hooks, workspace root, worker hosts, retry, OpenCode/Codex config, and per-project concurrency.

Runner integration decision:
- Introduce a first-class runner adapter boundary rather than continuing to grow `AgentRunner` with runner-specific branches.
- Codex and OpenCode adapters both receive `ProjectContext`, issue, workspace, attempt/runtime opts, and an event sink.
- Codex adapter owns Codex app-server session lifecycle.
- OpenCode adapter owns OpenCode command/session lifecycle, attached server URL, session discovery/resume-result, handoff completeness checks, and review-decision gating.
- Both adapters emit normalized runner events for dashboard/API: `runner_kind`, `session_id`, `execution_dir`, `started_at`, `last_event`, `last_message`, `last_error`, token/usage fields when available, and handoff/exit status.
- OpenCode is not a subprocess detail hidden behind Codex semantics. It is a peer implementation path selected by repo-local `WORKFLOW.md` runner routes.

Disabled projects are represented in root state and dashboard/API as `paused`, but no project worker is started and no dispatch occurs.

Root reload changes project supervision dynamically:
- New `enabled: true` project starts a worker supervisor.
- Existing `enabled: true` project with changed metadata updates project context.
- Project changed to `enabled: false` is gracefully stopped and remains visible as paused.
- Invalid workflow blocks only that project and surfaces project `error`; the daemon and other projects continue.

## Migration Plan

1. Introduce root config parser/store and `ProjectContext` without changing existing single-project CLI behavior.
2. Start dynamic project supervisors for enabled projects using namespaced process names while keeping the old single-project runtime path working.
3. Refactor project-scoped modules away from global singleton config and tracker calls.
4. Replace dashboard/API projection with aggregate project state while keeping compatibility routes.
5. Tighten smart polling tests for owner-input and milestone gating inside per-project workers.
6. Add new systemd unit and cutover validation; do not execute cutover in implementation tasks.

## Task 1: Root Config And Project Context

**Repo path:** `/home/agent/proj/symphony/elixir`

**Branch/worktree:** Continue from `/home/agent/proj/symphony` on `agent-server/opencode-runner-extension` unless a dedicated implementation worktree is created by the coding system.

**Allowed files:**
- `lib/symphony_elixir/root_config.ex`
- `lib/symphony_elixir/root_config_store.ex`
- `lib/symphony_elixir/project_context.ex`
- `lib/symphony_elixir/project_registry.ex`
- `lib/symphony_elixir/cli.ex`
- `lib/symphony_elixir/config/schema.ex`
- `test/symphony_elixir/root_config_test.exs`
- `test/support/test_support.exs`
- `README.md`
- `WORKFLOW.md`

**Forbidden files:**
- `/etc/systemd/system/*`
- `/home/agent/proj/*/WORKFLOW.md`
- `lib/symphony_elixir/orchestrator.ex` except to add compatibility-free type references after tests require it.
- Generated build outputs under `_build`, `deps`, `log`.

- [x] Add `SymphonyElixir.RootConfig` that loads YAML from a path and parses `server.host`, `server.port`, and `projects`.
- [x] Validate project `id` with lower-case URL-safe identifiers, require unique ids, require `workflow_path`, and keep `enabled` default false.
- [x] Add `SymphonyElixir.ProjectContext` struct with `project_id`, `name`, `enabled`, `workflow_path`, `dashboard_order`, `logs_root`, and process names.
- [x] Add tests for valid multi-project config, duplicate project ids, missing workflow paths, disabled project visibility, and configured server port.
- [x] Extend CLI parsing so `--projects-config /home/agent/.symphony/config/projects.yml` parses root config without starting project workers, while the existing `[path-to-WORKFLOW.md]` single-project mode still works.

**Validation commands:**

```bash
mix format
mix specs.check
mix test test/symphony_elixir/root_config_test.exs
mix test test/symphony_elixir/workspace_and_config_test.exs
```

**Stop conditions:**
- Root config parsing requires live Linear credentials.
- Existing single-project CLI mode breaks.
- Root config defaults introduce a global project concurrency limit.

**Handoff requirements:**
- Report exact root config schema.
- Report compatibility behavior for existing `openai-symphony --port <port> <WORKFLOW.md>`.
- Include passing validation output.

## Task 2: Dynamic Project Supervisor

**Repo path:** `/home/agent/proj/symphony/elixir`

**Branch/worktree:** Same implementation branch/worktree as Task 1.

**Allowed files:**
- `lib/symphony_elixir.ex`
- `lib/symphony_elixir/project_supervisor.ex`
- `lib/symphony_elixir/root_config_store.ex`
- `lib/symphony_elixir/project_registry.ex`
- `lib/symphony_elixir/workflow_store.ex`
- `lib/symphony_elixir/workflow.ex`
- `test/symphony_elixir/project_supervisor_test.exs`
- `test/support/test_support.exs`

**Forbidden files:**
- `/etc/systemd/system/*`
- `lib/symphony_elixir/linear/*`
- `lib/symphony_elixir/opencode/*`
- `lib/symphony_elixir/codex/*`

- [x] Introduce a root-mode application child layout with `RootConfigStore`, `Registry`, and `DynamicSupervisor`.
- [x] Let each enabled project start a child supervisor containing its own `WorkflowStore`, `Task.Supervisor`, and paused `Orchestrator` names.
- [x] Update `WorkflowStore.start_link/1` to accept `:name` and `:workflow_path`; preserve old `WorkflowStore.current/0` compatibility for single-project mode.
- [x] Add root config reload handling that starts/stops project child supervisors without restarting the daemon.
- [x] Represent invalid workflow projects as errored project states instead of crashing root supervisor.
- [x] Add tests that root reload starts a new enabled project, stops a disabled project, and survives one project child crash.

**Validation commands:**

```bash
mix format
mix specs.check
mix test test/symphony_elixir/project_supervisor_test.exs
mix test test/symphony_elixir/workspace_and_config_test.exs
```

**Stop conditions:**
- A bad project workflow prevents the daemon from booting.
- Stopping one project kills other project workers.
- Reload requires full daemon restart.

**Handoff requirements:**
- Include supervisor tree summary.
- Include proof that disabled projects are not dispatching.
- Include proof that one child crash does not stop siblings.

## Task 3: Project-Scoped Config And Runtime

**Repo path:** `/home/agent/proj/symphony/elixir`

**Branch/worktree:** Same implementation branch/worktree as Task 1.

**Allowed files:**
- `lib/symphony_elixir/config.ex`
- `lib/symphony_elixir/workflow.ex`
- `lib/symphony_elixir/workflow_store.ex`
- `lib/symphony_elixir/tracker.ex`
- `lib/symphony_elixir/linear/adapter.ex`
- `lib/symphony_elixir/linear/client.ex`
- `lib/symphony_elixir/workspace.ex`
- `lib/symphony_elixir/prompt_builder.ex`
- `lib/symphony_elixir/agent_runner.ex`
- `lib/symphony_elixir/opencode/runner.ex`
- `lib/symphony_elixir/codex/app_server.ex`
- `lib/symphony_elixir/orchestrator.ex`
- `test/symphony_elixir/project_runtime_test.exs`
- Existing focused tests touched by these modules.

**Forbidden files:**
- Systemd units.
- Project repo files under `/home/agent/proj`.
- Broad rewrites of Codex/OpenCode protocol logic.

- [ ] Add project-scoped config access that can read settings from a `ProjectContext` or project workflow store.
- [ ] Keep `Config.settings!/0` as compatibility for single-project mode, but make new runtime calls use `Config.settings!(context)` or equivalent.
- [ ] Make tracker calls project-scoped so two Linear project slugs can be polled independently.
- [ ] Make workspace root/hooks resolve per project.
- [ ] Make `AgentRunner.run/3` accept and pass project context through Codex/OpenCode execution.
- [ ] Replace global `SymphonyElixir.TaskSupervisor` usage in project runtime with the project task supervisor.
- [ ] Keep existing OpenCode behavior intact while moving config/session access behind the first-class runner adapter boundary.
- [ ] Add tests proving two enabled projects can run with different `WORKFLOW.md` files, different `agent.max_concurrent_agents`, and different tracker project slugs.

**Validation commands:**

```bash
mix format
mix specs.check
mix test test/symphony_elixir/project_runtime_test.exs
mix test test/symphony_elixir/workspace_and_config_test.exs
mix test test/symphony_elixir/opencode_runner_test.exs
```

**Stop conditions:**
- Any project-scoped path falls back to another project config.
- OpenCode/Codex integrations are rewritten instead of parameterized.
- Existing single-project tests regress.

**Handoff requirements:**
- List every remaining `Config.settings!()` call and justify whether it is daemon-global, single-project compatibility, or still needs follow-up.
- Include evidence that per-project concurrency remains local.

## Task 3A: First-Class OpenCode Runner Adapter

**Repo path:** `/home/agent/proj/symphony/elixir`

**Branch/worktree:** Same implementation branch/worktree as Task 1.

**Allowed files:**
- `lib/symphony_elixir/runner.ex`
- `lib/symphony_elixir/runner/codex_adapter.ex`
- `lib/symphony_elixir/runner/opencode_adapter.ex`
- `lib/symphony_elixir/runner/event.ex`
- `lib/symphony_elixir/agent_runner.ex`
- `lib/symphony_elixir/opencode/runner.ex`
- `lib/symphony_elixir/opencode/session_store.ex`
- `lib/symphony_elixir/opencode/task_prompt.ex`
- `lib/symphony_elixir/process_policy.ex`
- `lib/symphony_elixir/codex/app_server.ex`
- `lib/symphony_elixir/orchestrator.ex`
- `test/symphony_elixir/runner_adapter_test.exs`
- `test/symphony_elixir/opencode_runner_test.exs`
- `test/symphony_elixir/project_runtime_test.exs`

**Forbidden files:**
- Live OpenCode database under `$XDG_DATA_HOME`.
- Live `/home/agent/proj/*` repositories.
- Systemd units.
- Any rewrite that removes the existing OpenCode task prompt contract marker `symphony:opencode-task-prompt:v1`.

- [ ] Define a `SymphonyElixir.Runner` behaviour with `run(context, workspace, issue, opts)` returning normalized success, reroute, blocked, and error outcomes.
- [ ] Move Codex-specific turn/session code behind `Runner.CodexAdapter` without changing current Codex app-server behavior.
- [ ] Move OpenCode-specific command/session code behind `Runner.OpenCodeAdapter`; do not leave OpenCode as only an `AgentRunner` branch.
- [ ] Preserve OpenCode review-decision gating and loop-breaker behavior before dispatch.
- [ ] Preserve missing OpenCode task prompt reroute to a Codex-owned state, but make it an OpenCode adapter outcome rather than generic `AgentRunner` behavior.
- [ ] Normalize OpenCode session identity: command, execution dir, title, attached server URL, existing session id, resume-result command, and handoff completeness.
- [ ] Emit runner events from OpenCode equivalent to Codex event updates where data exists: started, session_reused, command_started, command_completed, handoff_complete, rerouted, blocked, failed.
- [ ] Add dashboard/API fields for `runner_kind: "opencode"` sessions without pretending they are Codex sessions.
- [ ] Add tests that OpenCode can be selected by state route, reuses an attached session, reports a session id/execution dir, reroutes missing prompt, blocks on process policy, and updates the configured result state after successful handoff.
- [ ] Add tests that Codex and OpenCode adapters can run in two different projects at the same time without sharing config, tracker, workspace, or session metadata.

**Validation commands:**

```bash
mix format
mix specs.check
mix test test/symphony_elixir/runner_adapter_test.exs
mix test test/symphony_elixir/opencode_runner_test.exs
mix test test/symphony_elixir/project_runtime_test.exs
```

**Stop conditions:**
- OpenCode remains a hidden subprocess branch in `AgentRunner`.
- OpenCode session reuse or handoff parsing requires a live OpenCode server/database in unit tests.
- OpenCode dispatch can happen without the architect-authored task prompt marker.
- Codex adapter behavior changes while extracting the runner boundary.

**Handoff requirements:**
- Include the runner adapter contract and event schema.
- Include OpenCode parity evidence: session identity, project-scoped config, dashboard/API projection, missing-prompt reroute, process-policy block, successful handoff.
- Include remaining known gaps where OpenCode cannot expose the same telemetry as Codex and how the dashboard labels those gaps.

## Task 4: Aggregate Dashboard And API

**Repo path:** `/home/agent/proj/symphony/elixir`

**Branch/worktree:** Same implementation branch/worktree as Task 1.

**Allowed files:**
- `lib/symphony_elixir_web/router.ex`
- `lib/symphony_elixir_web/controllers/observability_api_controller.ex`
- `lib/symphony_elixir_web/presenter.ex`
- `lib/symphony_elixir_web/live/dashboard_live.ex`
- `lib/symphony_elixir/status_dashboard.ex`
- `test/symphony_elixir/observability_api_test.exs`
- `test/symphony_elixir/status_dashboard_snapshot_test.exs`

**Forbidden files:**
- Tracker/runners unless an API projection needs a typed field added by earlier tasks.
- Systemd units.

- [ ] Change `GET /api/v1/state` to aggregate all configured projects.
- [ ] Add `GET /api/v1/projects/:project_id/state`.
- [ ] Add `GET /api/v1/projects/:project_id/issues/:issue_identifier`.
- [ ] Keep `GET /api/v1/:issue_identifier` as compatibility-only for single-project mode or return clear ambiguous-route error in root mode.
- [ ] Change `POST /api/v1/refresh` to refresh all enabled projects.
- [ ] Add `POST /api/v1/projects/:project_id/refresh`.
- [ ] Update dashboard home to list project status, running/retrying/blocked counts, active milestone, active issue, runner kind, runner session id, last event, and last error.
- [ ] Project issue payloads must preserve runner-specific detail under a `runner` object instead of Codex-only fields.
- [ ] Add project-specific dashboard view.
- [ ] Add API tests for aggregate state, paused disabled project, errored project, project refresh, and issue lookup.

**Validation commands:**

```bash
mix format
mix specs.check
mix test test/symphony_elixir/observability_api_test.exs
mix test test/symphony_elixir/status_dashboard_snapshot_test.exs
```

**Stop conditions:**
- API requires separate ports per project.
- Disabled projects disappear from aggregate state.
- Compatibility routes hide ambiguous project routing.

**Handoff requirements:**
- Include sample JSON for aggregate and project state.
- Include dashboard screenshot or snapshot output if available.

## Task 5: Smart Polling And Milestone Isolation

**Repo path:** `/home/agent/proj/symphony/elixir`

**Branch/worktree:** Same implementation branch/worktree as Task 1.

**Allowed files:**
- `lib/symphony_elixir/orchestrator.ex`
- `lib/symphony_elixir/linear/adapter.ex`
- `lib/symphony_elixir/tracker.ex`
- `test/symphony_elixir/workspace_and_config_test.exs`
- `test/symphony_elixir/project_runtime_test.exs`
- New focused `test/symphony_elixir/smart_polling_test.exs`

**Forbidden files:**
- OpenCode/Codex runners unless tests expose a context propagation bug.
- Systemd units.

- [ ] Preserve exact Project Milestone launch gate: first matching `phase_state: todo` line is required; `paused`, `needs-decision`, and unmarked milestones do not dispatch.
- [ ] Ensure active/running/retrying/blocked `Need Owner Input` inside a project suppresses milestone discovery for that project.
- [ ] Ensure owner-input polling focuses on issue/comment updates instead of milestone discovery.
- [ ] Ensure one project waiting for owner input does not block other projects.
- [ ] Ensure active milestone inside one project prevents dispatch from another milestone in the same project.
- [ ] Ensure the next milestone starts only after current milestone has no running/retrying/blocked/active issues.
- [ ] Add tests for two enabled projects dispatching independently with `max_concurrent_agents: 1` each.

**Validation commands:**

```bash
mix format
mix specs.check
mix test test/symphony_elixir/smart_polling_test.exs
mix test test/symphony_elixir/workspace_and_config_test.exs
mix test test/symphony_elixir/project_runtime_test.exs
```

**Stop conditions:**
- Any test fixture relies on benchmark/task names or desired metric targets.
- Owner-input issue triggers duplicate milestone planning.
- Tasks from two milestones in the same project can run simultaneously.

**Handoff requirements:**
- Include tests proving per-project owner-input isolation.
- Include tests proving all four projects could run concurrently when each allows one agent.

## Task 6: Systemd, Cutover Assets, And Operational Validation

**Repo path:** `/home/agent/proj/symphony/elixir`

**Branch/worktree:** Same implementation branch/worktree as Task 1.

**Allowed files:**
- `README.md`
- `docs/multiproject_cutover.md`
- `priv/systemd/openai-symphony.service`
- `lib/symphony_elixir/cli.ex`
- `test/symphony_elixir/cli_test.exs`

**Forbidden files:**
- Live `/etc/systemd/system/*` during implementation.
- Live `/home/agent/.symphony/config/projects.yml` unless explicitly asked.
- Stopping/disabling existing services.

- [ ] Add a checked-in systemd unit template for `openai-symphony.service`.
- [ ] Document root config installation at `/home/agent/.symphony/config/projects.yml`.
- [ ] Document daemon logs at `/home/agent/.symphony/logs/openai-symphony/daemon/` and project logs at `/home/agent/.symphony/logs/openai-symphony/<project_id>/`.
- [ ] Add CLI tests for root config mode and old single workflow mode.
- [ ] Write cutover runbook that first verifies current project runs are idle, then installs the root config/unit, then starts the new daemon, then disables old units only after validation.

**Validation commands:**

```bash
mix format
mix specs.check
mix test test/symphony_elixir/cli_test.exs
make all
```

**Stop conditions:**
- Any task attempts to stop, disable, or delete old services before explicit cutover approval.
- New unit removes old per-project units.
- Cutover docs imply milestone existence alone authorizes execution.

**Handoff requirements:**
- Include exact unit template path.
- Include dry-run cutover checklist.
- Include rollback instructions: stop new `openai-symphony.service`, restart previously active per-project units, verify ports `4111`-`4114`.

## Test Matrix

| Area | Required proof |
| --- | --- |
| Root config | Valid multiple projects; duplicate id rejection; disabled project visible and not dispatched; invalid workflow isolated to one project. |
| Supervisor | Root reload starts/stops project workers; child crash does not stop siblings; disabled project has no worker pid. |
| Runtime scoping | Two projects with different `WORKFLOW.md` settings keep tracker/workspace/runner/concurrency separate. |
| Runner parity | Codex and OpenCode both run through first-class adapters with project-scoped config, lifecycle outcomes, normalized events, and API/dashboard projection. |
| Smart polling | Owner input suppresses milestone discovery per project; milestone batches do not mix; all enabled projects can run independently. |
| API | Aggregate state includes all projects; project state is scoped; project issue lookup is scoped; refresh-all and refresh-one route correctly. |
| Dashboard | Home lists projects with paused/error/running state; project detail shows existing running/retrying/blocked rows. |
| CLI | Existing single workflow mode still works; root config mode starts daemon on configured server host/port. |
| Ops | New unit template exists; old units are not removed; cutover docs require explicit approval and idle checks. |

## Cutover Plan

Do not execute this during implementation.

1. Confirm all old services are either idle or explicitly approved to drain:
   - `systemctl --no-pager --plain status openai-symphony-mnemesh.service openai-symphony-nervure.service openai-symphony-neryva.service openai-symphony-agent-forge.service`
2. Confirm no active OpenCode/Codex child processes should be interrupted.
3. Install `/home/agent/.symphony/config/projects.yml`.
4. Install `/etc/systemd/system/openai-symphony.service` from checked-in template.
5. Start new service on `127.0.0.1:4110`.
6. Verify `GET /api/v1/state` includes all four projects and disabled projects show paused.
7. Verify enabled projects can dispatch independently.
8. Stop and disable old per-project units only after acceptance.
9. Rollback: stop new root service, restart old units, verify old ports `4111`-`4114`.

## Residual Risks

- The previous multiproject worktree may exist on an unreachable host; current evidence did not find it locally.
- Existing dirty changes in `agent_runner.ex` and `orchestrator.ex` must not be overwritten; implementation should start by preserving or landing that work.
- OpenCode parity is bigger than process execution. Acceptance must verify session identity, reuse/resume, handoff completeness, process-policy block, missing-prompt reroute, and observability projection.
- Current active services include live project work; cutover before draining would interrupt running agents.
- The broadest risk is incomplete removal of global config access. Require a final `rg "Config\\.settings!|WorkflowStore|TaskSupervisor|Orchestrator"` audit before acceptance.
- API compatibility routes may be ambiguous in root mode; prefer explicit project routes for new clients.
