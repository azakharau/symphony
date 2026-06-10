# Symphony

Symphony turns project work into isolated implementation runs. The active vNext runtime is the Rust
`symphony-vnext` service, which schedules Linear issues and runs OpenCode ACP in per-issue
worktrees.

[![Symphony demo video preview](.github/media/symphony-demo-poster.jpg)](https://player.vimeo.com/video/1186371009?h=5626e4b899)

> [!WARNING]
> Symphony vNext is an engineering preview for trusted operator environments.

## Active Runtime

Rust vNext is the only active service implementation in this repository. The old Elixir runtime and
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

Rust vNext parks legacy steward states (`Preparing`, `In Review`, `RCA Required`) instead of treating
them as executable runtime aliases.

## Configuration

Use the checked-in sample as the active service shape:

```bash
cargo run -p symphony-vnext -- validate-config --config config/symphony.projects.yml
cargo run -p symphony-vnext -- init-store --database /var/lib/symphony-vnext/runtime.sqlite3
cargo run -p symphony-vnext -- daemon --config config/symphony.projects.yml --database /var/lib/symphony-vnext/runtime.sqlite3
```

Continuous service mode uses the systemd unit template in
`deploy/systemd/symphony-vnext.service` after the operator approves the host cutover. Use `--once`
only for non-live bootstrap validation. Continuous mode requires `LINEAR_API_KEY` so the daemon can
poll and mutate Linear through the Rust adapter. The service reads that key from the existing
file-backed Symphony environment at `/home/agent/.symphony/env/linear.env`; do not duplicate the key
in project workflow files.

## Validation

Default validation does not start OpenCode, mutate Linear, or restart systemd:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Live cutover validation requires host credentials and operator control:

```bash
SYMPHONY_VNEXT_LIVE_OPENCODE_ACP=1 cargo test -p symphony-vnext --test bootstrap \
  installed_opencode_acp_supports_ndjson_config_options_without_prompting -- --nocapture
cargo build --release -p symphony-vnext
/usr/local/bin/opencode acp
systemctl status symphony-vnext.service
curl -fsS http://127.0.0.1:4110/api/dashboard
curl -fsS http://127.0.0.1:4110/api/projects/symphony
```

## Runtime Contract

See [SPEC.md](SPEC.md) for the Rust/OpenCode-only vNext service contract.

## License

This project is licensed under the [Apache License 2.0](LICENSE).
