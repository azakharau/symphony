"use client";

import { currentRunnerSession } from "@/src/current-runner-session";
import { Badge, Panel, processStateLabel, providerModeLabel, runtimeFailureText } from "@/src/components";
import { LiveDuration } from "@/src/live-duration";
import type { EvalRun, IssueDetail, RunnerSession, SessionActivity, TimelineEvent, TodoActivity } from "@/src/types";

type BadgeTone = "good" | "warn" | "bad" | "idle";
const DEFAULT_LINEAR_WORKSPACE_SLUG = "alexey-zakharov";
const DEFAULT_RUNNER_WEB_BASE = "https://runner.vestalink.net";

export function IssueInspector({ issue }: { issue: IssueDetail }) {
  const session = currentRunnerSession(issue);
  const activity = session?.activity;
  const timeline = sortTimelineEvents(activity?.timeline ?? []);
  const toolEvents = timeline.filter((event) => event.kind === "tool" || event.tool);
  const linearUrl = linearIssueUrl(issue.identifier);
  const runnerDirectory = session ? runnerSessionDirectory(session) : undefined;
  const runnerUrl = session && runnerDirectory ? runnerSessionUrl(session.runner_session_id, runnerDirectory) : undefined;
  const lastEvent = latestEventText(issue, session, timeline);
  const processTone = session?.process_alive === true ? "good" : session?.process_alive === false ? "bad" : "warn";
  const summary = {
    todos: activity?.todos.length ?? session?.todo_count ?? 0,
    agents: (activity?.sessions.length ?? 0) + (activity?.subagents.length ?? 0),
    tools: toolEvents.length,
    events: timeline.length,
  };

  return (
    <div className="flex flex-col gap-5">
      <section className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm">
        <div className="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
          <div className="min-w-0">
            <p className="text-xs font-semibold uppercase tracking-wide text-slate-500">{issue.identifier}</p>
            <h2 className="mt-1 text-2xl font-semibold">{issue.title}</h2>
            <div className="mt-3 flex flex-wrap gap-2">
              <Badge tone={inspectorTone(issue.lifecycle_stage)}>{issue.display_status}</Badge>
              <Badge tone={processTone}>{session ? processStateLabel(session.process_id, session.process_alive) : "process unavailable"}</Badge>
              <Badge tone={inspectorTone(session?.current_stage ?? issue.lifecycle_stage)}>stage {humanizeLabel(session?.current_stage ?? issue.lifecycle_stage)}</Badge>
            </div>
            <p className="mt-3 max-w-3xl text-sm text-slate-600">
              Last event: <span className="font-medium text-slate-900">{lastEvent}</span>
            </p>
          </div>
          <div className="flex flex-wrap justify-start gap-2 lg:justify-end">
            <a className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-semibold text-white shadow-sm hover:bg-blue-700" href={linearUrl} target="_blank" rel="noreferrer">
              Open in Linear
            </a>
            {runnerUrl ? (
              <a className="rounded-lg border border-slate-300 bg-white px-4 py-2 text-sm font-semibold text-slate-800 shadow-sm hover:border-blue-300 hover:text-blue-700" href={runnerUrl} target="_blank" rel="noreferrer">
                Open in runner
              </a>
            ) : null}
          </div>
        </div>
      </section>

      <section className="grid gap-3 sm:grid-cols-4">
        <Summary value={summary.todos} label="todos" />
        <Summary value={summary.agents} label="agents" />
        <Summary value={summary.tools} label="tool events" />
        <Summary value={summary.events} label="timeline events" />
      </section>

      <Panel title="runner session inspector" action={session?.runner_session_id ?? "no session"}>
        <RunnerEvidence issue={issue} session={session} toolEvents={toolEvents} />
      </Panel>

      <Panel title="Timeline" action="lifecycle and recent activity">
        <LifecycleTimeline issue={issue} session={session} events={timeline} />
      </Panel>

      <section className="grid gap-5 lg:grid-cols-2">
        <Panel title="Todos" action={summary.todos ? `${summary.todos} reported` : "empty"}>
          <TodosTab issue={issue} />
        </Panel>
        <Panel title="Agents" action={summary.agents ? `${summary.agents} sessions` : "empty"}>
          <AgentsTab issue={issue} />
        </Panel>
        <Panel title="Tools" action={toolEvents.length ? `${toolEvents.length} events` : "empty"}>
          <ToolsByAgent issue={issue} events={toolEvents} />
        </Panel>
        <Panel title="Evals" action={issue.eval_results.length ? `${issue.eval_results.length} runs` : "empty"}>
          <EvalsSection evals={issue.eval_results} session={session} />
        </Panel>
        <Panel title="Git" action={issue.git_ref ? "refs reported" : "empty"}>
          <GitSection issue={issue} session={session} />
        </Panel>
        <Panel title="Evidence" action="raw details available">
          <EvidenceTab issue={issue} />
        </Panel>
      </section>
    </div>
  );
}

function RunnerEvidence({ issue, session, toolEvents }: { issue: IssueDetail; session?: RunnerSession; toolEvents: TimelineEvent[] }) {
  const tokens = tokenBreakdown(session?.token_count ?? 0, session?.cached_token_count);
  return (
    <dl className="grid gap-2 text-sm sm:grid-cols-2 lg:grid-cols-4">
      <KeyValue label="active agent" value={session?.active_agent ?? session?.agent ?? "unavailable"} detail={session?.active_model ?? session?.model ?? "model unavailable"} />
      <KeyValue label="provider/session" value={providerModeLabel(session?.provider_mode)} detail={session?.provider_id ? `provider ${session.provider_id}` : "provider id unavailable"} />
      <KeyValue label="session id" value={session?.runner_session_id ?? "unavailable"} detail={session ? processStateLabel(session.process_id, session.process_alive) : "process not checked"} mono />
      <KeyValue label="stage" value={humanizeLabel(session?.current_stage ?? issue.lifecycle_stage)} detail={`lifecycle ${humanizeLabel(session?.lifecycle_stage ?? issue.lifecycle_stage)}`} />
      <KeyValue label="tokens" value={formatCompactNumber(tokens.net)} detail={cachedTokenDetail(tokens.cached, session?.token_metrics?.metrics_status)} />
      <KeyValue label="cached-token availability" value={session?.cached_token_count === undefined || session.cached_token_count === null ? "unavailable" : formatCompactNumber(session.cached_token_count)} detail={session?.token_metrics?.metrics_source ?? "legacy session counter"} />
      <KeyValue label="tools" value={`${session?.activity?.running_tool_count ?? 0} running / ${session?.activity?.pending_tool_count ?? 0} pending`} detail={`${toolEvents.length} recent tool events`} />
      <KeyValue label="todos" value={session?.todo_count ?? 0} detail={session?.activity ? "todo detail available" : "aggregate count only"} />
      <KeyValue label="duration" value={<LiveDuration startedAtMs={session?.started_at_ms} fallbackMs={session?.duration_ms} />} detail={sessionLastUpdated(session)} />
      <KeyValue label="failure taxonomy" value={runtimeFailureText(session?.runtime_failure_kind)} detail={session?.silence_observed ? "session is quiet or stale" : "no silence marker"} />
      <KeyValue label="ACP telemetry" value={`${session?.acp_frame_count ?? 0} frames`} detail={sessionEvidenceSummary(session?.session_evidence_refs)} />
      <KeyValue label="last event" value={latestEventText(issue, session, session?.activity?.timeline ?? [])} />
    </dl>
  );
}

function LifecycleTimeline({ issue, session, events }: { issue: IssueDetail; session?: RunnerSession; events: TimelineEvent[] }) {
  const stages = session?.stage_history.length ? session.stage_history : [session?.current_stage ?? issue.lifecycle_stage];
  return (
    <ol className="ml-2 border-l border-slate-200">
      {stages.map((stage, index) => (
        <li key={`${stage}-${index}`} className="relative pb-3 pl-5 text-sm">
          <span className="absolute -left-[0.45rem] top-1 h-3.5 w-3.5 rounded-full bg-blue-600" aria-label={stage === session?.current_stage ? "current lifecycle stage" : "lifecycle stage"} />
          <div className="flex flex-wrap items-baseline gap-2">
            <span className="font-semibold text-slate-950">stage {humanizeLabel(stage)}</span>
            {stage === session?.current_stage ? <span className="text-xs text-blue-700">current</span> : null}
          </div>
          <p className="mt-1 text-xs text-slate-500">lifecycle step {index + 1} of {stages.length}</p>
        </li>
      ))}
      {events.length ? events.map((event) => <TimelineItem key={`${event.session_id}-${event.part_id}`} event={event} />) : (
        <li className="relative pl-5 text-sm text-slate-600">
          <span className="absolute -left-[0.35rem] top-1 h-2.5 w-2.5 rounded-full bg-slate-300" aria-hidden />
          No timeline activity is available; lifecycle stage history is shown above.
        </li>
      )}
    </ol>
  );
}

function EvalsSection({ evals, session }: { evals: EvalRun[]; session?: RunnerSession }) {
  if (!evals.length && !session?.eval_stage) return <Limited message="No eval runs were reported for this issue." />;
  return (
    <div className="grid gap-2 text-sm">
      {session?.eval_stage ? <Evidence label="active eval stage" value={session.eval_stage} /> : null}
      {evals.map((evalRun) => (
        <Evidence key={evalRun.run_id} label={evalRun.suite} value={`${evalRun.status}${evalRun.details_json ? ` · ${evalRun.details_json}` : ""}`} />
      ))}
    </div>
  );
}

function GitSection({ issue, session }: { issue: IssueDetail; session?: RunnerSession }) {
  const ref = issue.git_ref;
  if (!ref && !session?.worktree_path) return <Limited message="No git or worktree references were reported for this issue." />;
  return (
    <dl className="grid gap-2 text-sm">
      <KeyValue label="branch" value={ref?.branch ?? "unavailable"} mono />
      <KeyValue label="worktree" value={session?.worktree_path ?? ref?.worktree_path ?? "unavailable"} mono />
      <KeyValue label="commit" value={ref?.head_sha ?? "no commit reported"} detail={ref?.head_sha ? "head SHA reported by API" : "commit status unavailable"} mono />
      <KeyValue label="pull request" value={ref?.pr_url ? <a className="text-blue-700 hover:underline" href={ref.pr_url} target="_blank" rel="noreferrer">{ref.pr_url}</a> : "not reported"} />
      <KeyValue label="push/merge" value="not reported" detail="API exposes branch, worktree, commit, and PR only." />
    </dl>
  );
}

function TodosTab({ issue }: { issue: IssueDetail }) {
  const session = currentRunnerSession(issue);
  const todos = session?.activity?.todos ?? [];
  if (!todos.length) return <Limited message={session?.activity_error ?? "Todo details are unavailable; only aggregate todo counts are exposed."} />;
  return (
    <ol className="grid gap-1.5">
      {todos.map((todo) => (
        <TodoPlanItem key={`${todo.session_id}-${todo.position}`} todo={todo} />
      ))}
    </ol>
  );
}

function TodoPlanItem({ todo }: { todo: TodoActivity }) {
  const state = todoState(todo.status);
  return (
    <li className={`grid grid-cols-[1.5rem_1fr] gap-3 rounded-lg px-2 py-2 text-sm ${state.rowClass}`}>
      <span className="mt-0.5 flex h-5 w-5 items-center justify-center" aria-label={state.label}>
        {state.kind === "completed" ? <span className="flex h-5 w-5 items-center justify-center rounded-full bg-emerald-100 text-xs font-bold text-emerald-700">✓</span> : null}
        {state.kind === "active" ? <span className="h-4 w-4 animate-spin rounded-full border-2 border-blue-200 border-t-blue-700" /> : null}
        {state.kind === "pending" ? <span className="h-3 w-3 rounded-full border border-slate-300 bg-white" /> : null}
      </span>
      <div className="min-w-0">
        <span className={state.contentClass}>{todo.content}</span>
      </div>
    </li>
  );
}

function todoState(status: string): { kind: "completed" | "active" | "pending"; label: string; rowClass: string; contentClass: string } {
  const normalized = status.toLowerCase();
  if (["completed", "complete", "done"].includes(normalized)) {
    return {
      kind: "completed",
      label: "completed",
      rowClass: "text-slate-500",
      contentClass: "font-medium text-slate-500 line-through decoration-slate-400 decoration-2",
    };
  }
  if (["in_progress", "in-progress", "active", "running"].includes(normalized)) {
    return {
      kind: "active",
      label: "in progress",
      rowClass: "bg-blue-50/60",
      contentClass: "font-semibold text-slate-950",
    };
  }
  return {
    kind: "pending",
    label: "pending",
    rowClass: "text-slate-600",
    contentClass: "font-medium text-slate-700",
  };
}


export function ActivityTimeline({ events }: { events: TimelineEvent[] }) {
  const orderedEvents = sortTimelineEvents(events);
  return (
    <ol className="ml-2 border-l border-slate-200">
      {orderedEvents.map((event) => (
        <TimelineItem key={`${event.session_id}-${event.part_id}`} event={event} />
      ))}
    </ol>
  );
}

export function ToolsByAgent({ issue, events }: { issue: IssueDetail; events: TimelineEvent[] }) {
  if (!events.length) return <Limited message="No running, pending, or recent tool events were reported." />;
  const activity = currentRunnerSession(issue)?.activity;
  const agents = (activity?.sessions ?? []).concat(activity?.subagents ?? []);
  const agentsById = new Map(agents.map((agent) => [agent.session_id, agent]));
  const eventsBySession = new Map<string, TimelineEvent[]>();
  for (const event of events) {
    const group = eventsBySession.get(event.session_id) ?? [];
    group.push(event);
    eventsBySession.set(event.session_id, group);
  }
  const orderedSessionIds = [
    ...agents.map((agent) => agent.session_id).filter((sessionId) => eventsBySession.has(sessionId)),
    ...[...eventsBySession.keys()].filter((sessionId) => !agentsById.has(sessionId)),
  ];

  return (
    <div className="grid gap-4">
      {orderedSessionIds.map((sessionId) => {
        const agent = agentsById.get(sessionId);
        const agentEvents = eventsBySession.get(sessionId) ?? [];
        return (
          <section key={sessionId} className="grid gap-2">
            <div className="flex flex-wrap items-baseline gap-2">
              <h3 className="font-semibold text-slate-950">{agent?.title ?? sessionId}</h3>
              <span className="text-xs text-slate-500">{agent?.agent ?? "agent unknown"} · {agentEvents.length} tool events</span>
            </div>
            <ActivityTimeline events={agentEvents} />
          </section>
        );
      })}
    </div>
  );
}

function TimelineItem({ event }: { event: TimelineEvent }) {
  const active = isActiveStatus(event.status);
  const completed = isCompletedStatus(event.status);
  const title = event.title || event.tool || event.kind;
  const summary = event.summary && event.summary !== title ? event.summary : "";
  return (
    <li className="relative pb-3 pl-5 text-sm last:pb-0">
      <span className="absolute -left-[0.45rem] top-1 flex h-3.5 w-3.5 items-center justify-center rounded-full bg-white" aria-label={active ? "running event" : completed ? "completed event" : "timeline event"}>
        {active ? <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-blue-200 border-t-blue-700" /> : null}
        {!active && completed ? <span className="h-3.5 w-3.5 rounded-full bg-emerald-500" /> : null}
        {!active && !completed ? <span className="h-2.5 w-2.5 rounded-full bg-slate-300" /> : null}
      </span>
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="font-semibold text-slate-950">{title}</span>
        {event.status ? <span className="text-xs text-slate-500">{event.status}</span> : null}
      </div>
      {summary ? <p className="mt-1 text-slate-600">{summary}</p> : null}
      <p className="mt-1 text-xs text-slate-500">{event.tool ?? event.kind} · {formatEpochMs(event.time_updated_ms)}</p>
    </li>
  );
}

function AgentsTab({ issue }: { issue: IssueDetail }) {
  return <AgentsTree issue={issue} />;
}

export function AgentsTree({ issue }: { issue: IssueDetail }) {
  const activity = currentRunnerSession(issue)?.activity;
  const agents = (activity?.sessions ?? []).concat(activity?.subagents ?? []);
  if (!agents.length) return <Limited message="Agent tree is unavailable for this session." />;
  const activeSessionIds = new Set(
    (activity?.timeline ?? [])
      .filter((event) => isActiveStatus(event.status))
      .map((event) => event.session_id),
  );
  const session = currentRunnerSession(issue);
  if (!activeSessionIds.size && session?.process_alive) activeSessionIds.add(session.runner_session_id);
  const tree = buildAgentTree(agents);
  return (
    <ul className="grid gap-2">
      {tree.map((node) => (
        <AgentTreeNode key={node.agent.session_id} node={node} activeSessionIds={activeSessionIds} />
      ))}
    </ul>
  );
}

type AgentNode = {
  agent: SessionActivity;
  children: AgentNode[];
};

function AgentTreeNode({ node, activeSessionIds }: { node: AgentNode; activeSessionIds: Set<string> }) {
  const active = activeSessionIds.has(node.agent.session_id);
  const tokens = tokenBreakdown(
    node.agent.tokens_input + node.agent.tokens_output + node.agent.tokens_reasoning + node.agent.tokens_cache_read + node.agent.tokens_cache_write,
    node.agent.tokens_cache_read + node.agent.tokens_cache_write,
  );
  return (
    <li>
      <div className={`grid grid-cols-[1.25rem_1fr] gap-3 rounded-lg px-2 py-2 text-sm ${active ? "bg-blue-50/70" : ""}`}>
        <span className="mt-1 flex h-4 w-4 items-center justify-center" aria-label={active ? "active agent" : "idle agent"}>
          {active ? <span className="h-4 w-4 animate-spin rounded-full border-2 border-blue-200 border-t-blue-700" /> : <span className="h-2.5 w-2.5 rounded-full bg-slate-300" />}
        </span>
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-semibold text-slate-950">{node.agent.title}</span>
            <span className="rounded-full bg-slate-100 px-2 py-0.5 text-xs font-medium text-slate-500">{node.agent.is_subagent ? "subagent" : "root"}</span>
          </div>
          <p className="mt-1 text-xs text-slate-500">{node.agent.agent ?? "agent unknown"} · {node.agent.model ?? "model unknown"} · {formatCompactNumber(tokens.net)} tokens · {formatCompactNumber(tokens.cached)} cached</p>
        </div>
      </div>
      {node.children.length ? (
        <ul className="ml-4 mt-1 grid gap-1 border-l border-slate-200 pl-4">
          {node.children.map((child) => (
            <AgentTreeNode key={child.agent.session_id} node={child} activeSessionIds={activeSessionIds} />
          ))}
        </ul>
      ) : null}
    </li>
  );
}

function buildAgentTree(agents: SessionActivity[]): AgentNode[] {
  const nodes = new Map(agents.map((agent) => [agent.session_id, { agent, children: [] as AgentNode[] }]));
  const roots: AgentNode[] = [];
  for (const node of nodes.values()) {
    const parentId = node.agent.parent_session_id;
    const parent = parentId ? nodes.get(parentId) : undefined;
    if (parent) {
      parent.children.push(node);
    } else {
      roots.push(node);
    }
  }
  return roots;
}

function isActiveStatus(status?: string | null): boolean {
  return ["active", "running", "in_progress", "in-progress"].includes((status ?? "").toLowerCase());
}

function isCompletedStatus(status?: string | null): boolean {
  return ["done", "completed", "complete", "success", "succeeded"].includes((status ?? "").toLowerCase());
}

function formatCompactNumber(value: number): string {
  return new Intl.NumberFormat("en-US").format(value);
}

function linearIssueUrl(identifier: string): string {
  const workspace = process.env.NEXT_PUBLIC_LINEAR_WORKSPACE_SLUG || DEFAULT_LINEAR_WORKSPACE_SLUG;
  return `https://linear.app/${encodeURIComponent(workspace)}/issue/${encodeURIComponent(identifier)}`;
}

function runnerSessionUrl(sessionId: string, directory: string): string {
  const base = process.env.NEXT_PUBLIC_RUNNER_WEB_BASE || DEFAULT_RUNNER_WEB_BASE;
  return `${base.replace(/\/+$/, "")}/${base64UrlEncodeUtf8(directory)}/session/${encodeURIComponent(sessionId)}`;
}

function runnerSessionDirectory(session: NonNullable<IssueDetail["runner_sessions"][number]>): string | undefined {
  const activity = session.activity;
  const rootSession = activity?.sessions.find((item) => item.session_id === activity.root_session_id);
  const currentSession = activity?.sessions.find((item) => item.session_id === session.runner_session_id);
  return currentSession?.directory || rootSession?.directory || session.worktree_path || undefined;
}

function base64UrlEncodeUtf8(value: string): string {
  const binary = String.fromCodePoint(...new TextEncoder().encode(value));
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function sessionEvidenceSummary(refs?: string[] | null): string {
  if (!refs?.length) return "0 evidence refs";
  return `${refs.length} evidence refs: ${refs.slice(0, 2).join(", ")}`;
}

function tokenBreakdown(total: number, cached?: number | null): { net: number; cached: number } {
  const safeCached = Math.max(0, cached ?? 0);
  return {
    net: Math.max(0, total - safeCached),
    cached: safeCached,
  };
}

function sortTimelineEvents(events: TimelineEvent[]): TimelineEvent[] {
  return [...events].sort((left, right) => (left.time_created_ms || left.time_updated_ms) - (right.time_created_ms || right.time_updated_ms));
}

function latestEventText(issue: IssueDetail, session?: RunnerSession, events: TimelineEvent[] = []): string {
  const latestTimelineEvent = sortTimelineEvents(events).at(-1);
  return latestTimelineEvent?.summary || latestTimelineEvent?.title || session?.last_event || issue.last_runner_event || "no runner event reported";
}

function sessionLastUpdated(session?: RunnerSession): string {
  const updated = session?.activity?.last_updated_ms;
  if (!updated) return "last update unavailable";
  return `updated ${formatEpochMs(updated)}`;
}

function cachedTokenDetail(cached: number, status?: string | null): string {
  if (!status) return `${formatCompactNumber(cached)} cached`;
  return `${formatCompactNumber(cached)} cached · metrics ${status}`;
}

function humanizeLabel(value?: string | null): string {
  return value ? value.replaceAll("_", " ") : "unknown";
}

function inspectorTone(value?: string | null): BadgeTone {
  const normalized = (value ?? "").toLowerCase();
  if (["running", "review", "eval", "handoff", "completed", "done", "active"].some((token) => normalized.includes(token))) return "good";
  if (["blocked", "silent", "quota", "provider", "idle"].some((token) => normalized.includes(token))) return "warn";
  if (["failed", "failure", "defect", "exit", "canceled"].some((token) => normalized.includes(token))) return "bad";
  return "idle";
}

function formatEpochMs(value: number): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return new Intl.DateTimeFormat("en-US", {
    timeZone: "UTC",
    month: "short",
    day: "2-digit",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
    timeZoneName: "short",
  }).format(date);
}

function EvidenceTab({ issue }: { issue: IssueDetail }) {
  const sessionEvidenceRefs = currentRunnerSession(issue)?.session_evidence_refs ?? [];
  return (
    <div className="grid gap-3 text-sm">
      <Evidence label="blocker" value={issue.blocker ? `${issue.blocker.kind}: ${issue.blocker.message}` : "none"} />
      <Evidence label="failure" value={issue.failure ? `${issue.failure.kind}: ${issue.failure.message}` : "none"} />
      <Evidence label="runtime defect" value={issue.runtime_defect ? `${issue.runtime_defect.classification}: ${issue.runtime_defect.next_action}` : "none"} />
      <Evidence label="session evidence refs" value={sessionEvidenceRefs.length ? sessionEvidenceRefs.join("; ") : "none"} />
      <Evidence label="self-defect routing" value={issue.self_defect_routing ? `${issue.self_defect_routing.fingerprint ?? "fingerprint unavailable"}: ${issue.self_defect_routing.next_action ?? "inspect"}` : "none"} />
      <Evidence label="evals" value={issue.eval_results.length ? issue.eval_results.map((item) => `${item.suite} ${item.status}`).join("; ") : "none"} />
      <Evidence label="stop reason" value={issue.stop_reason ?? "none"} />
      <details className="rounded-xl border border-slate-200 bg-slate-50 p-3">
        <summary className="cursor-pointer font-semibold text-slate-950">Raw issue JSON</summary>
        <pre className="mt-3 max-h-[26rem] overflow-auto rounded-lg bg-slate-950 p-3 text-xs text-slate-50">{JSON.stringify(issue, null, 2)}</pre>
      </details>
    </div>
  );
}

function Evidence({ label, value }: { label: string; value: string }) {
  return <div className="rounded-xl border border-slate-200 bg-slate-50 p-3"><span className="font-semibold">{label}</span><p className="mt-1 text-slate-600">{value}</p></div>;
}

function KeyValue({ label, value, detail, mono = false }: { label: string; value: React.ReactNode; detail?: string; mono?: boolean }) {
  return <div className="rounded-xl border border-slate-200 bg-slate-50 p-3"><dt className="text-xs uppercase tracking-wide text-slate-500">{label}</dt><dd className={`mt-1 break-all ${mono ? "font-mono text-xs" : "font-medium"}`}>{value}</dd>{detail ? <dd className="mt-1 text-xs text-slate-500">{detail}</dd> : null}</div>;
}

function Summary({ value, label }: { value: number; label: string }) {
  return <div className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm"><div className="text-2xl font-semibold">{value}</div><div className="text-sm text-slate-500">{label}</div></div>;
}

function Limited({ message }: { message: string }) {
  return <div className="rounded-xl border border-dashed border-slate-300 bg-slate-50 p-4 text-sm text-slate-600">{message}</div>;
}
