# Symphony Operator Runbook

## Services and ports

Symphony runs as two independent user services on the host:

| Unit | Process | Port | Purpose |
| --- | --- | --- | --- |
| `symphony.service` | Rust `symphony daemon` | `127.0.0.1:4115` | API, SSE, Linear polling, runner orchestration |
| `symphony-dashboard.service` | Next production server from `apps/dashboard` | `127.0.0.1:4120` | Operator dashboard UI and BFF routes |

The Rust service no longer serves dashboard HTML. The only supported UI is the Next dashboard.
Keep the two processes and user units separate.

## Environment

Rust service:

- `RUST_LOG=symphony=warn`
- `LINEAR_API_KEY` from `/home/agent/.symphony/env/linear.env`
- config: `/home/agent/.symphony/symphony/projects.toml`
- database: `/home/agent/.symphony/symphony/runtime.sqlite3`

Dashboard service:

- `PORT=4120`
- `SYMPHONY_API_BASE=http://127.0.0.1:4115`
- `OCU_COMMAND=ocu --plain --localhost`
- `OCU_TIMEOUT_MS=5000`
- optional overrides: `/home/agent/.symphony/env/dashboard.env`

Do not place Linear secrets in the dashboard unit or dashboard env file.

## Oh My Pi ACP operations

The OMP integration contract is `docs/oh-my-pi-acp-orchestration-contract.md`.
Treat that document as the source for supported surfaces, trust boundaries, and failure taxonomy.

Configure OMP ACP per project in `/home/agent/.symphony/symphony/projects.toml` with a single
`[[projects.omp_acp_providers]]` block for the project being cut over. Leave unrelated active
projects unchanged. A provider block must declare `id`, absolute `command`, `args = ["acp"]`,
`cwd = "issue_worktree"` or `cwd = "project_repo"`, an explicit `env_allowlist`, optional
`agent`/`model`/`effort`, and capabilities with `acp_stdio = true`. Keep `live_smoke = false`
unless the operator is deliberately running live OMP validation.

Supported Symphony surfaces:

- `omp acp` as the stdio provider for one configured project.
- Per-issue worktree or project-repo cwd selection from config.
- ACP initialize/session lifecycle, session id, process id, frame count, provider id, and bounded session evidence refs.
- Dashboard/API telemetry for `provider_mode = "omp_acp"`, runtime failure kind, silence markers, and session evidence refs.

Unsupported or observation-only surfaces:

- No CLI transcript scraping, OAuth/API-key proxying, user OMP config mutation, ambient MCP catalog scanning, or implicit `.omp` path trust.
- Hook and extension output is evidence only unless a project contract proves stronger enforcement.
- RPC mode and `pi-shell-acp` are separate surfaces and must not be treated as the `omp acp` provider path.
- Large raw tool output must not be persisted in the Symphony runtime DB.

Failure taxonomy to preserve in operator reports:

- Missing OMP binary.
- Unsupported OMP version or hook event.
- Malformed ACP frame.
- ACP provider/auth unavailable.
- Untrusted config path.
- Hook package load failure.
- Unsupported runtime authority claim.
- Local session evidence unavailable.
- Live smoke skipped because the explicit opt-in flag is absent.

Opt-in live OMP ACP smoke is disabled by default so normal tests do not need network, installed
OMP, or provider quota. Run it only when an operator has already configured a disposable OMP
command and approved the check:

```bash
SYMPHONY_LIVE_OMP_ACP=1 \
SYMPHONY_LIVE_OMP_COMMAND=/absolute/path/to/omp \
cargo nextest run -p symphony --test bootstrap live_omp_acp_smoke_starts_session_when_explicitly_enabled
```

If OMP needs non-default ACP args, add `SYMPHONY_LIVE_OMP_ACP_ARGS="acp"`. The smoke uses a temp
cwd, sends only `initialize` and `session/new`, checks the optional `--version` output when
available, bounds session evidence refs, and kills the child process. It must not edit user OMP
config.

## Install or update

Run service restarts only inside an operator-approved safe restart window.

```bash
cd /home/agent/proj/symphony
cargo build --release -p symphony
install -Dm0755 target/release/symphony /home/agent/.cargo/bin/symphony
install -Dm0644 config/symphony.projects.toml /home/agent/.symphony/symphony/projects.toml

cd /home/agent/proj/symphony/apps/dashboard
bun install --frozen-lockfile
SYMPHONY_API_BASE=http://127.0.0.1:4115 OCU_COMMAND="ocu --plain --localhost" bun run build

cd /home/agent/proj/symphony
install -Dm0644 deploy/systemd/symphony.service /home/agent/.config/systemd/user/symphony.service
install -Dm0644 deploy/systemd/symphony-dashboard.service /home/agent/.config/systemd/user/symphony-dashboard.service
systemctl --user daemon-reload
systemctl --user enable symphony.service symphony-dashboard.service
systemctl --user restart symphony.service
systemctl --user restart symphony-dashboard.service
```

If an unmanaged `symphony daemon` process is already bound to `4115`, stop it before starting
`symphony.service` so there is only one runtime process.

## Independent restart commands

Restart dashboard only:

```bash
systemctl --user restart symphony-dashboard.service
systemctl --user is-active symphony.service
systemctl --user is-active symphony-dashboard.service
curl -fsS http://127.0.0.1:4115/api/dashboard >/tmp/symphony-api-smoke.json
curl -fsSI http://127.0.0.1:4120/
```

Restart Rust runtime only:

```bash
systemctl --user restart symphony.service
systemctl --user is-active symphony.service
systemctl --user is-active symphony-dashboard.service
curl -fsS http://127.0.0.1:4115/api/dashboard >/tmp/symphony-api-smoke.json
curl -fsSI http://127.0.0.1:4120/
```

## Smoke checks

Rust API and SSE:

```bash
curl -fsS -H 'Accept: application/json' http://127.0.0.1:4115/api/dashboard | python3 -m json.tool >/tmp/symphony-dashboard-api.json
curl -fsSI http://127.0.0.1:4115/api/dashboard
curl -fsS http://127.0.0.1:4115/api/dashboard/events | head -5
```

Rust root must not serve HTML:

```bash
curl -sS -D /tmp/symphony-root.headers http://127.0.0.1:4115/ -o /tmp/symphony-root.body || true
cat /tmp/symphony-root.headers
python3 - <<'PY'
from pathlib import Path
headers = Path('/tmp/symphony-root.headers').read_text()
body = Path('/tmp/symphony-root.body').read_text(errors='ignore')
assert 'text/html' not in headers.lower()
assert '<html' not in body.lower()
print(headers.splitlines()[0])
PY
```

Dashboard service:

```bash
curl -fsSI http://127.0.0.1:4120/
curl -fsS http://127.0.0.1:4120/ | head
curl -fsS http://127.0.0.1:4120/api/dashboard | python3 -m json.tool >/tmp/symphony-dashboard-bff.json
```

Dashboard degraded and reconnect behavior while Rust is unavailable:

```bash
systemctl --user stop symphony.service
systemctl --user is-active symphony-dashboard.service
curl -fsSI http://127.0.0.1:4120/
curl -sS http://127.0.0.1:4120/ | grep -Ei 'unavailable|degraded|reconnect|Dashboard unavailable'
systemctl --user start symphony.service
until curl -fsS http://127.0.0.1:4115/api/dashboard >/dev/null; do sleep 1; done
curl -fsS http://127.0.0.1:4120/api/dashboard | python3 -m json.tool >/tmp/symphony-dashboard-reconnected.json
```

Multiproject activation checks remain API-first:

```bash
/home/agent/.cargo/bin/symphony validate-config --config config/symphony.projects.toml
curl -fsS http://127.0.0.1:4115/api/projects/symphony | python3 -m json.tool | sed -n '/"eligible"/,/"blockers"/p'
curl -fsS http://127.0.0.1:4115/api/projects/recall | python3 -m json.tool | sed -n '/"eligible"/,/"blockers"/p'
```

OMP ACP dashboard expectations:

- The configured project should show `provider_mode` as `omp_acp` on running issue/session rows.
- `provider_id`, `process_id`, `acp_frame_count`, and `session_evidence_refs` should be present when a session starts.
- Auth, malformed-frame, missing-binary, and unsupported-version failures should appear as typed runtime failures rather than owner-input blockers.
- Unrelated projects should continue to show their existing runner provider mode and should not inherit the OMP provider block.

## OMP ACP cutover and cleanup

Cut over one project at a time:

- Add or enable one `omp_acp_providers` block for the target project only.
- Run `symphony validate-config --config /home/agent/.symphony/symphony/projects.toml` before restart.
- Restart only `symphony.service` during a safe restart window; do not restart the dashboard unless its own deployment changed.
- Confirm `/api/projects/<project>` eligibility and `/api/dashboard` telemetry before allowing new issue starts.
- Leave existing unrelated active projects and sessions on their prior provider until they are separately cut over.

Safe restart and cleanup notes:

- Prefer parking or letting in-flight issue sessions finish before switching a project's provider.
- If an OMP ACP startup fails before session attachment, Symphony should terminate the process tree and surface the typed failure.
- For a stuck OMP child, stop `symphony.service`, verify no unrelated project child is being killed, then terminate only the process whose cleanup marker matches the target provider/issue/cwd.
- Do not delete `.omp`, `.omp/Pi`, or user-level OMP configuration as part of Symphony cleanup.
- Preserve per-issue worktrees and runtime SQLite evidence for postmortem unless an operator explicitly approves removal.

## Rollback

Rollback the services independently:

```bash
systemctl --user stop symphony-dashboard.service
systemctl --user restart symphony.service
```

If the Rust service cannot run, leave active issues parked in Linear and recover from the SQLite store
plus per-issue runner worktrees. Do not restore removed legacy UI routes.
