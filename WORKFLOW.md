# Symphony vNext Workflow

This file is the project-local policy target referenced by the Rust vNext multiproject config.

## Active Runtime

- Runtime: Rust `symphony-vnext`.
- Implementation runner: OpenCode ACP only.
- Active milestone: `05. Rust vNext: OpenCode-only multiproject Symphony`.
- Active milestone id: `7a04f8cf-dece-48b9-a2ec-0356ed639943`.

## State Policy

- `Backlog` is planning inventory and is not executable.
- `Todo` is the queued executable state.
- `In Progress` is the OpenCode-owned implementation state.
- `Need Owner Input` is parked owner/provider/blocker state.
- `Done`, `Canceled`, `Cancelled`, `Closed`, and `Duplicate` are terminal.

Legacy steward states (`Preparing`, `In Review`, and `RCA Required`) are not active runtime states.
The Rust runtime parks them in `Need Owner Input` with typed evidence if they appear in the active
queue.

## Validation

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Live validation requires operator-approved host access to OpenCode ACP, Linear credentials, the
systemd service, and dashboard/API endpoints.
