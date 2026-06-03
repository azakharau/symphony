# Multiproject runtime operator runbook

This runbook covers the root Symphony runtime that supervises multiple project workers from one
`projects.yml` file. It is operator preparation and migration guidance only. Do not stop existing
per-project services, enable a new systemd unit, restart active services, or mutate project
`WORKFLOW.md` files unless the owner has approved the exact service and action.

## Runtime shape

Single-project mode still starts from one workflow file:

```bash
./bin/symphony --i-understand-that-this-will-be-running-without-the-usual-guardrails ./WORKFLOW.md
```

Multiproject mode starts from a root config file instead:

```bash
./bin/symphony \
  --i-understand-that-this-will-be-running-without-the-usual-guardrails \
  --projects-config /home/agent/.symphony/config/projects.yml
```

The two modes are mutually exclusive. Do not pass both `--projects-config` and a positional
`WORKFLOW.md` path.

Root config owns only daemon-level and project inventory metadata. Each project `WORKFLOW.md`
remains the authority for tracker settings, prompts, workspace roots, hooks, runner routes,
OpenCode/Codex settings, polling, retries, stewardship, and validation policy.

## Root config schema

Default root config path for local operations:

```text
/home/agent/.symphony/config/projects.yml
```

Example:

```yaml
server:
  host: 127.0.0.1
  port: 4110
projects:
  - id: mnemesh
    name: Mnemesh
    enabled: false
    repo_root: /home/agent/proj/mnemesh
    app_root: /home/agent/proj/mnemesh
    workflow_path: /home/agent/proj/mnemesh/WORKFLOW.md
    dashboard_order: 10
    logs_root: /home/agent/.symphony/logs/mnemesh
    linear:
      team:
        key: MNE
        name: Mnemesh
      project:
        id: "<linear-project-id>"
        slug: "<linear-project-slug>"
        name: Mnemesh
      milestone:
        id: "<active-milestone-id>"
        name: "<active-milestone-name>"
    mnemesh:
      workspace_id: "<workspace-id>"
      task_id: "<task-id>"
      subtask_id: "<subtask-id>"
      handoff_cursor: "<cursor>"
    runner:
      owner: opencode
    execution:
      enabled: true
    gates:
      dispatch_enabled: false
```

Root fields:

- `server.host`: optional HTTP bind host; defaults to `127.0.0.1`.
- `server.port`: optional non-negative integer. If omitted, the root config does not set a port.
- `projects`: list of project entries. An empty list is valid for config parsing but does not
  dispatch work.

Project fields:

- `id`: required lower-case URL-safe identifier. It must be unique across the file.
- `name`: optional display name. Blank or non-string values fall back to `id`.
- `enabled`: optional boolean, default `false`. Disabled projects remain visible but do not start
  dispatchable project workers.
- `workflow_path`: required path to that project's `WORKFLOW.md`. Relative paths resolve relative
  to the `projects.yml` directory.
- `repo_root`, `app_root`, `logs_root`: optional paths. Relative paths resolve relative to the
  `projects.yml` directory.
- `dashboard_order`: optional integer for display ordering.
- `linear.team`: optional map. Only `key` and `name` are retained.
- `linear.project`: optional map. Only `id`, `slug`, and `name` are retained.
- `linear.milestone`: optional map. Only `id` and `name` are retained.
- `mnemesh`: optional map. Only `workspace_id`, `task_id`, `subtask_id`, and `handoff_cursor` are
  retained.
- `runner`: optional map reserved for project-level runner metadata.
- `execution.enabled`: optional gate, default `true`. Set to `false` to block dispatch while keeping
  the project configured.
- `gates.dispatch_enabled`: optional gate, default `true`. Set to `false` for a deliberate
  operator hold.

Dispatch blockers are explicit:

- disabled project: `enabled: false` or disabled status
- invalid root project config
- missing `WORKFLOW.md`
- invalid project `WORKFLOW.md`
- `execution.enabled: false`
- `gates.dispatch_enabled: false`

## Pre-migration checklist

Complete this before starting the root multiproject runtime or cutting over any existing
per-project service.

1. Confirm owner approval for the named action: config draft, read-only validation, root service
   start, per-project service stop, rollback, or documentation update.
2. Capture the currently active per-project services:

   ```bash
   systemctl list-units 'openai-symphony*.service' --type=service --all --no-pager
   systemctl show 'openai-symphony*.service' --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager
   ss -ltnp '( sport = :3000 or sport = :4110 or sport = :4111 or sport = :4112 or sport = :4113 or sport = :4114 or sport = :4115 )'
   ```

3. For each project entry, verify the `workflow_path` points to the intended project
   `WORKFLOW.md` and that the file is readable.
4. Keep new project entries disabled first with `enabled: false` or with
   `gates.dispatch_enabled: false`.
5. Set the root dashboard port so it does not collide with current per-project service ports.
   The local convention is root port `4110`; existing per-project services have used `4111`
   through `4115`.
6. Record rollback targets for every service that may be changed: unit name, current `ExecStart`,
   current workflow path, current port, and current runner route policy.
7. Check for active issue runs before changing any service. If a project is in a critical section,
   either wait or get explicit owner approval for the risk.
8. Preserve dirty or unrelated repo changes before editing config or docs.

## Root config validation

Run these checks from the Elixir app root before starting or reloading a root config:

```bash
cd /home/agent/proj/symphony/elixir
mix test test/symphony_elixir/root_config_test.exs
mix test test/symphony_elixir/project_supervisor_test.exs
mix format --check-formatted
mix specs.check
git diff --check
```

For a concrete `projects.yml`, also parse it with the application code before proposing cutover:

```bash
cd /home/agent/proj/symphony/elixir
mix run -e 'path = "/home/agent/.symphony/config/projects.yml"; {:ok, config} = SymphonyElixir.RootConfig.load(path); IO.inspect(config, label: "root_config")'
```

Expected result: parsing succeeds, intended projects have `status: :valid`, and any intentional
operator hold is visible through `enabled: false`, `execution.enabled: false`, or
`gates.dispatch_enabled: false`.

## Dry-run startup checklist

Use this only after root config validation passes.

- Projects config: `/home/agent/.symphony/config/projects.yml`
- Owner approval reference: `<comment/ticket/time>`
- Existing services inventory timestamp: `<UTC timestamp>`
- Root dashboard target: `127.0.0.1:4110`
- Projects intentionally enabled: `<project ids>`
- Projects intentionally held: `<project ids and hold reason>`
- Command:

  ```bash
  cd /home/agent/proj/symphony/elixir
  ./bin/symphony \
    --i-understand-that-this-will-be-running-without-the-usual-guardrails \
    --projects-config /home/agent/.symphony/config/projects.yml
  ```

- First-read checks:

  ```bash
  curl -fsS http://127.0.0.1:4110/api/v1/state | python3 -m json.tool
  curl -fsS http://127.0.0.1:4110/ >/tmp/symphony-root-dashboard.html
  ```

- Decision: `<validated/blocked/rollback>`
- Notes: `<residual risks>`

## Cutover policy

No cutover is authorized by this runbook. The normal safe sequence is:

1. Draft or update `/home/agent/.symphony/config/projects.yml` with all projects held.
2. Validate root config and tests.
3. Start the root runtime on a non-conflicting port.
4. Verify dashboard/API visibility and dispatch blockers.
5. Enable one low-risk project only after owner approval.
6. Observe one issue lifecycle or an owner-approved synthetic validation run.
7. Repeat project-by-project.
8. Stop old per-project services only after the corresponding project is proven under the root
   runtime and the owner approves the exact service mutation.

Never run `systemctl start`, `stop`, `restart`, `reload`, `enable`, `disable`, `daemon-reload`,
`kill`, `pkill`, or equivalent commands unless the owner approved that exact action.

## Rollback checklist

Use this only after owner-approved cutover work has started.

1. State the rollback trigger: failed startup, dashboard unavailable, unexpected dispatch, issue
   lifecycle failure, Linear state mismatch, owner decision, or service health regression.
2. Preserve evidence before rollback:

   ```bash
   systemctl show '<service>' --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager
   curl -fsS http://127.0.0.1:4110/api/v1/state | python3 -m json.tool
   ```

3. Disable new dispatch in `projects.yml` with `enabled: false` or
   `gates.dispatch_enabled: false`.
4. Restore the previous per-project service or workflow settings from the recorded rollback target.
5. Run only owner-approved service mutation commands needed for rollback.
6. Verify restored service state and issue queue behavior with read-only checks.
7. Record the rollback result, unresolved blocker, and next owner decision needed.

## Known risks

- Root config parsing validates project workflow shape when the workflow file exists, but it does
  not prove live Linear credentials, OpenCode reachability, or full issue lifecycle behavior.
- Disabled or dispatch-held projects are visible operational state, not a substitute for owner
  approval to mutate services.
- Running old per-project services and the root runtime against the same Linear project can create
  duplicate dispatch risk unless the project is held in one runtime.
- Service and port inventory can drift after the timestamped check. Refresh inventory immediately
  before any approved mutation.
