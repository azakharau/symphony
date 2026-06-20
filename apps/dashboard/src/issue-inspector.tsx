"use client";

import { useState } from "react";

import { Badge, Panel } from "@/src/components";
import type { IssueDetail, SessionActivity, TimelineEvent, TodoActivity } from "@/src/types";

const tabs = ["Todos", "Timeline", "Agents", "Tools", "Evidence"] as const;
const DEFAULT_LINEAR_WORKSPACE_SLUG = "alexey-zakharov";
const DEFAULT_OPENCODE_WEB_BASE = "https://opencode.vestalink.net";
type Tab = (typeof tabs)[number];

export function IssueInspector({ issue }: { issue: IssueDetail }) {
  const [activeTab, setActiveTab] = useState<Tab>("Todos");
  const session = issue.opencode_sessions.at(-1);
  const activity = session?.activity;
  const timeline = activity?.timeline ?? [];
  const toolEvents = timeline.filter((event) => event.kind === "tool" || event.tool);

  const summary = {
    todos: activity?.todos.length ?? session?.todo_count ?? 0,
    agents: (activity?.sessions.length ?? 0) + (activity?.subagents.length ?? 0),
    tools: toolEvents.length,
    events: timeline.length,
  };
  const sessionTokens = tokenBreakdown(session?.token_count ?? 0, session?.cached_token_count);
  const linearUrl = linearIssueUrl(issue.identifier);
  const opencodeDirectory = session ? opencodeSessionDirectory(session) : undefined;
  const opencodeUrl = session && opencodeDirectory ? opencodeSessionUrl(session.opencode_session_id, opencodeDirectory) : undefined;
  const sessionDuration = formatDuration(session?.duration_ms);

  return (
    <div className="flex flex-col gap-5">
      <section className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm">
        <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
          <div>
            <p className="text-xs font-semibold uppercase tracking-wide text-slate-500">{issue.identifier}</p>
            <h2 className="mt-1 text-2xl font-semibold">{issue.title}</h2>
            <div className="mt-3 flex flex-wrap gap-2">
              <Badge tone={issue.lifecycle_stage === "failed" ? "bad" : issue.lifecycle_stage === "blocked" ? "warn" : "good"}>{issue.display_status}</Badge>
            </div>
          </div>
          <div className="flex flex-col gap-3 lg:min-w-[520px]">
            <div className="flex flex-wrap justify-start gap-2 lg:justify-end">
              <a className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-semibold text-white shadow-sm hover:bg-blue-700" href={linearUrl} target="_blank" rel="noreferrer">
                Open in Linear
              </a>
              {opencodeUrl ? (
                <a className="rounded-lg border border-slate-300 bg-white px-4 py-2 text-sm font-semibold text-slate-800 shadow-sm hover:border-blue-300 hover:text-blue-700" href={opencodeUrl} target="_blank" rel="noreferrer">
                  Open in OpenCode
                </a>
              ) : null}
            </div>
            <dl className="grid gap-2 text-sm sm:grid-cols-2">
              <KeyValue label="active agent" value={session?.active_agent ?? session?.agent ?? "unavailable"} />
              <KeyValue label="model" value={session?.active_model ?? session?.model ?? "unavailable"} />
              <KeyValue label="tokens" value={formatCompactNumber(sessionTokens.net)} detail={`${formatCompactNumber(sessionTokens.cached)} cached`} />
              <KeyValue label="duration" value={sessionDuration} />
              <KeyValue label="worktree" value={session?.worktree_path ?? issue.git_ref?.worktree_path ?? "unavailable"} mono />
              <KeyValue label="git" value={issue.git_ref ? `${issue.git_ref.branch} ${issue.git_ref.head_sha ?? ""}` : "unavailable"} mono />
            </dl>
          </div>
        </div>
      </section>

      <section className="grid gap-3 sm:grid-cols-4">
        <Summary value={summary.todos} label="todos" />
        <Summary value={summary.agents} label="agents" />
        <Summary value={summary.tools} label="tool events" />
        <Summary value={summary.events} label="timeline events" />
      </section>

      <Panel title="OpenCode session inspector" action={session?.opencode_session_id ?? "no session"}>
        <div className="mb-4 flex gap-2 overflow-x-auto">
          {tabs.map((tab) => (
            <button
              key={tab}
              className={`rounded-full border px-3 py-2 text-sm font-medium ${activeTab === tab ? "border-blue-600 bg-blue-50 text-blue-700" : "border-slate-200 bg-white text-slate-600"}`}
              type="button"
              onClick={() => setActiveTab(tab)}
            >
              {tab}
            </button>
          ))}
        </div>
        {activeTab === "Todos" ? <TodosTab issue={issue} /> : null}
        {activeTab === "Timeline" ? <TimelineTab events={timeline} /> : null}
        {activeTab === "Agents" ? <AgentsTab issue={issue} /> : null}
        {activeTab === "Tools" ? <ToolsByAgent issue={issue} events={toolEvents} /> : null}
        {activeTab === "Evidence" ? <EvidenceTab issue={issue} /> : null}
      </Panel>
    </div>
  );
}

function TodosTab({ issue }: { issue: IssueDetail }) {
  const session = issue.opencode_sessions.at(-1);
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

function TimelineTab({ events, empty = "No timeline activity is available." }: { events: TimelineEvent[]; empty?: string }) {
  if (!events.length) return <Limited message={empty} />;
  return <ActivityTimeline events={events} />;
}

export function ActivityTimeline({ events }: { events: TimelineEvent[] }) {
  return (
    <ol className="ml-2 border-l border-slate-200">
      {events.map((event) => (
        <TimelineItem key={`${event.session_id}-${event.part_id}`} event={event} />
      ))}
    </ol>
  );
}

export function ToolsByAgent({ issue, events }: { issue: IssueDetail; events: TimelineEvent[] }) {
  if (!events.length) return <Limited message="No running, pending, or recent tool events were reported." />;
  const activity = issue.opencode_sessions.at(-1)?.activity;
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
  const activity = issue.opencode_sessions.at(-1)?.activity;
  const agents = (activity?.sessions ?? []).concat(activity?.subagents ?? []);
  if (!agents.length) return <Limited message="Agent tree is unavailable for this session." />;
  const activeSessionIds = new Set(
    (activity?.timeline ?? [])
      .filter((event) => isActiveStatus(event.status))
      .map((event) => event.session_id),
  );
  const session = issue.opencode_sessions.at(-1);
  if (!activeSessionIds.size && session?.process_alive) activeSessionIds.add(session.opencode_session_id);
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

function opencodeSessionUrl(sessionId: string, directory: string): string {
  const base = process.env.NEXT_PUBLIC_OPENCODE_WEB_BASE || DEFAULT_OPENCODE_WEB_BASE;
  return `${base.replace(/\/+$/, "")}/${base64UrlEncodeUtf8(directory)}/session/${encodeURIComponent(sessionId)}`;
}

function opencodeSessionDirectory(session: NonNullable<IssueDetail["opencode_sessions"][number]>): string | undefined {
  const activity = session.activity;
  const rootSession = activity?.sessions.find((item) => item.session_id === activity.root_session_id);
  const currentSession = activity?.sessions.find((item) => item.session_id === session.opencode_session_id);
  return currentSession?.directory || rootSession?.directory || session.worktree_path || undefined;
}

function base64UrlEncodeUtf8(value: string): string {
  const binary = String.fromCodePoint(...new TextEncoder().encode(value));
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function tokenBreakdown(total: number, cached?: number | null): { net: number; cached: number } {
  const safeCached = Math.max(0, cached ?? 0);
  return {
    net: Math.max(0, total - safeCached),
    cached: safeCached,
  };
}

function formatDuration(value?: number | null): string {
  if (value == null || value < 0) return "—";
  const totalSeconds = Math.floor(value / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
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
  return (
    <div className="grid gap-3 text-sm">
      <Evidence label="blocker" value={issue.blocker ? `${issue.blocker.kind}: ${issue.blocker.message}` : "none"} />
      <Evidence label="failure" value={issue.failure ? `${issue.failure.kind}: ${issue.failure.message}` : "none"} />
      <Evidence label="runtime defect" value={issue.runtime_defect ? `${issue.runtime_defect.classification}: ${issue.runtime_defect.next_action}` : "none"} />
      <Evidence label="self-defect routing" value={issue.self_defect_routing ? `${issue.self_defect_routing.fingerprint ?? "fingerprint unavailable"}: ${issue.self_defect_routing.next_action ?? "inspect"}` : "none"} />
      <Evidence label="evals" value={issue.eval_results.length ? issue.eval_results.map((item) => `${item.suite} ${item.status}`).join("; ") : "none"} />
      <Evidence label="stop reason" value={issue.stop_reason ?? "none"} />
    </div>
  );
}

function Evidence({ label, value }: { label: string; value: string }) {
  return <div className="rounded-xl border border-slate-200 bg-slate-50 p-3"><span className="font-semibold">{label}</span><p className="mt-1 text-slate-600">{value}</p></div>;
}

function KeyValue({ label, value, detail, mono = false }: { label: string; value: string; detail?: string; mono?: boolean }) {
  return <div className="rounded-xl border border-slate-200 bg-slate-50 p-3"><dt className="text-xs uppercase tracking-wide text-slate-500">{label}</dt><dd className={`mt-1 break-all ${mono ? "font-mono text-xs" : "font-medium"}`}>{value}</dd>{detail ? <dd className="mt-1 text-xs text-slate-500">{detail}</dd> : null}</div>;
}

function Summary({ value, label }: { value: number; label: string }) {
  return <div className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm"><div className="text-2xl font-semibold">{value}</div><div className="text-sm text-slate-500">{label}</div></div>;
}

function Limited({ message }: { message: string }) {
  return <div className="rounded-xl border border-dashed border-slate-300 bg-slate-50 p-4 text-sm text-slate-600">{message}</div>;
}
