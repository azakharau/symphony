"use client";

import { useState } from "react";

import { Badge, Panel } from "@/src/components";
import type { IssueDetail, TodoActivity } from "@/src/types";

const tabs = ["Todos", "Timeline", "Agents", "Tools", "Messages", "Evidence"] as const;
type Tab = (typeof tabs)[number];

export function IssueInspector({ issue }: { issue: IssueDetail }) {
  const [activeTab, setActiveTab] = useState<Tab>("Todos");
  const session = issue.opencode_sessions.at(-1);
  const activity = session?.activity;
  const timeline = activity?.timeline ?? [];
  const toolEvents = timeline.filter((event) => event.kind === "tool" || event.tool);
  const messages = timeline.filter((event) => event.kind === "message");
  const tokenTotal = session?.token_count ?? 0;

  const summary = {
    todos: activity?.todos.length ?? session?.todo_count ?? 0,
    agents: (activity?.sessions.length ?? 0) + (activity?.subagents.length ?? 0),
    tools: toolEvents.length,
    messages: messages.length || session?.message_count || 0,
  };

  return (
    <div className="flex flex-col gap-5">
      <section className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm">
        <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
          <div>
            <p className="text-xs font-semibold uppercase tracking-wide text-slate-500">{issue.identifier}</p>
            <h2 className="mt-1 text-2xl font-semibold">{issue.title}</h2>
            <div className="mt-3 flex flex-wrap gap-2">
              <Badge tone={issue.lifecycle_stage === "failed" ? "bad" : issue.lifecycle_stage === "blocked" ? "warn" : "good"}>{issue.display_status}</Badge>
              <Badge>{issue.lifecycle_stage}</Badge>
              <Badge>{session?.current_stage ?? "no session"}</Badge>
            </div>
          </div>
          <dl className="grid gap-2 text-sm sm:grid-cols-2 lg:min-w-[520px]">
            <KeyValue label="active agent" value={session?.active_agent ?? session?.agent ?? "unavailable"} />
            <KeyValue label="model" value={session?.active_model ?? session?.model ?? "unavailable"} />
            <KeyValue label="process" value={session ? processState(session.process_alive) : "unavailable"} />
            <KeyValue label="tokens" value={tokenTotal.toLocaleString("en-US")} />
            <KeyValue label="worktree" value={session?.worktree_path ?? issue.git_ref?.worktree_path ?? "unavailable"} mono />
            <KeyValue label="git" value={issue.git_ref ? `${issue.git_ref.branch} ${issue.git_ref.head_sha ?? ""}` : "unavailable"} mono />
            <KeyValue label="last event" value={session?.last_event ?? issue.last_runner_event ?? "unavailable"} />
          </dl>
        </div>
      </section>

      <section className="grid gap-3 sm:grid-cols-4">
        <Summary value={summary.todos} label="todos" />
        <Summary value={summary.agents} label="agents" />
        <Summary value={summary.tools} label="tool events" />
        <Summary value={summary.messages} label="messages" />
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
        {activeTab === "Tools" ? <TimelineTab events={toolEvents} empty="No running, pending, or recent tool events were reported." /> : null}
        {activeTab === "Messages" ? <TimelineTab events={messages} empty="Full raw messages are unavailable through the dashboard API; showing bounded timeline summaries only." /> : null}
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

function TimelineTab({ events, empty = "No timeline activity is available." }: { events: NonNullable<IssueDetail["opencode_sessions"][number]["activity"]>["timeline"]; empty?: string }) {
  if (!events.length) return <Limited message={empty} />;
  return (
    <div className="grid gap-2">
      {events.map((event) => (
        <details key={`${event.session_id}-${event.part_id}`} className="rounded-xl border border-slate-200 bg-white p-3 text-sm">
          <summary className="cursor-pointer font-semibold">{event.title ?? event.kind} <span className="font-normal text-slate-500">{event.status ?? "status unknown"}</span></summary>
          <p className="mt-2 text-slate-700">{event.summary}</p>
          <p className="mt-2 text-xs text-slate-500">{event.session_id} · {event.tool ?? event.kind} · {event.time_updated_ms}</p>
        </details>
      ))}
    </div>
  );
}

function AgentsTab({ issue }: { issue: IssueDetail }) {
  const activity = issue.opencode_sessions.at(-1)?.activity;
  const agents = (activity?.sessions ?? []).concat(activity?.subagents ?? []);
  if (!agents.length) return <Limited message="Agent tree is unavailable for this session." />;
  return (
    <div className="grid gap-2">
      {agents.map((agent) => (
        <div key={agent.session_id} className="rounded-xl border border-slate-200 bg-slate-50 p-3 text-sm">
          <div className="flex flex-wrap items-center gap-2"><Badge>{agent.is_subagent ? "subagent" : "root"}</Badge><span className="font-semibold">{agent.title}</span></div>
          <p className="mt-1 text-slate-600">{agent.agent ?? "agent unknown"} · {agent.model ?? "model unknown"}</p>
          <p className="mt-1 text-xs text-slate-500">parent {agent.parent_session_id ?? "none"} · tokens {agent.tokens_input + agent.tokens_output + agent.tokens_reasoning}</p>
        </div>
      ))}
    </div>
  );
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

function KeyValue({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return <div className="rounded-xl border border-slate-200 bg-slate-50 p-3"><dt className="text-xs uppercase tracking-wide text-slate-500">{label}</dt><dd className={`mt-1 break-all ${mono ? "font-mono text-xs" : "font-medium"}`}>{value}</dd></div>;
}

function Summary({ value, label }: { value: number; label: string }) {
  return <div className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm"><div className="text-2xl font-semibold">{value}</div><div className="text-sm text-slate-500">{label}</div></div>;
}

function Limited({ message }: { message: string }) {
  return <div className="rounded-xl border border-dashed border-slate-300 bg-slate-50 p-4 text-sm text-slate-600">{message}</div>;
}

function processState(alive?: boolean | null): string {
  if (alive === true) return "alive";
  if (alive === false) return "stopped";
  return "unknown";
}
