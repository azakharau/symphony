# Symphony Operator Runbook

## Services and ports

Symphony runs as two independent user services on the host:

| Unit | Process | Port | Purpose |
| --- | --- | --- | --- |
| `symphony.service` | Rust `symphony daemon` | `127.0.0.1:4115` | API, SSE, Linear polling, OpenCode orchestration |
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
curl -fsS http://127.0.0.1:4115/api/projects/mnemesh | python3 -m json.tool | sed -n '/"eligible"/,/"blockers"/p'
```

## Rollback

Rollback the services independently:

```bash
systemctl --user stop symphony-dashboard.service
systemctl --user restart symphony.service
```

If the Rust service cannot run, leave active issues parked in Linear and recover from the SQLite store
plus per-issue OpenCode worktrees. Do not restore removed legacy UI routes.
