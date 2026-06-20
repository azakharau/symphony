# Symphony Dashboard Observability Console

Design gate for `SYM-95`.

## Goal

Move the dashboard experience out of the Rust runtime and define the product
contract for a standalone `apps/dashboard` Next.js App Router console. Rust
Symphony remains API/runtime-only. The dashboard is an operator console for
seeing what is running, what is blocked, how quota is trending, and how
OpenCode agents and subagents are behaving.

The dashboard must not show billing amounts. Rust API payloads may still contain legacy
billing fields until later contract cleanup, but Next BFF responses and UI
surfaces intended for the dashboard must omit them.

## Visual Direction

Use a dense, list/table-first observability UI. Project cards are not the
primary navigation pattern because operators compare many projects, statuses,
capacity slots, blockers, defects, and last events at once. Tables make
scanning, sorting, filtering, and drilldown faster than cards.

The style should be quiet and operational: compact status strips, readable
tables, narrow badges, progress bars, persistent navigation, and interactive
inspectors. Avoid marketing layout, oversized hero sections, decorative cards,
and large unstructured text blocks.

## Information Architecture

Top-level tabs:

- `Overview`
- `Projects`
- `Quota`
- `Defects`

Drilldowns:

- Project detail: `/projects/[projectId]`
- Issue detail / OpenCode session inspector:
  `/projects/[projectId]/issues/[issueId]`

## Page Contracts

### Overview

Purpose: one-screen operations triage.

Desktop layout:

```text
[Top nav: Overview | Projects | Quota | Defects] [Refresh/SSE status]

[Running] [Capacity] [5h quota] [Blockers] [Defects]

Running now table
project | issue | stage | agent/model | tokens | tools | last event | worktree

Blockers table
project | issue | blocker kind | message | age/last event | action

Project status table
project | health | running | blocked | capacity | primary reason | last event
```

Mobile layout:

```text
[Nav as horizontal tabs]
[Status summary stacked]
[Running now list]
[Blockers list]
[Project status compact rows]
```

States:

- Empty: no running sessions, no blockers, project rows still show idle reason.
- Running: running table is first, with issue links and current agent/model.
- Blocked: blocker rows are visible above project table.
- Failed/runtime-defect: defect badge and next action are prominent.
- Quota unavailable: quota summary shows unavailable state, not stale zeros.
- Quota normal: 5h usage bar appears in summary, weekly stays on Quota page.

### Projects

Purpose: compare all projects quickly.

Desktop table columns:

```text
project | health | enabled | running/slots | active | blocked | terminal |
primary reason | last event | cleanup
```

Rows link to project detail. Provide filters for health, enabled, and blocked.

Mobile uses compact rows with project name, health badge, capacity, primary
reason, and last event.

### Project Drilldown

Purpose: answer why a specific project is running, idle, blocked, or unhealthy.

Sections:

- Runtime health and capacity.
- Current execution table.
- Queue and blockers.
- Recent run history.
- Defects related to this project.

Current execution columns:

```text
issue | stage | active agent/model | process | tokens | tools | todos |
last event | worktree
```

### Issue Drilldown / OpenCode Session Inspector

Purpose: replace raw HTML transcript walls with interactive operational views.

Header:

```text
issue id/title | display status | lifecycle stage | active agent/model |
process state | last event | worktree | git branch/head
```

Inspector tabs:

- `Todos`: OpenCode todo list grouped by status and priority. Operators can
  filter `pending`, `in_progress`, and `done`, and inspect updated time and
  owning session.
- `Timeline`: compact event feed from OpenCode parts. Filter by root session,
  subagent, tool, kind, and status. Show summaries by default; raw payloads are
  hidden behind an explicit expand action.
- `Agents`: root session and subagent tree. Show parent/child relation, title,
  agent, model, updated time, token split, and activity. Include simple
  subagent insights such as count, stale subagents, most recently active
  subagent, and token-heavy subagent.
- `Tools`: running, pending, and recent tool events with status and summary.
- `Messages`: searchable message history view. This is not the default first
  view. It should support filtering and row expansion so long OpenCode text
  does not dominate the page.
- `Evidence`: blockers, failures, runtime defect, self-defect routing, eval
  results, git/worktree refs, and stop reason.

Current Rust projection already exposes `activity.sessions`,
`activity.subagents`, `activity.todos`, `activity.timeline`,
`running_tool_count`, and `pending_tool_count`. Full raw message history is not
currently exposed through the dashboard API; `SYM-96` should either document
that limitation or add a UI-consumable API/BFF contract for bounded message
history.

### Quota

Purpose: make OpenCode quota readable without exposing billing amounts.

Data comes from Next BFF calling:

```bash
ocu --plain --localhost
```

Render quota buckets as usage bars:

```text
bucket | window | used percent | remaining percent | reset time | freshness
```

States:

- Normal: show 5h and weekly windows.
- Unavailable: command missing, non-zero exit, malformed JSON, or timeout.
  Show clear unavailable status and last attempted command source, without
  implying usage is zero.

Overview shows only compact 5h quota. Weekly belongs on this page.

### Defects

Purpose: keep runtime/self-defects visible without letting noisy history
dominate the start page.

Table columns:

```text
fingerprint | severity | kind | relation | source issue | managed issue |
occurrences | first seen | last seen | next action
```

Sort by blocking/high-severity next actions first, then last seen. Deduplicate
by fingerprint and managed issue. Rows link back to issue drilldown evidence.

## Data Source Map

| Surface | Rust API | Next BFF | `ocu` |
| --- | --- | --- | --- |
| Overview status strips | `/api/dashboard` | normalize and omit billing fields | 5h quota only |
| Overview running table | `/api/dashboard` running issues | normalize | no |
| Overview blockers | `/api/dashboard`, project/issue drilldown as needed | normalize | no |
| Projects table | `/api/dashboard` | normalize | no |
| Project drilldown | `/api/projects/{project_id}` | proxy/normalize | no |
| Issue inspector | `/api/projects/{project_id}/issues/{issue_id}` | proxy/normalize, optional bounded message view | no |
| Quota page | no | route handler shells configured `OCU_COMMAND` | `ocu --plain --localhost` |
| Defects page | aggregate/project/issue defect projections | dedupe and normalize | no |

## Baseline Evidence

Current live Rust dashboard baseline was captured through an SSH tunnel to the
host-local API on `agent-server`.

- Desktop: `artifacts/screenshots/sym-95-baseline/current-rust-dashboard-desktop-1440x1000.png`
- Mobile: `artifacts/screenshots/sym-95-baseline/current-rust-dashboard-mobile-390x844.png`

Playwright smoke notes:

- Desktop viewport: `1440x1000`.
- Mobile viewport: `390x844`.
- Page title: `Symphony Operations`.
- Console warnings/errors: only `favicon.ico` 404 was observed.
- Baseline root text did not expose billing amounts.

## Acceptance Screenshots For Implementation

Future UI tasks must capture these screenshots under
`artifacts/screenshots/`:

- Overview empty desktop and mobile.
- Overview running desktop and mobile.
- Overview blocked desktop and mobile.
- Overview failed/runtime-defect desktop and mobile.
- Quota normal desktop and mobile.
- Quota unavailable desktop and mobile.
- Projects table desktop and mobile.
- Project drilldown desktop and mobile.
- Issue inspector desktop and mobile, including `Todos`, `Timeline`, and
  `Agents` tabs.

## SYM-96 Foundation Notes

`SYM-96` should create the minimal Next shell and BFF contracts only. Do not
build the full Overview, Projects, Quota, Defects, or inspector surfaces yet.
The shell should prove that:

- `apps/dashboard` can run independently from the Rust runtime.
- BFF routes can fetch/normalize Rust JSON.
- BFF quota route can parse `ocu --plain --localhost`.
- Dashboard-targeted responses and UI omit billing fields.
- Rust HTML remains in place until the final cutover task.
