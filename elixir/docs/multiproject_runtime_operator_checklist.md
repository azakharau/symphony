# Multiproject runtime operator checklist

This checklist covers the current multiproject runtime foundation. It is an operator
preparation guide only: do not stop existing per-project services, enable a new
root daemon, change systemd units, or migrate live issue processing unless the
owner explicitly approves the named service and exact action.

## Current runtime boundary

- Single-project mode remains supported with `./bin/symphony /path/to/WORKFLOW.md`.
- Root mode is selected with `--projects-config /path/to/projects.yml`.
- Root mode currently loads root configuration, validates each project, starts
  project-local infrastructure, and keeps per-project orchestrators dispatch-paused.
- The current foundation does not yet replace live per-project services or prove
  full multiproject dispatch readiness.
- Per-project `WORKFLOW.md` remains the authority for tracker, states, runner
  routes, workspace roots, hooks, prompts, OpenCode/Codex settings, and
  per-project concurrency.

## Active milestone model

- The active milestone is configured per project in that project's `WORKFLOW.md`
  front matter:

```yaml
stewardship:
  active_milestone_id: "0b8b5a7e-d9a6-47df-a824-435cce359cb2"
  active_milestone_name: "01. Multiproject runtime foundation"
```

- Dispatch is allowed only for issues whose Project Milestone id matches
  `stewardship.active_milestone_id`.
- A missing active milestone pointer blocks milestone work.
- Milestone descriptions are product context only. `phase_state:*` text has no
  runtime effect and must not be used as a dispatch gate.
- When all known child issues for the active milestone are terminal, Symphony
  clears the runtime active pointer, records that closure in the in-memory
  runtime cache, and waits for the owner to clear or replace the configured
  pointer.

## Root projects config

The intended durable path is `/home/agent/.symphony/config/projects.yml`.

Minimal shape:

```yaml
server:
  host: 127.0.0.1
  port: 4110

projects:
  - id: mnemesh
    name: Mnemesh
    enabled: false
    workflow_path: /home/agent/proj/mnemesh/WORKFLOW.md
    repo_root: /home/agent/proj/mnemesh
    app_root: /home/agent/proj/mnemesh
    dashboard_order: 10
    execution:
      enabled: true
    gates:
      dispatch_enabled: false
    linear:
      team:
        key: MNE
      project:
        name: mnemesh
      milestone:
        id: "<active-project-milestone-id>"
        name: "<active-project-milestone-name>"
```

Field notes:

- `projects[].id` is required, unique, and lower-case URL-safe text.
- `projects[].workflow_path` is required and resolved relative to the config
  file when not absolute.
- `enabled` defaults to `false`; disabled projects stay visible as disabled and
  do not start project workers.
- `execution.enabled: false` blocks dispatch even when the project is enabled.
- `gates.dispatch_enabled: false` blocks dispatch while keeping the project
  configured.
- Invalid project `WORKFLOW.md` files are isolated to that project context and
  must not bring down other configured projects.

## Pre-migration checklist

Run this before proposing any owner-approved cutover.

1. Confirm the current repository and app roots:

```bash
pwd
realpath /home/agent/proj/symphony/elixir
git -C /home/agent/proj/symphony status --short
```

2. Record current service inventory without mutation:

```bash
systemctl list-units 'openai-symphony*.service' --type=service --all --no-pager
systemctl show 'openai-symphony*.service' --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager
ss -ltnp '( sport = :3000 or sport = :4110 or sport = :4111 or sport = :4112 or sport = :4113 or sport = :4114 or sport = :4115 )'
```

3. For every candidate project, record:

- Current service unit name, if any.
- Current `WORKFLOW.md` path from `systemctl show ExecStart`.
- Current `tracker.project_slug`, active states, terminal states, runner routes,
  workspace root, and `stewardship.active_milestone_id`.
- Whether the active milestone has non-terminal child issues.
- Whether `execution.enabled` and `gates.dispatch_enabled` should remain false
  during preparation.

4. Validate the root config parser and focused runtime contracts from the Elixir
   app root:

```bash
cd /home/agent/proj/symphony/elixir
mix test test/symphony_elixir/root_config_test.exs
mix test test/symphony_elixir/project_supervisor_test.exs
mix test test/symphony_elixir/workspace_and_config_test.exs
mix format --check-formatted
mix specs.check
git diff --check
```

5. Dry-start root mode only in an owner-approved, non-live environment:

```bash
cd /home/agent/proj/symphony/elixir
./bin/symphony --i-understand-that-this-will-be-running-without-the-usual-guardrails --projects-config /home/agent/.symphony/config/projects.yml
```

Do not run this command against live project config while the old per-project
services are active unless the owner approved that exact action.

## Cutover readiness checklist

Cutover remains blocked until all items below are true.

- Owner approval names the exact service or daemon action.
- Existing per-project services are idle, or the owner accepts the live-run risk.
- The root config is committed or otherwise durably recorded.
- Each project has an explicit active milestone pointer or is intentionally
  disabled/gated off.
- No project relies on `phase_state:*` milestone description text for dispatch.
- A rollback target records the previous unit, workflow path, runner routes, and
  workspace root for every affected project.
- Focused root-config, project-supervisor, workspace/config, formatting, spec,
  and whitespace checks passed after the final docs/config edit.
- Post-change validation commands and expected project states are written before
  any service mutation command is run.

## Rollback checklist

Use this only after an owner-approved cutover action has started.

1. Preserve failure evidence with read-only service, listener, dashboard/API,
   logs, and issue lifecycle commands.
2. Restore the previous workflow or root config from the recorded rollback
   target.
3. Run only owner-approved service mutation commands required for rollback.
4. Verify restored service state with read-only `systemctl show`, listener, and
   issue lifecycle checks.
5. Record remaining blockers before resuming dispatch.

## Known blockers and residual risks

- Root mode currently starts project infrastructure with dispatch paused; full
  context-aware multiproject dispatch is still later milestone work.
- No checked-in systemd root daemon unit is part of this foundation checklist.
- No live cutover has been approved by this document.
- Service inventory, listener state, and active issue state can drift after it is
  recorded; refresh evidence immediately before any owner-approved action.
