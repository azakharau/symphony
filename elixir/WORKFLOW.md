---
tracker:
  kind: linear
  project_slug: "87b3b7431580"
  active_states:
    - Todo
    - Preparing
    - In Progress
    - In Review
    - "Need Owner Input"
    - "RCA Required"
  terminal_states:
    - Done
    - Canceled
    - Duplicate
polling:
  interval_ms: 30000
  full_interval_ms: 30000
  fast_states:
    - Todo
    - Preparing
    - In Progress
    - "Need Owner Input"
workspace:
  root: /home/agent/.symphony/workspaces/codex/symphony
hooks:
  after_create: |
    git clone --branch agent-server/opencode-runner-extension --single-branch git@github.com:azakharau/symphony.git .
agent:
  max_concurrent_agents: 1
  max_turns: 4
runner:
  default: codex
  routes:
    Todo: codex
    Preparing: codex
    "In Review": codex
    "Need Owner Input": codex
    "RCA Required": codex
    "In Progress": codex
opencode:
  protocol: acp
  command: /usr/local/bin/opencode
  args:
    - acp
  project_root: /home/agent/proj/symphony
  server_url: null
  agent: build
  model: openai/gpt-5.5
  format: json
  result_state: "In Review"
  timeout_ms: 10800000
  read_timeout_ms: 30000
  stall_timeout_ms: 0
  permission_policy: reject

OpenCode live validation gate:

- Default `mix test` is deterministic and excludes the live OpenCode gate; it must not require `/usr/local/bin/opencode`, an OpenCode web server, or live Linear.
- Run the opt-in adapter gate from `/home/agent/proj/symphony/elixir` with `SYMPHONY_OPENCODE_LIVE=1 OPENCODE_COMMAND=/usr/local/bin/opencode mix test.opencodelive`.
- To link handoff evidence to a visible OpenCode Web session, also set `OPENCODE_SERVER_URL=http://127.0.0.1:3000`; ACP still runs as the stdio command `/usr/local/bin/opencode acp`, and the URL is recorded only as attach metadata.
- The gate uses an isolated memory tracker, explicit test-only OpenCode routing, an evidence-only no-edit prompt, and `git status --short` before/after protection. It validates the OpenCode adapter without changing the production bootstrap route, which remains Codex-only for SYM vNext work.
- Cleanup is limited to test-owned temporary workspaces. If the gate fails, interpret the command output as local OpenCode/server/session evidence and record the exact command, result, attach URL, session id when present, and any Mnemesh evidence refs on the Linear/Mnemesh validation record.
- `/home/agent/proj/symphony` is the canonical live gate project root. Do not use vendor aliases for live validation or service configuration.

process_policy:
  rca_required_state: "RCA Required"
  max_rejections_per_slice: 2
  timeout_state: "Need Owner Input"
  state_timeouts_ms:
    "In Review": 1800000
    "RCA Required": 1800000
codex:
  command: /home/agent/.symphony/bin/codex-ws-stdio-proxy
  read_timeout_ms: 10800000
  turn_timeout_ms: 10800000
  approval_policy: never
  thread_sandbox: danger-full-access
  turn_sandbox_policy:
    type: dangerFullAccess
---

Symphony steward workflow for the Symphony project. This file describes the current human
stewardship process used around the Elixir prototype; it is not the Rust vNext runtime contract.
`../SPEC.md` is authoritative for vNext and removes Codex from runtime routes, adapters, acceptance,
RCA, and fallback execution.

Codex session contract (current human steward process only):

- Symphony must start issue-scoped Codex threads from the per-issue workspace.
- Do not resume the main project Machine Architect thread for Symphony execution packets.
- Run Codex turns with the issue workspace as cwd/project root, not `/home/agent/proj/symphony`.
- Keep canonical repo paths as task context only; do not let Codex UI sessions accumulate under the main project.

Project identity:

- Linear team: `SYM` / `SYMPHONY`.
- Linear project: `symphony`, `projectId=07df87ce-4e93-4d2c-a73d-84aee1f27e07`, `project_slug=87b3b7431580`.
- Canonical repository checkout: `/home/agent/proj/symphony`.
- Elixir app root: `/home/agent/proj/symphony/elixir`.
- Current working branch for this project: `agent-server/opencode-runner-extension`.

Role boundary:

- This Elixir workflow is the temporary bootstrap surface for developing Symphony vNext only.
- Codex owns implementation, validation, acceptance/rejection, RCA, docs/runbooks, git closure, and Linear hygiene for current Symphony vNext development tasks.
- OpenCode is not used by this bootstrap workflow to execute, repair, or accept Symphony vNext work.
- Codex may edit application code when the current SYM issue requires it, while preserving unrelated dirty/user changes.
- Do not change the vNext product contract from this workflow file: `../SPEC.md` remains the target Rust/OpenCode-only runtime contract.
- Do not choose new product milestones or seed top-level backlog work. Work only inside Linear milestones and issues already curated by the owner/CTO.

Milestone rules:

- Do not keep workflow-local milestone pointers.
- Linear is the milestone control plane: milestone ordering/status, issue priority, issue state, and explicit blockers define execution order.
- Work one eligible issue at a time from the next nonterminal milestone that has unblocked work in `Todo`, `Preparing`, `In Review`, `RCA Required`, or `Need Owner Input`.
- If all issues in the current milestone are terminal or blocked by nonterminal dependencies, do not synthesize replacement work; report the blocked/exhausted state and wait for owner/CTO backlog changes.
- Milestone descriptions are product context only; never parse them as runtime state.
- `phase_state:*` text has no runtime effect and must not gate dispatch.
- Do not scan, rank, promote, or synthesize new milestones.

State contract (current bootstrap process only; vNext uses Symphony-owned Linear writes plus
OpenCode ACP implementation/eval/repair):

- `Todo`: queued work only. Symphony promotes one eligible issue to `Preparing`; Codex must not run while the issue is still `Todo`.
- `Preparing`: Codex-owned start gate. Verify the Linear milestone and blockers, confirm the issue spec is executable, move the same issue to `In Progress`, and continue through Codex when coding is required.
- `In Progress`: Codex-owned implementation. Inspect the current repo/worktree state, implement the scoped change, run relevant validation, and move to `In Review` only with concrete evidence.
- `In Review`: Codex-owned acceptance. Inspect the diff and validation evidence directly; post one marked review decision; then accept/close, reject, ask owner, or route to RCA.
- `RCA Required`: Codex-owned RCA and repair. Identify root cause first, then implement the systemic repair in Codex when it is in scope.
- `Need Owner Input`: read the latest owner-visible comment, apply the owner decision if present, otherwise keep it parked.
- `Done`, `Canceled`, and `Duplicate` are terminal.

Issue context:

- Identifier: {{ issue.identifier }}
- Title: {{ issue.title }}
- Status: {{ issue.state }}
- URL: {{ issue.url }}
- Labels: {{ issue.labels }}

Description:
{% if issue.description %}
{{ issue.description }}
{% else %}
No description provided.
{% endif %}

{% if attempt %}
Continuation context:

- This is retry/continuation attempt #{{ attempt }}.
- Resume from existing Linear, repo, worktree, and runner state. Do not restart completed implementation work or create duplicate execution comments.
{% endif %}

Hard process guards:

- Treat `slice_id` as the stable implementation-slice identity across review and repair attempts.
- A first rejection for a slice may produce one scoped repair prompt using the same `slice_id`.
- A second rejection for the same `slice_id` must move the issue to `RCA Required`. No third point-repair prompt is allowed.
- In `RCA Required`, produce the RCA first, then create a fundamentally redesigned implementation prompt with a new `slice_id` if coding is still appropriate.
- Do not optimize benchmark behavior for benchmark-specific issue names, paths, or fixtures.
- Preserve existing dirty/unrelated user changes. Never reset, checkout, or clean unrelated files.

Codex execution contract (current bootstrap process only; vNext plain issue specs use the sections
`Context`, `Change`, `Acceptance Criteria`, `Validation`, `Handoff`, and `Non-goals`):

- Work from the issue workspace that Symphony creates for the SYM issue.
- Use concise internal task framing from the Linear spec; do not create or repost OpenCode implementation prompts.
- Do not start OpenCode ACP, rely on OpenCode session state, or treat OpenCode handoff comments as the required execution path for Symphony vNext development.
- For code changes, inspect the current diff, implement the scoped change, run issue-relevant validation, and record exact command outcomes.
- Preserve unrelated dirty/user changes. Never reset, checkout, or clean unrelated files.

Review decision contract:

- Every `In Review` decision must include a Linear comment with this marker before any state transition:

<!-- symphony:review-decision:v1 -->
```text
status: accepted|rejected|needs_owner_input|rca_required
slice_id: <same stable slice_id, or none if no implementation slice exists>
reason: <one concise reason>
```

- `status: rejected` is counted by Symphony for the matching `slice_id`.
- After two rejected decisions for the same `slice_id`, Symphony must route to `RCA Required` rather than another repair.

Validation and closure:

- For Symphony code changes, run the validation commands specified by the issue. At minimum prefer targeted `mix test`, `mix format --check-formatted`, `mix specs.check`, and `git diff --check` when relevant.
- Inspect the diff directly before accepting work. Tests are supporting evidence, not acceptance by themselves.
- Codex owns final git stage/commit/push when the issue requires closure and validation is sufficient.
- Do not stop live per-project services, enable the new multiproject service, or mutate systemd cutover state unless the issue explicitly says that approval was granted.
- If the issue asks for cutover preparation, produce templates/runbooks and validation evidence only.
