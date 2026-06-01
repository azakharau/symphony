# OpenCode active-service migration runbook

This runbook records read-only evidence and operator checklists for migrating active Symphony project services to the OpenCode-capable runner path. It is preparation only: do not cut over, restart, reload, enable, disable, kill, or otherwise mutate services until the owner explicitly approves the per-service action.

## Source configuration checked

- `AGENTS.md` requires runtime config to come from `WORKFLOW.md` front matter and warns that workspace/orchestrator safety matters during validation.
- `AGENTS.md` keeps validation centered on targeted checks first and `mix specs.check` for full Elixir validation when code changes require it.
- `WORKFLOW.md` identifies the operator-visible project root as `/home/agent/proj/symphony`, the Elixir app root as `/home/agent/proj/symphony/elixir`, and the current branch as `agent-server/opencode-runner-extension`.
- `WORKFLOW.md` sets `runner.default: codex`, routes `In Progress` to `opencode`, and configures OpenCode with `/usr/local/bin/opencode`, `http://127.0.0.1:3000`, agent `build`, JSON format, and `result_state: "In Review"`.
- As of 2026-06-01, the physical checkout lives at `/home/agent/proj/symphony`; `/home/agent/.symphony/vendor/openai-symphony` is a compatibility symlink for older service paths.

## Read-only active-service inventory

Inventory timestamp: `2026-05-31T22:35Z`.

Commands used:

```bash
systemctl list-units 'openai-symphony*.service' --type=service --all --no-pager
systemctl show 'openai-symphony*.service' --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager
ss -ltnp '( sport = :3000 or sport = :4111 or sport = :4112 or sport = :4113 or sport = :4114 or sport = :4115 )'
```

Observed loaded active running units:

| Unit | User | Working directory | Workflow path from `ExecStart` | Port evidence |
| --- | --- | --- | --- | --- |
| `openai-symphony-agent-forge.service` | `agent` | `/home/agent/.symphony/vendor/openai-symphony/elixir` | `/home/agent/proj/neryva-agent-forge/WORKFLOW.md` | `127.0.0.1:4114` listening as `beam.smp` |
| `openai-symphony-mnemesh.service` | `agent` | `/home/agent/.symphony/vendor/openai-symphony/elixir` | `/home/agent/proj/mnemesh/WORKFLOW.md` | `127.0.0.1:4111` listening as `beam.smp` |
| `openai-symphony-nervure.service` | `agent` | `/home/agent/.symphony/vendor/openai-symphony/elixir` | `/home/agent/proj/nervure/WORKFLOW.md` | `127.0.0.1:4112` listening as `beam.smp` |
| `openai-symphony-neryva.service` | `agent` | `/home/agent/.symphony/vendor/openai-symphony/elixir` | `/home/agent/proj/neryva/WORKFLOW.md` | `127.0.0.1:4113` listening as `beam.smp` |
| `openai-symphony-symphony.service` | `agent` | `/home/agent/.symphony/vendor/openai-symphony/elixir` | `/home/agent/.symphony/vendor/openai-symphony/elixir/WORKFLOW.md` | `127.0.0.1:4115` listening as `beam.smp` |

Additional scoped listener evidence:

- OpenCode WebUI/API was listening on `127.0.0.1:3000` as process `opencode`, PID `2427937`.
- The scoped listener check only covered ports `3000` and `4111` through `4115`; it does not prove whether any other listeners existed or did not exist.

## OpenCode validation evidence

Validation timestamp: `2026-05-31T22:35Z`.

Commands used:

```bash
command -v /usr/local/bin/opencode
/usr/local/bin/opencode --version
curl -fsS -I http://127.0.0.1:3000
curl -fsS http://127.0.0.1:4115/api/v1/state | python3 -m json.tool
systemctl show openai-symphony-symphony.service --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User,ControlGroup --no-pager
systemctl status openai-symphony-symphony.service --no-pager --lines=20
curl -sS -D - --max-time 5 http://127.0.0.1:3000/
curl -fsS --max-time 5 http://127.0.0.1:3000/api/session
```

Observed results:

- `command -v /usr/local/bin/opencode` returned `/usr/local/bin/opencode`.
- `/usr/local/bin/opencode --version` returned `1.15.12`.
- `curl -fsS -I http://127.0.0.1:3000` returned `HTTP/1.1 200 OK`.
- `curl -sS -D - --max-time 5 http://127.0.0.1:3000/` returned `200` HTML.
- `curl -fsS --max-time 5 http://127.0.0.1:3000/api/session` returned a session whose first entry had id `ses_17fd34ed5ffex8JZNxQsBc2oVJ`, agent `build`, title `SYM-10 OpenCode active-service migration and validation`, model `gpt-5.5/openai`, and project ID `c3f56ec783874ee799140cba4e08ca35d48a801e`.

Additional SYM-10 route evidence from read-only probes:

- `curl -fsS http://127.0.0.1:4115/api/v1/state | python3 -m json.tool` returned state generated at `2026-05-31T22:35:06Z`; `running` contained `SYM-10` in `In Progress`, started at `2026-05-31T22:34:21Z`, with workspace `/home/agent/.symphony/workspaces/codex/symphony/SYM-10`, issue id `08aea6f6-f4b1-4661-a576-f17cd0f99662`, and `retrying` contained `SYM-5` due to no available orchestrator slots.
- `systemctl show` for `openai-symphony-symphony.service` confirmed `LoadState=loaded`, `ActiveState=active`, `SubState=running`, user `agent`, working directory `/home/agent/.symphony/vendor/openai-symphony/elixir`, and control group `/system.slice/openai-symphony-symphony.service`.
- `systemctl status openai-symphony-symphony.service --no-pager --lines=20` showed child process `/usr/local/bin/opencode run --dir /home/agent/proj/symphony --agent build --format json --title "SYM-10 OpenCode active-service migration and validation" --attach http://127.0.0.1:3000`, and the service dashboard listed `SYM-10` as `In Progress` at age about `0m57s`.

This evidence proves the current `SYM-10` `In Progress` run used the configured OpenCode route as far as non-mutating local service, runtime, HTTP, and session evidence can prove. It does not prove cutover readiness, full lifecycle completion, Linear transition behavior, rollback safety, or readiness to migrate any active service.

## Elixir validation evidence

Validation timestamp: `2026-05-31T22:49Z`.

Commands run from `/home/agent/.symphony/vendor/openai-symphony/elixir`:

```bash
mix test test/symphony_elixir/opencode_runner_test.exs
mix test test/symphony_elixir/orchestrator_status_test.exs
mix format --check-formatted
mix specs.check
git diff --check
```

Observed results:

- `mix test test/symphony_elixir/opencode_runner_test.exs` passed: `31 tests, 0 failures`.
- `mix test test/symphony_elixir/orchestrator_status_test.exs` passed: `48 tests, 0 failures`.
- `mix format --check-formatted` passed.
- `mix specs.check` passed with `specs.check: all public functions have @spec or exemption`.
- `git diff --check` passed.

## Migration prerequisites and readiness gates

Each service must pass these gates before any owner-approved migration step:

1. Owner approval exists for the named service and exact action.
2. No active issue run is in a critical section for the service being changed, or the owner explicitly accepts that risk.
3. The service's current `WORKFLOW.md` is reviewed and its intended runner routes are known.
4. The OpenCode command and server URL configured for that service are reachable with the validation commands above.
5. A service-specific rollback target is written down, including the previous `WORKFLOW.md` runner settings and the exact current unit name.
6. The operator has current read-only evidence for `systemctl list-units`, `systemctl show`, and scoped listening ports.
7. The operator has a service-specific validation plan for post-change issue processing and log review.
8. Dirty or unrelated repository changes are identified and preserved.
9. Multiproject impact is understood and owner-visible: multiproject work remains blocked until migration completes or an owner-visible blocker is recorded.

Readiness is blocked if any gate is unknown. Unknown readiness must be recorded as a risk or blocker, not treated as approval.

## Per-service migration checklist template

Use one copy of this checklist per service.

- Service name: `<openai-symphony-*.service>`
- Owner approval reference: `<comment/ticket/time>`
- Pre-change inventory timestamp: `<UTC timestamp>`
- Current workflow path: `<path from systemctl show ExecStart>`
- Current runner default/routes: `<from that service's WORKFLOW.md>`
- Target runner default/routes: `<intended OpenCode-capable route>`
- OpenCode binary check:
  - Command: `command -v /usr/local/bin/opencode && /usr/local/bin/opencode --version`
  - Result: `<pass/fail/output>`
- OpenCode endpoint check:
  - Command: `curl -fsS -I --max-time 5 http://127.0.0.1:3000`
  - Result: `<pass/fail/output>`
- Pre-change service check:
  - Command: `systemctl show '<service>' --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager`
  - Result: `<pass/fail/output>`
- Pre-change scoped listener check:
  - Command: `ss -ltnp '( sport = :3000 or sport = :4111 or sport = :4112 or sport = :4113 or sport = :4114 or sport = :4115 )'`
  - Result: `<pass/fail/output>`
- Planned change: `<workflow/config/service action>`
- Cutover command(s): `<leave blank until owner approval>`
- Post-change validation:
  - Command(s): `<service-specific read-only checks and issue lifecycle validation>`
  - Result: `<pass/fail/output>`
- Decision: `<accepted/rollback/blocked>`
- Notes: `<residual risks>`

## Rollback checklist template

Use this checklist only after owner-approved cutover work has started and rollback is needed.

1. State the rollback trigger: `<failed validation, endpoint unavailable, issue lifecycle failure, owner decision>`.
2. Preserve evidence before rollback:
   - `systemctl show '<service>' --property=Id,LoadState,ActiveState,SubState,FragmentPath,ExecStart,WorkingDirectory,User --no-pager`
   - Relevant service logs or issue lifecycle evidence, using read-only commands.
3. Restore the previous service-specific runner/workflow settings from the recorded rollback target.
4. Run only owner-approved service mutation commands needed for rollback.
5. Verify the restored service state with read-only inventory commands.
6. Verify the issue lifecycle or queue behavior that originally justified rollback.
7. Record the rollback result and remaining blocker for owner review.

## No-cutover policy

No cutover is authorized by this runbook. Until owner approval is explicit for a named service and exact action, operators must not run service or process mutation commands such as `systemctl start`, `stop`, `restart`, `reload`, `enable`, `disable`, `daemon-reload`, `kill`, `pkill`, `docker compose up/down`, or equivalents.

## Known blockers and residual risks

- Owner approval for active-service cutover was not part of this docs-only slice.
- The current inventory shows five active `openai-symphony*.service` units; changing any active service can affect live issue processing.
- The inventory was read-only and timestamped; service state may drift after `2026-05-31T22:35Z`.
- Local OpenCode runtime evidence was collected, including version, HTTP reachability, `4115` state, service status, and `/api/session` route checks; full end-to-end issue lifecycle and cutover readiness remain unproven by this runbook.
- At the 2026-05-31 inventory time, the active `openai-symphony-symphony.service` used `/home/agent/.symphony/vendor/openai-symphony/elixir/WORKFLOW.md` in its `ExecStart`, while the operator-visible repo path was `/home/agent/proj/symphony/elixir`. As of 2026-06-01, the vendor path is a compatibility symlink back to the physical `/home/agent/proj/symphony` checkout.
- The scoped port check only proves listener state for `3000` and `4111` through `4115`; it intentionally does not claim anything about other ports.
- `SYM-5` was retrying because no orchestrator slots were available in the 22:35Z state response, so capacity and scheduling behavior remain a residual risk for migration timing.
- Multiproject work remains blocked until this migration completes or an owner-visible blocker is recorded.
