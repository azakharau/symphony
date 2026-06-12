# Rust vNext Operator Runbook

## Service

Systemd user-service template: `deploy/systemd/openai-symphony-vnext-symphony.service`

Install/update:

```bash
cargo build --release -p symphony-vnext
install -Dm0755 target/release/symphony-vnext /home/agent/.cargo/bin/symphony-vnext
install -Dm0644 config/symphony.projects.toml /home/agent/.symphony/vnext/symphony/projects.toml
install -Dm0644 deploy/systemd/openai-symphony-vnext-symphony.service /home/agent/.config/systemd/user/openai-symphony-vnext-symphony.service
systemctl --user daemon-reload
systemctl --user restart openai-symphony-vnext-symphony.service
systemctl --user status openai-symphony-vnext-symphony.service
```

The user service points at `/home/agent/.symphony/vnext/symphony/projects.toml` and
`/home/agent/.symphony/vnext/symphony/runtime.sqlite3`. The service reads `LINEAR_API_KEY` from
`/home/agent/.symphony/env/linear.env`; do not copy the key into workflow files or checked-in
configuration.

## Smoke Checks

Local non-live checks:

```bash
/home/agent/.cargo/bin/symphony-vnext validate-config --config config/symphony.projects.toml
/home/agent/.cargo/bin/symphony-vnext init-store --database /tmp/symphony-vnext-smoke.sqlite3
/home/agent/.cargo/bin/symphony-vnext daemon --config config/symphony.projects.toml --database /tmp/symphony-vnext-smoke.sqlite3 --once
```

OpenCode ACP contract smoke without sending an implementation prompt:

```bash
SYMPHONY_VNEXT_LIVE_OPENCODE_ACP=1 cargo test -p symphony-vnext --test bootstrap \
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
systemctl --user stop openai-symphony-vnext-symphony.service
cp /home/agent/.symphony/vnext/symphony/runtime.sqlite3 /home/agent/.symphony/vnext/symphony/runtime.sqlite3.rollback
systemctl --user start openai-symphony-vnext-symphony.service
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
