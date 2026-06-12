# Symphony Operator Runbook

## Service

Systemd user-service template: `deploy/systemd/openai-symphony-symphony.service`

Install/update:

```bash
cargo build --release -p symphony
install -Dm0755 target/release/symphony /home/agent/.cargo/bin/symphony
install -Dm0644 config/symphony.projects.toml /home/agent/.symphony/symphony/projects.toml
install -Dm0644 deploy/systemd/openai-symphony-symphony.service /home/agent/.config/systemd/user/openai-symphony-symphony.service
systemctl --user daemon-reload
systemctl --user restart openai-symphony-symphony.service
systemctl --user status openai-symphony-symphony.service
```

Run the `daemon-reload` and `restart` commands only inside an operator-approved safe restart window.
Until that cutover, the currently loaded `openai-symphony-vnext-*` user units are legacy-named host
aliases for the Rust/OpenCode Symphony runtime and must not be interrupted by this repository update.
The Mnemesh service should receive a primary-named unit in the same approved host cutover, or keep the
legacy-named user unit documented as a host alias until it can be safely renamed.

The user service points at `/home/agent/.symphony/symphony/projects.toml` and
`/home/agent/.symphony/symphony/runtime.sqlite3`. The service reads `LINEAR_API_KEY` from
`/home/agent/.symphony/env/linear.env`; do not copy the key into workflow files or checked-in
configuration.

## Smoke Checks

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
```

## Rollback

Rollback is Rust-service-only:

```bash
systemctl --user stop openai-symphony-symphony.service
cp /home/agent/.symphony/symphony/runtime.sqlite3 /home/agent/.symphony/symphony/runtime.sqlite3.rollback
systemctl --user start openai-symphony-symphony.service
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
