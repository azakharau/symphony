# Symphony

Symphony turns project work into isolated implementation runs. The active Symphony runtime is the Rust
`symphony` service, which schedules Linear issues and runs OpenCode ACP in per-issue
worktrees.

[![Symphony demo video preview](.github/media/symphony-demo-poster.jpg)](https://player.vimeo.com/video/1186371009?h=5626e4b899)

> [!WARNING]
> Symphony is an engineering preview for trusted operator environments.

## Active Runtime

Symphony is the only active service implementation in this repository. The old Elixir runtime and
Codex runner integration have been removed from active code, CI, and operator docs.

The Rust workspace contains:

- Typed multiproject config loading.
- Linear project and milestone scoping.
- OpenCode-only ACP launch configuration with nd-JSON stdio, `session/set_config_option` model and
  effort selection, and per-issue git worktrees.
- SQLite runtime state bootstrap and restart-safe state queries.
- Issue orchestration for `Todo`, `In Progress`, `Need Owner Input`, backlog, blockers, terminal
  reconciliation, eval repair loops, and git-closure handoffs.
- Dashboard/API read models for aggregate, project, and issue drilldown views.

Symphony parks legacy steward states (`Preparing`, `In Review`, `RCA Required`) instead of treating
them as executable runtime aliases.

## Configuration

Use the checked-in sample as the active service shape:

```bash
cargo run -p symphony -- validate-config --config config/symphony.projects.toml
cargo run -p symphony -- init-store --database /home/agent/.symphony/symphony/runtime.sqlite3
cargo run -p symphony -- daemon --config config/symphony.projects.toml --database /home/agent/.symphony/symphony/runtime.sqlite3
```

Continuous service mode uses the systemd unit template in
`deploy/systemd/symphony.service` after the operator approves a safe host restart window. Do not
restart currently running user services during documentation or config-only updates.
Use `--once` only for non-live bootstrap validation. Continuous mode requires `LINEAR_API_KEY` so the
daemon can poll and mutate Linear through the Rust adapter. The service reads that key from the
existing file-backed Symphony environment at `/home/agent/.symphony/env/linear.env`; do not duplicate
the key in project workflow files.

## Validation

Default validation does not start OpenCode, mutate Linear, or restart systemd:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Live cutover validation requires host credentials and operator control:

```bash
SYMPHONY_LIVE_OPENCODE_ACP=1 cargo test -p symphony --test bootstrap \
  installed_opencode_acp_supports_ndjson_config_options_without_prompting -- --nocapture
cargo build --release -p symphony
/usr/local/bin/opencode acp
systemctl --user status symphony.service
curl -fsS http://127.0.0.1:4115/api/dashboard
curl -fsS http://127.0.0.1:4115/api/projects/symphony
curl -fsS http://127.0.0.1:4115/api/projects/recall
```

## Runtime Contract

See [SPEC.md](SPEC.md) for the Rust/OpenCode-only Symphony service contract.

## License

This project is licensed under the [Apache License 2.0](LICENSE).
