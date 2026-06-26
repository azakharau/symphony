"use client";

import { currentRunnerSession } from "@/src/current-runner-session";
import { Badge, Panel, processStateLabel, providerModeLabel, runtimeFailureText } from "@/src/components";
import { LiveDuration } from "@/src/live-duration";
import type { DashboardTokenMetrics, EvalRun, IssueDetail, RunnerSession, SessionActivity, TimelineEvent, TodoActivity } from "@/src/types";

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
  const processTone = session?.process_alive === true ? "good" : session?.process_alive === false ? "bad" : "warn";

  return (
    <div className="flex flex-col gap-5">
      <section className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm">
        <div className="flex min-w-0 flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
          <div className="min-w-0">
            <p className="break-words text-xs font-semibold uppercase tracking-wide text-slate-500">{issue.identifier}</p>
            <h2 className="mt-1 break-words text-xl font-semibold sm:text-2xl">{issue.title}</h2>
            <div className="mt-3 flex flex-wrap gap-2">
              <Badge tone={inspectorTone(issue.lifecycle_stage)}>{humanizeLabel(issue.display_status)}</Badge>
              <Badge tone={processTone}>{session ? processStateLabel(session.process_id, session.process_alive) : "process unavailable"}</Badge>
              <Badge tone={inspectorTone(session?.current_stage ?? issue.lifecycle_stage)}>stage {humanizeLabel(session?.current_stage ?? issue.lifecycle_stage)}</Badge>
            </div>
            <p className="mt-3 max-w-3xl break-words text-sm text-slate-600">{executionSummary(issue, session, timeline)}</p>
          </div>
          <div className="grid gap-2 sm:flex sm:flex-wrap sm:justify-start lg:justify-end">
            <a className="rounded-lg bg-blue-600 px-4 py-2 text-center text-sm font-semibold text-white shadow-sm hover:bg-blue-700" href={linearUrl} target="_blank" rel="noreferrer">
              Open in Linear
            </a>
            {runnerUrl ? (
              <a className="rounded-lg border border-slate-300 bg-white px-4 py-2 text-center text-sm font-semibold text-slate-800 shadow-sm hover:border-blue-300 hover:text-blue-700" href={runnerUrl} target="_blank" rel="noreferrer">
                Open in runner
              </a>
            ) : null}
          </div>
        </div>
      </section>

      <Panel title="Current runner status" action={session?.runner_session_id ?? "No runner session reported by API"}>
        <RunnerEvidence issue={issue} session={session} toolEvents={toolEvents} />
      </Panel>

      <Panel title="Lifecycle timeline" action="stage history and recent activity">
        <LifecycleTimeline issue={issue} session={session} events={timeline} />
      </Panel>

      <section className="grid gap-5 lg:grid-cols-3">
        <Panel title="OMP workers" action={activity ? `${(activity.sessions.length + activity.subagents.length).toString()} sessions` : sourceUnavailable(session, "activity source unavailable")}>
          <AgentsTab issue={issue} />
        </Panel>
        <Panel title="Todo activity" action={activity ? `${activity.todos.length.toString()} reported` : sourceUnavailable(session, "todo source unavailable")}>
          <TodosTab issue={issue} />
        </Panel>
        <Panel title="Tool activity" action={activity ? `${toolEvents.length.toString()} recent events` : sourceUnavailable(session, "tool source unavailable")}>
          <ToolsByAgent issue={issue} events={toolEvents} />
        </Panel>
      </section>

      <section className="grid gap-5 lg:grid-cols-2">
        <Panel title="Eval state" action={issue.eval_results.length ? `${issue.eval_results.length.toString()} runs` : sourceUnavailable(session, "no eval runs reported")}>
          <EvalsSection evals={issue.eval_results} session={session} />
        </Panel>
        <Panel title="Git and worktree" action={issue.git_ref ? "refs reported" : sourceUnavailable(session, "git refs unavailable")}>
          <GitSection issue={issue} session={session} />
        </Panel>
      </section>

      {hasOperationalBlocker(issue, session) ? (
        <Panel title="Blocker and failure state" action="shown because issue has blocking evidence">
          <OperationalBlocker issue={issue} session={session} />
        </Panel>
      ) : null}

      <Panel title="Debug details" action="raw data collapsed">
        <DebugDetails issue={issue} />
      </Panel>
    </div>
  );
}

function RunnerEvidence({ issue, session, toolEvents }: { issue: IssueDetail; session?: RunnerSession; toolEvents: TimelineEvent[] }) {
  const tokens = tokenBreakdown(session?.token_count ?? 0, session?.cached_token_count, session?.token_metrics ?? issue.token_metrics);
  return (
    <dl className="grid gap-2 text-sm sm:grid-cols-2 xl:grid-cols-4">
      <KeyValue label="runner identity" value={session?.active_agent ?? session?.agent ?? "unavailable"} detail={session?.active_model ?? session?.model ?? "model unavailable"} />
      <KeyValue label="provider" value={providerModeLabel(session?.provider_mode)} detail={session?.provider_id ? `provider ${session.provider_id}` : "provider id unavailable"} />
      <KeyValue label="session id" value={session?.runner_session_id ?? "unavailable"} detail={session ? processStateLabel(session.process_id, session.process_alive) : "process not checked"} mono />
      <KeyValue label="stage" value={humanizeLabel(session?.current_stage ?? issue.lifecycle_stage)} detail={`lifecycle ${humanizeLabel(session?.lifecycle_stage ?? issue.lifecycle_stage)}`} />
      <KeyValue label="duration" value={<LiveDuration startedAtMs={session?.started_at_ms} fallbackMs={session?.duration_ms} />} detail={sessionLastUpdated(session)} />
      <KeyValue label="tokens" value={`${formatCompactNumber(tokens.accounted)} total`} detail={tokens.detail} />
      <KeyValue label="cache metrics" value={tokens.cachedAvailable ? `${formatCompactNumber(tokens.cached)} cached` : "unavailable"} detail={tokens.availabilityDetail} />
      <KeyValue label="tool activity" value={`${session?.activity?.running_tool_count ?? 0} running / ${session?.activity?.pending_tool_count ?? 0} pending`} detail={session?.activity ? `${toolEvents.length} recent tool events` : sourceUnavailable(session, "bounded tool activity unavailable")} />
      <KeyValue label="todo activity" value={session?.todo_count ?? 0} detail={session?.activity ? "todo detail available" : sourceUnavailable(session, "todo detail unavailable")} />
      <KeyValue label="ACP telemetry" value={`${session?.acp_frame_count ?? 0} frames`} detail={sessionEvidenceSummary(session?.session_evidence_refs)} />
      {session?.runtime_failure_kind ? <KeyValue label="runtime failure" value={runtimeFailureText(session.runtime_failure_kind)} detail={session.silence_observed ? "session is quiet or stale" : undefined} /> : null}
      {session?.silence_observed && !session.runtime_failure_kind ? <KeyValue label="runtime silence" value="session is quiet or stale" /> : null}
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
            <span className="break-words font-semibold text-slate-950">stage {humanizeLabel(stage)}</span>
            {stage === session?.current_stage ? <span className="text-xs text-blue-700">current</span> : null}
          </div>
          <p className="mt-1 text-xs text-slate-500">lifecycle step {index + 1} of {stages.length}</p>
        </li>
      ))}
      {events.length ? events.map((event) => <TimelineItem key={`${event.session_id}-${event.part_id}`} event={event} />) : (
        <li className="relative pl-5 text-sm text-slate-600">
          <span className="absolute -left-[0.35rem] top-1 h-2.5 w-2.5 rounded-full bg-slate-300" aria-hidden />
          {activityEmptyReason(session, "timeline")}
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
  if (!todos.length) return <Limited message={activityEmptyReason(session, "todo")} />;
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
  if (!events.length) return <Limited message={activityEmptyReason(currentRunnerSession(issue), "tool")} />;
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
      <div className="flex min-w-0 flex-wrap items-baseline gap-2">
        <span className="min-w-0 break-words font-semibold text-slate-950">{title}</span>
        {event.status ? <span className="text-xs text-slate-500">{humanizeLabel(event.status)}</span> : null}
      </div>
      {summary ? <p className="mt-1 break-words text-slate-600">{summary}</p> : null}
      <p className="mt-1 break-words text-xs text-slate-500">{event.tool ?? event.kind} · {formatEpochMs(event.time_updated_ms)}</p>
    </li>
  );
}

function AgentsTab({ issue }: { issue: IssueDetail }) {
  return <AgentsTree issue={issue} />;
}

export function AgentsTree({ issue }: { issue: IssueDetail }) {
  const activity = currentRunnerSession(issue)?.activity;
  const agents = (activity?.sessions ?? []).concat(activity?.subagents ?? []);
  if (!agents.length) return <Limited message={activityEmptyReason(currentRunnerSession(issue), "worker")} />;
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
  const cached = Math.max(0, node.agent.tokens_cache_read + node.agent.tokens_cache_write);
  const nonCached = Math.max(
    0,
    node.agent.tokens_input + node.agent.tokens_output + node.agent.tokens_reasoning,
  );
  return (
    <li>
      <div className={`grid grid-cols-[1.25rem_1fr] gap-3 rounded-lg px-2 py-2 text-sm ${active ? "bg-blue-50/70" : ""}`}>
        <span className="mt-1 flex h-4 w-4 items-center justify-center" aria-label={active ? "active agent" : "idle agent"}>
          {active ? <span className="h-4 w-4 animate-spin rounded-full border-2 border-blue-200 border-t-blue-700" /> : <span className="h-2.5 w-2.5 rounded-full bg-slate-300" />}
        </span>
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="min-w-0 break-words font-semibold text-slate-950">{node.agent.title}</span>
            <span className="rounded-full bg-slate-100 px-2 py-0.5 text-xs font-medium text-slate-500">{node.agent.is_subagent ? "subagent" : "root"}</span>
          </div>
          <p className="mt-1 break-words text-xs text-slate-500">{node.agent.agent ?? "agent unknown"} · {node.agent.model ?? "model unknown"} · {formatCompactNumber(nonCached)} tokens · {formatCompactNumber(cached)} cached</p>
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

function executionSummary(issue: IssueDetail, session: RunnerSession | undefined, events: TimelineEvent[]): string {
  if (issue.blocker?.message) return `Blocked: ${issue.blocker.message}`;
  if (issue.runtime_defect?.next_action) return `Runtime defect: ${issue.runtime_defect.next_action}`;
  if (issue.failure?.message) return `Failed: ${issue.failure.message}`;
  if (!session) return `No runner session is reported; lifecycle is ${humanizeLabel(issue.lifecycle_stage)}.`;

  const activity = session.activity;
  const activeAgent = session.active_agent ?? session.agent ?? "runner";
  const provider = providerModeLabel(session.provider_mode);
  const stage = humanizeLabel(session.current_stage);
  const tools = activity
    ? `${activity.running_tool_count} running / ${activity.pending_tool_count} pending tools`
    : sourceUnavailable(session, "tool activity unavailable");
  const recent = latestMeaningfulActivity(events);
  return `${activeAgent} is executing ${stage} through ${provider}; ${tools}. ${recent}`;
}

function latestMeaningfulActivity(events: TimelineEvent[]): string {
  const latestTimelineEvent = sortTimelineEvents(events).at(-1);
  if (!latestTimelineEvent) return "not yet observed: OMP timeline activity has not been reported.";
  return `Latest activity: ${latestTimelineEvent.summary || latestTimelineEvent.title || humanizeLabel(latestTimelineEvent.kind)}.`;
}

function sourceUnavailable(session: RunnerSession | undefined, fallback: string): string {
  if (!session) return "No runner session reported by API";
  return session.activity_error ?? fallback;
}

function activityEmptyReason(session: RunnerSession | undefined, source: "timeline" | "todo" | "tool" | "worker"): string {
  if (!session?.activity) return sourceUnavailable(session, `${source} activity source unavailable`);
  switch (source) {
    case "timeline":
      return "not yet observed: OMP timeline activity has not been reported.";
    case "todo":
      return "not yet observed: OMP todo activity has not been reported.";
    case "tool":
      return "not yet observed: OMP tool activity has not been reported.";
    case "worker":
      return "not yet observed: OMP worker activity has not been reported.";
  }
}

function hasOperationalBlocker(issue: IssueDetail, session?: RunnerSession): boolean {
  return Boolean(issue.blocker || issue.failure || issue.runtime_defect || issue.self_defect_routing || issue.stop_reason || session?.runtime_failure_kind || session?.silence_observed);
}

function OperationalBlocker({ issue, session }: { issue: IssueDetail; session?: RunnerSession }) {
  return (
    <div className="grid gap-3 text-sm sm:grid-cols-2">
      {issue.blocker ? <Evidence label="blocker" value={issue.blocker.message} /> : null}
      {issue.failure ? <Evidence label="failure" value={failureSummary(issue.failure)} /> : null}
      {issue.runtime_defect ? <Evidence label="runtime defect" value={runtimeDefectSummary(issue.runtime_defect)} /> : null}
      {issue.self_defect_routing ? <Evidence label="self-defect routing" value={selfDefectSummary(issue.self_defect_routing)} /> : null}
      {session?.runtime_failure_kind ? <Evidence label="runtime failure" value={runtimeFailureText(session.runtime_failure_kind)} /> : null}
      {session?.silence_observed ? <Evidence label="runtime silence" value="session is quiet or stale" /> : null}
      {issue.stop_reason ? <Evidence label="stop reason" value={issue.stop_reason} /> : null}
    </div>
  );
}

function failureSummary(failure: NonNullable<IssueDetail["failure"]>): string {
  const count = failure.occurrence_count > 1 ? ` · ${failure.occurrence_count} occurrences` : "";
  return `${failure.message} · ${humanizeLabel(failure.kind)}${count}`;
}

function runtimeDefectSummary(defect: NonNullable<IssueDetail["runtime_defect"]>): string {
  return `${defect.next_action} · ${humanizeLabel(defect.classification)}`;
}

function selfDefectSummary(route: NonNullable<IssueDetail["self_defect_routing"]>): string {
  const action = route.next_action ?? "inspect routed defect";
  const kind = humanizeLabel(route.kind ?? route.defect_kind ?? route.relation ?? route.relation_mode);
  const count = route.occurrence_count && route.occurrence_count > 1 ? ` · ${route.occurrence_count} occurrences` : "";
  return `${action} · ${kind}${count}`;
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

type InspectorTokenBreakdown = {
  accounted: number;
  reported: number;
  nonCached: number;
  cached: number;
  cachedAvailable: boolean;
  detail: string;
  availabilityDetail: string;
};

function tokenBreakdown(total: number, cached?: number | null, metrics?: DashboardTokenMetrics | null): InspectorTokenBreakdown {
  if (metrics) {
    const status = metrics.metrics_status || "unknown";
    const freshness = metrics.metrics_freshness && metrics.metrics_freshness !== "fresh" && metrics.metrics_freshness !== status ? ` · ${metrics.metrics_freshness}` : "";
    const reason = metrics.metrics_reason ? ` · ${metrics.metrics_reason}` : "";
    const cachedTokens = Math.max(0, metrics.cached_token_count);
    const cacheRead = Math.max(0, metrics.cache_read_token_count);
    const cacheWrite = Math.max(0, metrics.cache_write_token_count);
    const nonCached = Math.max(0, metrics.non_cached_token_count);
    const splitProven = status.toLowerCase() !== "unavailable" && (status.toLowerCase() !== "degraded" || cachedTokens > 0 || cacheRead > 0 || cacheWrite > 0);
    const cacheEvidence = cacheRead || cacheWrite ? ` (read ${formatCompactNumber(cacheRead)} · write ${formatCompactNumber(cacheWrite)})` : "";
    const metricState = `metrics ${status}${freshness}${reason}`;
    return {
      accounted: Math.max(0, metrics.accounted_total_token_count),
      reported: Math.max(0, metrics.reported_total_token_count),
      nonCached,
      cached: cachedTokens,
      cachedAvailable: splitProven,
      detail: splitProven
        ? `${formatCompactNumber(metrics.reported_total_token_count)} reported · ${formatCompactNumber(nonCached)} non-cache · ${formatCompactNumber(cachedTokens)} cached${cacheEvidence} · ${metricState}`
        : `${formatCompactNumber(metrics.reported_total_token_count)} reported · unavailable split · ${metricState}`,
      availabilityDetail: `${metrics.metrics_source || "metrics source unavailable"} · ${metricState}`,
    };
  }

  const safeCached = cached === undefined || cached === null ? null : Math.max(0, cached);
  const splitProven = safeCached !== null && safeCached > 0;
  return {
    accounted: Math.max(0, total),
    reported: Math.max(0, total),
    nonCached: splitProven ? Math.max(0, total - safeCached) : Math.max(0, total),
    cached: safeCached ?? 0,
    cachedAvailable: splitProven,
    detail: splitProven ? `${formatCompactNumber(safeCached)} cached · metrics legacy` : "unavailable split · metrics unavailable",
    availabilityDetail: splitProven ? "legacy session counter" : "no split metrics collected",
  };
}

function sortTimelineEvents(events: TimelineEvent[]): TimelineEvent[] {
  return [...events].sort((left, right) => (left.time_created_ms || left.time_updated_ms) - (right.time_created_ms || right.time_updated_ms));
}


function sessionLastUpdated(session?: RunnerSession): string {
  const updated = session?.activity?.last_updated_ms ?? runnerEventUpdatedMs(session?.last_event);
  if (!updated) return "last update unavailable";
  return `updated ${formatEpochMs(updated)}`;
}

function runnerEventUpdatedMs(lastEvent?: string | null): number | null {
  const raw = lastEvent?.startsWith("omp_jsonl_updated:")
    ? lastEvent.slice("omp_jsonl_updated:".length)
    : lastEvent?.startsWith("runner_archive_updated:")
      ? lastEvent.slice("runner_archive_updated:".length)
      : null;
  if (!raw) return null;
  const updated = Number(raw);
  return Number.isFinite(updated) && updated > 0 ? updated : null;
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

function DebugDetails({ issue }: { issue: IssueDetail }) {
  const session = currentRunnerSession(issue);
  const sessionEvidenceRefs = session?.session_evidence_refs ?? [];
  return (
    <div className="grid gap-3 text-sm">
      <div className="rounded-xl border border-slate-200 bg-slate-50 p-3">
        <span className="font-semibold">debug evidence summary</span>
        <p className="mt-1 break-words text-slate-600">
          {sessionEvidenceRefs.length ? `${sessionEvidenceRefs.length} session evidence refs: ${sessionEvidenceRefs.join("; ")}` : "No session evidence refs reported."}
        </p>
      </div>
      <details className="rounded-xl border border-slate-200 bg-slate-50 p-3">
        <summary className="cursor-pointer font-semibold text-slate-950">Raw issue JSON</summary>
        <pre className="mt-3 max-h-[26rem] max-w-full overflow-auto whitespace-pre-wrap break-words rounded-lg bg-slate-950 p-3 text-xs text-slate-50">{JSON.stringify(issue, null, 2)}</pre>
      </details>
    </div>
  );
}

function Evidence({ label, value }: { label: string; value: string }) {
  return <div className="min-w-0 rounded-xl border border-slate-200 bg-slate-50 p-3"><span className="font-semibold">{label}</span><p className="mt-1 break-words text-slate-600">{value}</p></div>;
}

function KeyValue({ label, value, detail, mono = false }: { label: string; value: React.ReactNode; detail?: string; mono?: boolean }) {
  return <div className="min-w-0 rounded-xl border border-slate-200 bg-slate-50 p-3"><dt className="text-xs uppercase tracking-wide text-slate-500">{label}</dt><dd className={`mt-1 overflow-hidden break-words ${mono ? "font-mono text-xs" : "font-medium"}`}>{value}</dd>{detail ? <dd className="mt-1 break-words text-xs text-slate-500">{detail}</dd> : null}</div>;
}


function Limited({ message }: { message: string }) {
  return <div className="rounded-xl border border-dashed border-slate-300 bg-slate-50 p-4 text-sm text-slate-600">{message}</div>;
}
