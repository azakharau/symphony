# Symphony multiproject root daemon dry-run and cutover runbook

This runbook is a checked-in operator asset for SYM-19. It separates read-only
preparation, isolated dry-run rehearsal, owner-approved cutover, health checks,
stop commands, and rollback. It does not authorize live service mutation.

## No-cutover policy and stop conditions

- Milestone existence, milestone descriptions, or `phase_state:*` text do not
  authorize dispatch, service mutation, or cutover.
- Do not run `systemctl start`, `stop`, `restart`, `reload`, `enable`,
  `disable`, `daemon-reload`, `kill`, `pkill`, or equivalent mutation commands
  unless the owner approval names the exact service and exact action.
- Do not run the root daemon against live
  `/home/agent/.symphony/config/projects.yml` while old per-project services are
  active unless the owner approval names that exact root-daemon action.
- Stop immediately if active child runs could be interrupted, the rollback target
  is missing, the root config is unreviewed, or any command would remove old
  per-project units before acceptance.

## Checked-in assets and live targets

- Unit template: `elixir/priv/systemd/openai-symphony.service`.
- Approved live unit target, only after owner approval:
  `/etc/systemd/system/openai-symphony.service`.
- Durable root config target: `/home/agent/.symphony/config/projects.yml`.
- Root daemon bind address: `127.0.0.1:4110`; the host is supplied by the root
  config and the unit passes `--port 4110`.
- Daemon log root: `/home/agent/.symphony/logs/openai-symphony/daemon/`.
- Project log roots: `/home/agent/.symphony/logs/openai-symphony/<project_id>/`.
- Runtime state is owned by the `agent` user. Systemd owns the root daemon main
  process PID for `openai-symphony.service`; Symphony owns only project-local
  runtime state under the configured logs/workspace/state paths and must not
  delete or disable old per-project service units during cutover.

## Project config resolution

The root daemon is selected with `--projects-config <path-to-projects.yml>`.
Relative `projects[].workflow_path` entries resolve relative to the projects
config file. Absolute workflow paths are used as written. Disabled projects stay
visible as disabled and do not start project workers. `execution.enabled: false`
or `gates.dispatch_enabled: false` keeps dispatch blocked while allowing the
project to remain configured for evidence collection.

Minimal durable config shape:

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
    execution:
      enabled: true
    gates:
      dispatch_enabled: false
```

## 1. Read-only evidence collection

Run these commands before any dry-run or owner-approved cutover. They collect
current service, listener, repository, and config evidence without mutation.

```bash
pwd
realpath /home/agent/proj/symphony/elixir
git -C /home/agent/proj/symphony status --short
systemctl list-units 'openai-symphony*.service' --type=service --all --no-pager
systemctl show 'openai-symphony*.service' --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager
ss -ltnp '( sport = :3000 or sport = :4110 or sport = :4111 or sport = :4112 or sport = :4113 or sport = :4114 or sport = :4115 )'
```

For each previously active per-project service, record the unit name, current
workflow path, working directory, user, listener port, runner routes, workspace
root, active milestone pointer, and whether there are active child runs.

## 2. Isolated no-mutation dry-run rehearsal

This rehearsal must use temporary config, logs, and state outside live paths. It
must not read or write `/home/agent/.symphony/config/projects.yml`, install a
unit, reload systemd, or stop old services.

```bash
export SYMPHONY_DRY_RUN_ROOT="$(mktemp -d /tmp/symphony-root-dry-run.XXXXXX)"
mkdir -p \
  "$SYMPHONY_DRY_RUN_ROOT/config" \
  "$SYMPHONY_DRY_RUN_ROOT/logs/openai-symphony/daemon" \
  "$SYMPHONY_DRY_RUN_ROOT/logs/openai-symphony/dry-run-symphony"
cat > "$SYMPHONY_DRY_RUN_ROOT/config/projects.yml" <<'YAML'
server:
  host: 127.0.0.1
  port: 4110
projects:
  - id: dry-run-symphony
    name: Dry-run Symphony
    enabled: false
    workflow_path: /home/agent/proj/symphony/elixir/WORKFLOW.md
    repo_root: /home/agent/proj/symphony
    app_root: /home/agent/proj/symphony/elixir
    logs_root: /tmp/replace-with-symphony-dry-run-root/logs/openai-symphony/dry-run-symphony
    execution:
      enabled: false
    gates:
      dispatch_enabled: false
YAML
python3 - <<'PY'
import os
from pathlib import Path

root = os.environ["SYMPHONY_DRY_RUN_ROOT"]
path = Path(root) / "config" / "projects.yml"
text = path.read_text()
text = text.replace("/tmp/replace-with-symphony-dry-run-root", root)
path.write_text(text)
PY
cd /home/agent/proj/symphony/elixir
mix test test/symphony_elixir/root_config_test.exs
mix test test/symphony_elixir/project_supervisor_test.exs
mix test test/symphony_elixir/workspace_and_config_test.exs
mix format --check-formatted
git -C /home/agent/proj/symphony diff --check
```

If the owner separately approves a foreground root-daemon rehearsal against only
this temporary config while old services remain active, use a port that is known
free from the read-only listener inventory and keep all projects disabled/gated:

```bash
cd /home/agent/proj/symphony/elixir
./bin/symphony --i-understand-that-this-will-be-running-without-the-usual-guardrails \
  --projects-config "$SYMPHONY_DRY_RUN_ROOT/config/projects.yml" \
  --logs-root "$SYMPHONY_DRY_RUN_ROOT/logs/openai-symphony/daemon" \
  --port 4110
```

Stop the foreground rehearsal with terminal interrupt only after confirming it
is the manually started dry-run process, not a live service process. If port
`4110` is occupied, do not kill the listener; choose another owner-approved dry
run port or stop.

## 3. Owner-approved cutover commands

Leave this section unused until owner approval names each exact action. The old
per-project units must remain installed and available for rollback.

```bash
# owner-approved action: install root config
install -o agent -g agent -m 0644 /path/to/reviewed/projects.yml /home/agent/.symphony/config/projects.yml

# owner-approved action: install checked-in unit template
sudo install -o root -g root -m 0644 \
  /home/agent/proj/symphony/elixir/priv/systemd/openai-symphony.service \
  /etc/systemd/system/openai-symphony.service

# owner-approved action: reload systemd after installing the unit
sudo systemctl daemon-reload

# owner-approved action: start the root daemon
sudo systemctl start openai-symphony.service
```

Do not disable or remove old `openai-symphony-*.service` units during initial
root daemon validation. Disable old units only after owner acceptance of the root
service and a separate approval for each exact old-unit action.

## 4. Health checks after approved start

These checks are read-only and should be run immediately after an approved root
service start.

```bash
systemctl show openai-symphony.service --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager
systemctl status openai-symphony.service --no-pager --lines=50
ss -ltnp '( sport = :3000 or sport = :4110 or sport = :4111 or sport = :4112 or sport = :4113 or sport = :4114 or sport = :4115 )'
curl -fsS --max-time 5 http://127.0.0.1:4110/api/v1/state | python3 -m json.tool
find /home/agent/.symphony/logs/openai-symphony -maxdepth 2 -type f -printf '%p\n' | sort
```

Expected evidence: `openai-symphony.service` is loaded and running as `agent`,
`127.0.0.1:4110` is listening, project state is visible, disabled projects are
reported as disabled/paused, and daemon/project logs are under the documented
log root.

## 5. Owner-approved stop commands

Stopping is a service mutation. Use only with approval naming
`openai-symphony.service` and the exact stop action.

```bash
# owner-approved action: stop root daemon
sudo systemctl stop openai-symphony.service

# read-only verification after stop
systemctl show openai-symphony.service --property=Id,LoadState,ActiveState,SubState --no-pager
ss -ltnp '( sport = :4110 )'
```

## 6. Rollback rehearsal and rollback commands

Before cutover, rehearse rollback on paper by recording the previously active
per-project units, their unit files, workflow paths, working directories, users,
ports, and runner routes. Preserve old per-project units; do not overwrite or
remove them during root daemon installation.

Rollback trigger examples: root daemon fails to start, `4110` is unavailable,
project state is missing or wrong, issue lifecycle validation fails, logs are not
written to the expected root, or the owner rejects the cutover.

```bash
# read-only evidence before rollback mutation
systemctl show openai-symphony.service --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager
systemctl list-units 'openai-symphony*.service' --type=service --all --no-pager
ss -ltnp '( sport = :3000 or sport = :4110 or sport = :4111 or sport = :4112 or sport = :4113 or sport = :4114 or sport = :4115 )'

# owner-approved action: stop new root service
sudo systemctl stop openai-symphony.service

# owner-approved action, only if the live unit target must be restored
sudo install -o root -g root -m 0644 /path/to/previous/openai-symphony.service /etc/systemd/system/openai-symphony.service
sudo systemctl daemon-reload

# owner-approved action: restart only units recorded as previously active.
# Example commands for the known old per-project units; run only the approved
# commands for units that were active in the pre-cutover inventory.
sudo systemctl restart openai-symphony-mnemesh.service
sudo systemctl restart openai-symphony-nervure.service
sudo systemctl restart openai-symphony-neryva.service
sudo systemctl restart openai-symphony-agent-forge.service
sudo systemctl restart openai-symphony-symphony.service

# read-only verification after rollback
systemctl show 'openai-symphony*.service' --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager
ss -ltnp '( sport = :3000 or sport = :4110 or sport = :4111 or sport = :4112 or sport = :4113 or sport = :4114 or sport = :4115 )'
```

Restart only units that were recorded as previously active and only after owner
approval names each unit/action; the known old per-project unit examples are
`openai-symphony-mnemesh.service`, `openai-symphony-nervure.service`,
`openai-symphony-neryva.service`, `openai-symphony-agent-forge.service`, and
`openai-symphony-symphony.service`. If the previous root config or unit target was
changed, restore it from the recorded rollback target before restarting old
services. Verify ports/listeners and issue lifecycle evidence before resuming
any dispatch.
