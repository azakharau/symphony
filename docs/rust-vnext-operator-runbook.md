# Rust vNext Operator Runbook

## Service

Systemd template: `deploy/systemd/symphony-vnext.service`

Install/update:

```bash
install -Dm0644 deploy/systemd/symphony-vnext.service /etc/systemd/system/symphony-vnext.service
systemctl daemon-reload
systemctl restart symphony-vnext.service
systemctl status symphony-vnext.service
```

The template points at `config/symphony.projects.yml` and
`/var/lib/symphony-vnext/runtime.sqlite3`. Systemd creates the data directory through
`StateDirectory=symphony-vnext`. The service reads `LINEAR_API_KEY` from
`/home/agent/.symphony/env/linear.env`; do not copy the key into workflow files or checked-in
configuration.

## Smoke Checks

Local non-live checks:

```bash
cargo run -p symphony-vnext -- validate-config --config config/symphony.projects.yml
cargo run -p symphony-vnext -- init-store --database /tmp/symphony-vnext-smoke.sqlite3
cargo run -p symphony-vnext -- daemon --config config/symphony.projects.yml --database /tmp/symphony-vnext-smoke.sqlite3 --once
```

Live checks:

```bash
/usr/local/bin/opencode acp
curl -fsS http://127.0.0.1:4110/api/dashboard
curl -fsS http://127.0.0.1:4110/api/projects/symphony
```

## Rollback

Rollback is Rust-service-only:

```bash
systemctl stop symphony-vnext.service
cp /var/lib/symphony-vnext/runtime.sqlite3 /var/lib/symphony-vnext/runtime.sqlite3.rollback
systemctl start symphony-vnext.service
```

Do not restart or restore the removed Elixir runtime. If the Rust service cannot run, leave active
issues parked in Linear and recover from the SQLite store plus per-issue OpenCode worktrees.

## Runtime Notes

Continuous mode runs the poll loop and dashboard API until the service is stopped. Use `--once` only
for local bootstrap validation.
