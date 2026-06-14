# Symphony Operator Runbook

## Service

Systemd user-service template: `deploy/systemd/symphony.service`

Install/update:

```bash
cargo build --release -p symphony
install -Dm0755 target/release/symphony /home/agent/.cargo/bin/symphony
install -Dm0644 config/symphony.projects.toml /home/agent/.symphony/symphony/projects.toml
install -Dm0644 deploy/systemd/symphony.service /home/agent/.config/systemd/user/symphony.service
systemctl --user daemon-reload
systemctl --user restart symphony.service
systemctl --user status symphony.service
```

Run the `daemon-reload` and `restart` commands only inside an operator-approved safe restart window.
The active host deployment is one multiproject `symphony.service`; do not run project-specific
legacy units for Symphony orchestration after cutover.

The user service points at `/home/agent/.symphony/symphony/projects.toml` and
`/home/agent/.symphony/symphony/runtime.sqlite3`. The service reads `LINEAR_API_KEY` from
`/home/agent/.symphony/env/linear.env`; do not copy the key into workflow files or checked-in
configuration.

## Smoke Checks

Use this lightweight activation smoke before enabling or after changing any configured
project. The smoke is non-destructive: it validates config, observes the already-running service and
dashboard/API state, and checks whether queued work is executable or intentionally idle. Do not run
service restarts as part of this smoke; restart only during the operator-approved install/cutover path
above.

### Multiproject activation checklist

1. Validate the checked-in root config and confirm each enabled project has a Mnemesh workspace root,
   isolated worktree root, OpenCode ACP command, eval default suite, and concurrency limit:

   ```bash
   /home/agent/.cargo/bin/symphony validate-config --config config/symphony.projects.toml
   sed -n '/\[\[projects\]\]/,/\[projects.concurrency\]/p' config/symphony.projects.toml
   ```

   For the checked-in Symphony project this should show `id = "symphony"`,
   `workspace_root = "/home/agent/proj/symphony"`,
   `worktree_root = "/home/agent/.symphony/workspaces/opencode/symphony"`,
   `/usr/local/bin/opencode` with `args = ["acp"]`, `default_suite = "symphony-validation"`,
   and `max_sessions = 1`.

2. Confirm the single host service is active without restarting it:

   ```bash
   systemctl --user status symphony.service --no-pager
   ```

   Host evidence should show the unit `active (running)`. Treat inactive or failed units as an
   operator blocker, not as permission to restart outside the safe window.

3. Check dashboard and per-project API reachability. The Symphony host service listens on port 4115
   and serves all enabled projects from the same daemon:

   ```bash
   curl -fsS http://127.0.0.1:4115/api/dashboard
   curl -fsS http://127.0.0.1:4115/api/projects/symphony
   curl -fsS http://127.0.0.1:4115/api/projects/mnemesh
   ```

   The project response must make the project state visible: active capacity, parked blockers,
   terminal counts, current session metadata, and whether capacity is full. A project with no Todo
   candidate is still healthy when the response makes the idle reason visible.

4. Detect executable Linear Todo candidates before starting new work. Use the project API first, then
   Linear only if the API does not already explain the queue:

   ```bash
   curl -fsS http://127.0.0.1:4115/api/projects/symphony \
     | python3 -m json.tool | sed -n '/"eligible"/,/"blockers"/p'
   ```

   A candidate is executable only when it is in Todo, has no nonterminal blocker, the project has
   free capacity, and required project config is present. Otherwise record the visible reason:
   blocker, capacity full, missing Mnemesh workspace root, provider failure, or intentionally idle.

5. Prevent duplicate OpenCode ACP sessions. If the project API shows an issue already `In Progress`
   with a session id, inspect or resume that session; do not create a new session or replay the
   original prompt. For a host snapshot like SYM-35 on Symphony, `capacity_full` plus current session
   metadata means the correct activation result is "running; no new session started".

6. Confirm ACP isolation and worktree cleanup boundaries. Running sessions must use
   `branch.worktree_root/<issue identifier>` and OpenCode must run with that worktree as `cwd`.
   Cleanup is allowed only after a successful structured handoff whose worktree path exactly matches
   the active session worktree; never remove an in-progress worktree during activation smoke.

7. Repeat the same pattern for additional enabled projects by substituting the project id and port
   from the deployed host config. For Mnemesh deployments, use the Mnemesh project workspace root as
   the canonical MCP evidence workspace and the project-specific Symphony/OpenCode worktree root only
   for isolated ACP execution.

Local non-live checks:

```bash
/home/agent/.cargo/bin/symphony validate-config --config config/symphony.projects.toml
/home/agent/.cargo/bin/symphony init-store --database /tmp/symphony-smoke.sqlite3
/home/agent/.cargo/bin/symphony daemon --config config/symphony.projects.toml --database /tmp/symphony-smoke.sqlite3 --once
```

OpenCode ACP contract smoke without sending an implementation prompt:

```bash
SYMPHONY_LIVE_OPENCODE_ACP=1 cargo test -p symphony --test bootstrap \
  installed_opencode_acp_supports_ndjson_config_options_without_prompting -- --nocapture
```

Live checks:

```bash
/usr/local/bin/opencode acp
curl -fsS http://127.0.0.1:4115/api/dashboard
curl -fsS http://127.0.0.1:4115/api/projects/symphony
curl -fsS http://127.0.0.1:4115/api/projects/mnemesh
```

## Rollback

Rollback is Rust-service-only:

```bash
systemctl --user stop symphony.service
cp /home/agent/.symphony/symphony/runtime.sqlite3 /home/agent/.symphony/symphony/runtime.sqlite3.rollback
systemctl --user start symphony.service
```

Do not restart or restore the removed Elixir runtime. If the Rust service cannot run, leave active
issues parked in Linear and recover from the SQLite store plus per-issue OpenCode worktrees.

## Runtime Notes

Continuous mode runs the poll loop and dashboard API until the service is stopped. Use `--once` only
for local bootstrap validation.

OpenCode ACP is a newline-delimited JSON-RPC subprocess. Symphony creates the git worktree first,
then sends `initialize`, `session/new`, `session/set_config_option` for `mode`, `model`, and
`effort`, and only then `session/prompt`. The checked-in Symphony project config selects
`openai/gpt-5.5` with `effort: high`.
