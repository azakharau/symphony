# Oh My Pi ACP orchestration contract

## Objective

Define how Symphony should orchestrate Oh My Pi ACP as a runtime integration
surface. This is a research artifact for Symphony coding tasks; OpenCode should
receive only implementation issues that reference this contract.

## Source-backed OMP surfaces

Symphony must distinguish these surfaces instead of treating "Oh My Pi" as one
opaque command:

- `omp acp`: OMP as an ACP stdio provider. This is the primary Symphony
  integration direction.
- Hook/extension loading: OMP discovers hook factories as extension modules;
  handlers register through `pi.on(...)`.
- Tool hooks: `tool_call` is pre-execution and can block; `tool_result` is
  post-execution and can replace bounded output content/details.
- Session/context hooks: `context`, `before_agent_start`, `session_start`,
  `session_before_compact`, `session.compacting`, `session_compact`, and
  `session_shutdown` are observation or advisory context surfaces unless a
  specific OMP contract proves stronger enforcement.
- SDK sessions: `createAgentSession()` defaults to file-backed
  `SessionManager.create(cwd)` and can resume/open/list/fork sessions.
- RPC mode: secondary integration surface for process/language isolation, not
  the same contract as the ACP provider path.
- `pi-shell-acp`: inverse bridge reference only. It lets Pi talk to ACP
  backends and must not be conflated with `omp acp`, where external ACP clients
  talk to OMP.

## Symphony ownership boundary

Symphony owns orchestration:

- Linear candidate selection and state writes;
- per-issue worktree allocation;
- runtime process lifecycle;
- ACP session tracking;
- handoff validation and closure;
- dashboard/API telemetry;
- retry, parking, and self-defect routing.

Symphony does not own product-specific runtime policy inside Nervure, Mnemesh,
Neryva, or other configured projects. Project repos may expose their own
adapter semantics, but Symphony must treat them as project code executed by the
OpenCode/OMP runtime, not as Symphony roadmap authority.

## Trust boundary

Symphony may verify and persist source-backed metadata, bounded observations,
and evidence references. It must not treat OMP packages, local plugins, local
session files, or extension output as trusted runtime authority without an
explicit verified enforcement surface.

Required hard boundaries:

- No CLI transcript scraping.
- No OAuth or API-key proxying.
- No implicit `.omp`, `.omp/Pi`, or user OMP config mutation.
- No ambient MCP catalog scanning.
- No trusted local path claim without digest/provenance evidence.
- No strict/native/runtime-authority claim from metadata-only evidence.
- No raw large tool output persistence inside Symphony runtime DB.

## Capability mapping

| OMP surface | Symphony status | Notes |
| --- | --- | --- |
| `omp acp` stdio provider | Implementable | Model command, cwd, session id, ACP lifecycle, and provider/auth failures. |
| `tool_call` hook | Observable via project/runtime integration | Symphony should surface evidence; hard policy belongs to the project/runtime contract. |
| `tool_result` hook | Observable/output evidence | Can explain bounded output replacement if project runtime emits evidence. |
| `context` / `before_agent_start` | Advisory evidence | Useful for dashboard timeline, not a Symphony execution gate by itself. |
| Session/compact hooks | Runtime telemetry | Useful for stale-session detection, resume, and dashboard history. |
| SDK file-backed sessions | Evidence source | Supports persistent session refs and resume/open/list/fork metadata. |
| RPC mode | Secondary adapter mode | Track separately from ACP stdio. |
| `pi-shell-acp` | Reference only | Opposite integration direction; do not depend on it for OMP ACP provider behavior. |

## Failure taxonomy

Implementation tasks must classify failures without collapsing them into owner
input:

- missing OMP binary;
- unsupported OMP version or hook event;
- malformed ACP frame;
- ACP provider/auth unavailable;
- untrusted config path;
- hook package load failure;
- unsupported runtime authority claim;
- local session evidence unavailable;
- live smoke skipped because the explicit opt-in flag is absent.

Owner input is only valid when the implementation has a real product decision
to ask. Tooling/runtime defects and unsupported surfaces must be typed failures
or explicit unsupported status.

## Implementation handoff rules

OpenCode orchestrated coding tasks should receive the relevant Linear issue
spec plus this document path as fixed context. They should not be asked to
choose the global product direction, create new roadmap structure, or target
named subagents. The orchestrator may decompose internally, but the Linear task
is the execution unit.

Every code/schema/doc change must close through normal git evidence: changed
files, validation commands, commit SHA, pushed branch/ref, and any unresolved
risk. A Linear comment alone is not closure evidence.
