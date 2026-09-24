# Symphony Workflow

Workflow policy for projects scheduled by the Rust `symphony` runtime.

## Active Runtime

- Runtime: Rust `symphony`.
- Implementation runner: OpenCode ACP by default (`[projects.runner].provider_mode = "acp"`). OMP ACP
  is opt-in per project (`provider_mode = "omp_acp"` plus a matching `[[projects.omp_acp_providers]]`
  block); see [SPEC.md](SPEC.md).

## Workflow Configuration

Workflow policy (state names, stage agents, owner-input and self-defect policy) comes from the root
config `[workflow]` table, falling back to built-in defaults. A project may override it with an
optional TOML file at `workflow_path` (default `workflow.toml`, relative to `repo_path`); when that
file is absent the project uses the root workflow unchanged. No project-local workflow file is
required.

## State Policy

State names below are the defaults; each stage maps to the configured `workflow.states` name.

- `Backlog` is planning inventory and is not executable.
- `Todo` is the queued executable state.
- `In Progress` is the runner-owned implementation state.
- `In Review` is the executable review stage for implementation handoff evidence.
- `Need Owner Input` is parked only for real owner/product/permission questions that need a human
  decision. Provider/runtime blockers are recorded as typed `provider_blocker` evidence, not moved
  here.
- `Done`, `Canceled`, `Cancelled`, `Closed`, and `Duplicate` are terminal.

Managed Symphony self-defect issues are created in the managed project's configured `todo` state
(P0) or `backlog` state (P1/P2); when `backlog` is disabled, P1/P2 use the configured `todo` state.
In `todo`, P1/P2 self-defects (Linear priority 2/3) are blocked from dispatch unless the issue
carries the configured `workflow.self_defects.executable_label` (default `self-defect-executable`).

Legacy steward states (`Preparing` and `RCA Required`) are not active runtime states and are never
queried as dispatch candidates.

## Validation

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
cd apps/dashboard && bun install --frozen-lockfile && bun run lint && bun run typecheck && bun run test && bun run build
```

Live validation requires operator-approved host access to OpenCode/OMP ACP, Linear credentials, the
systemd service, and dashboard/API endpoints.
