import Link from "next/link";

import { LiveDuration } from "@/src/live-duration";
import type { AggregateDashboard, DashboardProjectCard, DashboardTokenMetrics, IssueDetail, ProjectDetail, RunningIssueSummary, SelfDefectRouteSummary } from "@/src/types";
import type { QuotaResult, QuotaWindow } from "@/src/quota";

export function DashboardFrame({ children }: { children: React.ReactNode }) {
  return (
    <main className="mx-auto flex min-h-screen w-full max-w-7xl flex-col gap-5 px-4 py-5 text-slate-950 sm:px-6 lg:px-8">
      <header className="flex flex-col gap-3 border-b border-slate-200 pb-4 lg:flex-row lg:items-end lg:justify-between">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.28em] text-slate-500">Symphony operations</p>
          <h1 className="mt-1 text-2xl font-semibold tracking-tight sm:text-3xl">Observability console</h1>
        </div>
        <nav aria-label="Dashboard sections" className="flex gap-2 overflow-x-auto text-sm font-medium">
          <NavLink href="/">Overview</NavLink>
          <NavLink href="/projects">Projects</NavLink>
          <NavLink href="/quota">Quota</NavLink>
          <NavLink href="/defects">Defects</NavLink>
        </nav>
      </header>
      {children}
    </main>
  );
}

export function NavLink({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <Link className="rounded-full border border-slate-200 bg-white px-3 py-2 text-slate-700 shadow-sm hover:border-slate-400" href={href}>
      {children}
    </Link>
  );
}

export function UnavailablePanel({ title, message }: { title: string; message: string }) {
  return (
    <section className="rounded-2xl border border-amber-200 bg-amber-50 p-5 text-sm text-amber-950">
      <h2 className="font-semibold">{title}</h2>
      <p className="mt-2">{message}</p>
    </section>
  );
}

export function OverviewSurface({ dashboard, quota }: { dashboard: AggregateDashboard; quota: QuotaResult }) {
  const running = dashboard.projects.flatMap((project) => project.running_issues ?? []);
  const blockers = dashboard.projects.filter((project) => project.active_count === 0 && (project.parked_count > 0 || isProblemStatus(project.runner_health)));
  const defectCount = dashboard.projects.reduce((total, project) => total + (project.self_defect_routes?.length ?? 0), 0);
  const runningTokens = tokenBreakdown(dashboard.totals.running_tokens, dashboard.totals.running_cached_tokens, dashboard.totals.token_metrics);

  return (
    <div className="flex flex-col gap-5">
      <section className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <MetricCard
          title="Sessions"
          value={`${dashboard.totals.running_issue_count}/${dashboard.totals.max_sessions}`}
          detail={`${dashboard.totals.available_sessions} slots available · ${tokenSummary(runningTokens)}`}
          tone={running.length ? "good" : dashboard.totals.available_sessions > 0 ? "idle" : "warn"}
        />
        <QuotaCompact quota={quota} />
        <MetricCard title="Blockers" value={String(blockers.length)} detail="blocked or unhealthy projects" tone={blockers.length ? "warn" : "good"} />
        <MetricCard title="Defects" value={String(defectCount)} detail="deduped runtime signals" tone={defectCount ? "bad" : "good"} />
      </section>

      <Panel title="Running now" action={<span>{running.length ? "live sessions" : "empty"}</span>}>
        {running.length ? <RunningTable issues={running} /> : <EmptyState message="No runner sessions are running. Project rows below still show idle reasons." />}
      </Panel>

      <Panel title="Blockers and idle reasons">
        {blockers.length ? <ProjectReasonTable projects={blockers} /> : <EmptyState message="No blockers reported. Idle projects are waiting for eligible work or capacity." />}
      </Panel>

      <Panel title="Project health">
        <ProjectTable projects={dashboard.projects} />
      </Panel>
    </div>
  );
}

export function ProjectsSurface({ dashboard }: { dashboard: AggregateDashboard }) {
  return (
    <Panel title="Projects" action={<span>{dashboard.projects.length} total</span>}>
      <div className="mb-3 flex flex-wrap gap-2 text-xs text-slate-600">
        <span className="rounded-full border bg-slate-50 px-2 py-1">health</span>
        <span className="rounded-full border bg-slate-50 px-2 py-1">enabled</span>
        <span className="rounded-full border bg-slate-50 px-2 py-1">blocked</span>
      </div>
      <ProjectTable projects={dashboard.projects} detailed />
    </Panel>
  );
}

export function ProjectSurface({ project }: { project: ProjectDetail }) {
  const allIssues = project.active_issues.concat(project.history_issues);
  const runningIssues = project.active_issues.filter(isLiveIssue);
  const queueIssues = project.active_issues.filter((issue) => !isLiveIssue(issue));
  const blockers = queueIssues.concat(project.history_issues).filter((issue) => issue.blocker || issue.lifecycle_stage === "blocked");
  const defects = allIssues.filter((issue) => issue.runtime_defect || issue.self_defect_routing || issue.failure);

  return (
    <div className="flex flex-col gap-5">
      <section className="grid gap-3 lg:grid-cols-4">
        <MetricCard title="Runtime" value={project.liveness.status} detail={project.liveness.primary_reason_detail || project.liveness.reason} tone={statusTone(project.liveness.status)} />
        <MetricCard title="Capacity" value={`${project.capacity.running_sessions}/${project.capacity.max_sessions}`} detail={`${project.capacity.available_sessions} slots available`} />
        <MetricCard title="Queue" value={project.selected_candidate?.identifier ?? "idle"} detail={project.selected_candidate?.reason ?? "no selected candidate"} />
        <MetricCard title="Cleanup" value={project.cleanup_status} detail={project.enabled ? "enabled" : "disabled"} />
      </section>

      <Panel title={`${project.name} current execution`}>
        {runningIssues.length ? <IssueTable issues={runningIssues} projectId={project.project_id} /> : <EmptyState message="No live execution is currently reported for this project." />}
      </Panel>
      <Panel title="Queue and blockers">
        {blockers.length || project.suppression_reasons.length ? <BlockerTable issues={blockers} suppressions={project.suppression_reasons} projectId={project.project_id} /> : <EmptyState message="No blockers or suppression reasons are currently reported." />}
      </Panel>
      <Panel title="Recent run history">
        {project.history_issues.length ? <IssueTable issues={project.history_issues} projectId={project.project_id} /> : <EmptyState message="No terminal runs recorded yet." />}
      </Panel>
      <Panel title="Related defects">
        {defects.length ? <DefectIssueList issues={defects} projectId={project.project_id} /> : <EmptyState message="No runtime or self-defect evidence for this project." />}
      </Panel>
    </div>
  );
}

export function QuotaSurface({ quota }: { quota: QuotaResult }) {
  if (quota.status === "unavailable") {
    return (
      <Panel title="Quota">
        <div className="rounded-xl border border-amber-200 bg-amber-50 p-4 text-sm text-amber-950">
          <p className="font-semibold">Quota unavailable</p>
          <p className="mt-2">Quota data is temporarily unavailable.</p>
          <p className="mt-1">Reason: {quota.reason}</p>
        </div>
      </Panel>
    );
  }

  const windows = quota.quota.buckets.flatMap((bucket) => bucket.windows.map((window) => ({ ...window, bucket: bucket.title })));

  return (
    <Panel title="Quota windows">
      {windows.length ? (
        <div className="grid gap-3">
          {windows.map((window) => <QuotaWindowBar key={`${window.bucket}-${window.label}`} window={window} bucket={window.bucket} />)}
        </div>
      ) : (
        <EmptyState message="Quota data is available, but no window buckets were reported." />
      )}
    </Panel>
  );
}

export function DefectsSurface({ defects }: { defects: SelfDefectRouteSummary[] }) {
  return (
    <Panel title="Deduped defects" action={<span>{defects.length} fingerprints</span>}>
      {defects.length ? (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[900px] text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-slate-500">
              <tr>
                <th className="px-3 py-2">fingerprint</th>
                <th className="px-3 py-2">severity</th>
                <th className="px-3 py-2">kind</th>
                <th className="px-3 py-2">relation</th>
                <th className="px-3 py-2">source issue</th>
                <th className="px-3 py-2">managed issue</th>
                <th className="px-3 py-2">occurrences</th>
                <th className="px-3 py-2">first / last</th>
                <th className="px-3 py-2">next action</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-slate-100">
              {defects.map((defect) => (
                <tr key={`${defect.fingerprint}-${defect.managed_issue_id ?? "unmanaged"}`}>
                  <td className="px-3 py-3 font-mono text-xs">{defect.fingerprint}</td>
                  <td className="px-3 py-3"><Badge tone={statusTone(defect.severity)}>{defect.severity ?? "unknown"}</Badge></td>
                  <td className="px-3 py-3">{defect.kind ?? defect.defect_kind ?? "unknown"}</td>
                  <td className="px-3 py-3">{defect.relation ?? defect.relation_mode ?? "unrelated"}</td>
                  <td className="px-3 py-3">{defect.source_issue_identifier ?? defect.source_issue_id ?? "—"}</td>
                  <td className="px-3 py-3">{defect.managed_issue_identifier ?? defect.managed_issue_id ?? "—"}</td>
                  <td className="px-3 py-3">{defect.occurrence_count ?? 1}</td>
                  <td className="px-3 py-3 text-xs text-slate-600">{shortTime(defect.first_seen_at)} / {shortTime(defect.last_seen_at)}</td>
                  <td className="px-3 py-3">{defect.next_action ?? "inspect evidence"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <EmptyState message="No Symphony self/runtime defects are currently reported." />
      )}
    </Panel>
  );
}

function RunningTable({ issues }: { issues: RunningIssueSummary[] }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[760px] text-left text-sm">
        <thead className="text-xs uppercase tracking-wide text-slate-500">
          <tr>
            <th className="px-3 py-2">project</th>
            <th className="px-3 py-2">issue</th>
            <th className="px-3 py-2">stage</th>
            <th className="px-3 py-2">provider/state</th>
            <th className="px-3 py-2">agent/model</th>
            <th className="px-3 py-2">tokens</th>
            <th className="px-3 py-2">duration</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {issues.map((issue) => (
            <tr key={`${issue.project_id}-${issue.issue_id}`}>
              <td className="px-3 py-3">{issue.project_name}</td>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${issue.project_id}/issues/${issue.issue_id}`}>{issue.identifier}</Link><div className="text-xs text-slate-500">{issue.title}</div></td>
              <td className="px-3 py-3"><Badge tone={statusTone(issue.stage)}>{issue.stage ?? issue.display_status}</Badge></td>
              <td className="px-3 py-3"><ProviderStateBlock providerMode={issue.provider_mode} providerId={issue.provider_id} sessionId={issue.session_id} processId={issue.process_id} processAlive={issue.process_alive} runtimeFailureKind={issue.runtime_failure_kind} acpFrameCount={issue.acp_frame_count} evidenceCount={issue.session_evidence_refs?.length} silenceObserved={issue.silence_observed} /></td>
              <td className="px-3 py-3">{issue.active_agent ?? issue.agent ?? "—"}<div className="text-xs text-slate-500">{issue.active_model ?? issue.model ?? "model unknown"}</div></td>
              <td className="px-3 py-3"><TokenCell total={issue.token_count} cached={issue.cached_token_count} metrics={issue.token_metrics} /></td>
              <td className="px-3 py-3"><LiveDuration startedAtMs={issue.started_at_ms} fallbackMs={issue.duration_ms} /></td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ProjectTable({ projects, detailed = false }: { projects: DashboardProjectCard[]; detailed?: boolean }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[780px] text-left text-sm">
        <thead className="text-xs uppercase tracking-wide text-slate-500">
          <tr>
            <th className="px-3 py-2">project</th>
            <th className="px-3 py-2">health</th>
            <th className="px-3 py-2">enabled</th>
            <th className="w-16 whitespace-nowrap px-2 py-2 text-center tabular-nums" title="running/slots">slots</th>
            <th className="px-3 py-2">active</th>
            <th className="px-3 py-2">blocked</th>
            {detailed ? <th className="px-3 py-2">terminal</th> : null}
            <th className="px-3 py-2">primary reason</th>
            <th className="px-3 py-2">last event</th>
            <th className="px-3 py-2">cleanup</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {projects.map((project) => (
            <tr key={project.project_id}>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${project.project_id}`}>{project.name}</Link></td>
              <td className="px-3 py-3"><Badge tone={statusTone(project.runner_health)}>{project.runner_health}</Badge></td>
              <td className="px-3 py-3">{project.enabled ? "yes" : "no"}</td>
              <td className="w-16 whitespace-nowrap px-2 py-3 text-center tabular-nums" aria-label="running sessions / max slots">{project.capacity.running_sessions}/{project.capacity.max_sessions}</td>
              <td className="px-3 py-3">{project.active_count}</td>
              <td className="px-3 py-3">{project.parked_count}</td>
              {detailed ? <td className="px-3 py-3">{project.terminal_count}</td> : null}
              <td className="px-3 py-3">{project.liveness.primary_reason_detail || project.liveness.reason}</td>
              <td className="px-3 py-3">{project.last_event}</td>
              <td className="px-3 py-3">{project.cleanup_status}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ProjectReasonTable({ projects }: { projects: DashboardProjectCard[] }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[680px] text-left text-sm">
        <thead className="text-xs uppercase tracking-wide text-slate-500">
          <tr>
            <th className="px-3 py-2">project</th>
            <th className="px-3 py-2">health</th>
            <th className="px-3 py-2">enabled</th>
            <th className="px-3 py-2">primary reason</th>
            <th className="px-3 py-2">last event</th>
            <th className="px-3 py-2">cleanup</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {projects.map((project) => (
            <tr key={project.project_id}>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${project.project_id}`}>{project.name}</Link></td>
              <td className="px-3 py-3"><Badge tone={statusTone(project.runner_health)}>{project.runner_health}</Badge></td>
              <td className="px-3 py-3">{project.enabled ? "yes" : "no"}</td>
              <td className="px-3 py-3">{project.liveness.primary_reason_detail || project.liveness.reason}</td>
              <td className="px-3 py-3">{project.last_event}</td>
              <td className="px-3 py-3">{project.cleanup_status}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function IssueTable({ issues, projectId }: { issues: IssueDetail[]; projectId: string }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[860px] text-left text-sm">
        <thead className="text-xs uppercase tracking-wide text-slate-500">
          <tr>
            <th className="px-3 py-2">issue</th>
            <th className="px-3 py-2">stage</th>
            <th className="px-3 py-2">active agent/model</th>
            <th className="px-3 py-2">provider/session</th>
            <th className="px-3 py-2">process</th>
            <th className="px-3 py-2">tokens</th>
            <th className="px-3 py-2">tools</th>
            <th className="px-3 py-2">todos</th>
            <th className="px-3 py-2">duration</th>
            <th className="px-3 py-2">worktree</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {issues.map((issue) => {
            const session = issue.runner_sessions.at(-1);
            return (
              <tr key={issue.issue_id}>
                <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${projectId}/issues/${issue.issue_id}`}>{issue.identifier}</Link><div className="text-xs text-slate-500">{issue.title}</div></td>
                <td className="px-3 py-3"><Badge tone={statusTone(issue.lifecycle_stage)}>{issue.display_status}</Badge></td>
                <td className="px-3 py-3">{session?.active_agent ?? session?.agent ?? "—"}<div className="text-xs text-slate-500">{session?.active_model ?? session?.model ?? "model unknown"}</div></td>
                <td className="px-3 py-3">{session ? <ProviderStateBlock providerMode={session.provider_mode} providerId={session.provider_id} sessionId={session.runner_session_id} processId={session.process_id} processAlive={session.process_alive} runtimeFailureKind={session.runtime_failure_kind} acpFrameCount={session.acp_frame_count} evidenceCount={session.session_evidence_refs.length} silenceObserved={session.silence_observed} /> : "—"}</td>
                <td className="px-3 py-3">{session ? processStateLabel(session.process_id, session.process_alive) : "—"}</td>
                <td className="px-3 py-3"><TokenCell total={session?.token_count ?? 0} cached={session?.cached_token_count} metrics={session?.token_metrics} /></td>
                <td className="px-3 py-3">{session?.activity?.running_tool_count ?? 0}/{session?.activity?.pending_tool_count ?? 0}</td>
                <td className="px-3 py-3">{session?.todo_count ?? 0}</td>
                <td className="px-3 py-3"><LiveDuration startedAtMs={session?.started_at_ms} fallbackMs={session?.duration_ms} /></td>
                <td className="max-w-[220px] truncate px-3 py-3 font-mono text-xs">{session?.worktree_path ?? issue.git_ref?.worktree_path ?? "—"}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function BlockerTable({ issues, suppressions, projectId }: { issues: IssueDetail[]; suppressions: ProjectDetail["suppression_reasons"]; projectId: string }) {
  return (
    <div className="grid gap-3 text-sm">
      {issues.map((issue) => (
        <Link key={issue.issue_id} className="rounded-xl border border-amber-200 bg-amber-50 p-3 text-amber-950" href={`/projects/${projectId}/issues/${issue.issue_id}`}>
          <span className="font-semibold">{issue.identifier}</span> {issue.blocker?.kind ?? issue.display_status}: {issue.blocker?.message ?? issue.last_runner_event ?? "blocked"}
        </Link>
      ))}
      {suppressions.map((suppression) => (
        <div key={`${suppression.issue_id}-${suppression.reason_kind}`} className="rounded-xl border border-slate-200 bg-slate-50 p-3">
          <span className="font-semibold">{suppression.identifier}</span> {suppression.reason_kind}: {suppression.reason}
        </div>
      ))}
    </div>
  );
}

function DefectIssueList({ issues, projectId }: { issues: IssueDetail[]; projectId: string }) {
  return (
    <div className="grid gap-2 text-sm">
      {issues.map((issue) => (
        <Link key={issue.issue_id} className="rounded-xl border border-red-200 bg-red-50 p-3 text-red-950" href={`/projects/${projectId}/issues/${issue.issue_id}`}>
          <span className="font-semibold">{issue.identifier}</span> {issue.runtime_defect?.classification ?? issue.self_defect_routing?.kind ?? issue.self_defect_routing?.defect_kind ?? issue.failure?.kind}: {issue.runtime_defect?.next_action ?? issue.self_defect_routing?.next_action ?? issue.failure?.message}
        </Link>
      ))}
    </div>
  );
}

function QuotaCompact({ quota }: { quota: QuotaResult }) {
  if (quota.status === "unavailable") {
    return <MetricCard title="5h quota" value="unavailable" detail="quota data temporarily unavailable" tone="warn" />;
  }
  const window = quota.quota.buckets.flatMap((bucket) => bucket.windows).find((entry) => entry.label.toLowerCase() === "5h");
  const remaining = quotaRemainingPercent(window);
  return (
    <article className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm">
      <div className="text-xs font-semibold uppercase tracking-wide text-slate-500">5h quota</div>
      <div className="mt-2 text-2xl font-semibold">{remaining}% remaining</div>
      <Progress value={remaining} />
      <p className="mt-2 text-xs text-slate-500">reset {shortTime(window?.resetAt)}</p>
    </article>
  );
}

function QuotaWindowBar({ window, bucket }: { window: QuotaWindow; bucket: string }) {
  const remaining = quotaRemainingPercent(window);
  return (
    <div className="rounded-xl border border-slate-200 bg-white p-4">
      <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between">
        <div><div className="font-semibold">{bucket}</div><div className="text-sm text-slate-500">{window.label} window</div></div>
        <div className="text-sm text-slate-600">reset {shortTime(window.resetAt)}</div>
      </div>
      <Progress value={remaining} />
      <div className="mt-2 text-sm text-slate-600">{remaining}% remaining · {window.usedPercent ?? 0}% used</div>
    </div>
  );
}

function MetricCard({ title, value, detail, tone = "idle" }: { title: string; value: string; detail: string; tone?: Tone }) {
  return (
    <article className="rounded-2xl border border-slate-200 bg-white p-4 shadow-sm">
      <div className="flex items-center justify-between gap-2"><p className="text-xs font-semibold uppercase tracking-wide text-slate-500">{title}</p><span className={dotClass(tone)} /></div>
      <p className="mt-2 truncate text-2xl font-semibold">{value}</p>
      <p className="mt-1 text-sm text-slate-600">{detail}</p>
    </article>
  );
}

function TokenCell({ total, cached, metrics }: { total: number; cached?: number | null; metrics?: DashboardTokenMetrics | null }) {
  const tokens = tokenBreakdown(total, cached, metrics);
  return (
    <div>
      <div>{formatNumber(tokens.accounted)} / {formatNumber(tokens.reported)} total</div>
      <div className="text-xs text-slate-500">{formatNumber(tokens.nonCached)} non-cache · {cacheSummary(tokens)}</div>
      <div className={`text-xs ${tokens.statusTone === "warn" || tokens.statusTone === "bad" ? "font-medium text-amber-700" : "text-slate-500"}`}>metrics {tokens.status}</div>
    </div>
  );
}

function ProviderStateBlock({
  providerMode,
  providerId,
  sessionId,
  processId,
  processAlive,
  runtimeFailureKind,
  acpFrameCount,
  evidenceCount,
  silenceObserved,
}: {
  providerMode?: string | null;
  providerId?: string | null;
  sessionId?: string | null;
  processId?: number | null;
  processAlive?: boolean | null;
  runtimeFailureKind?: string | null;
  acpFrameCount?: number | null;
  evidenceCount?: number | null;
  silenceObserved?: boolean | null;
}) {
  return (
    <div>
      <div className="font-medium">{providerModeLabel(providerMode)}</div>
      <div className="text-xs text-slate-500">{providerId ? `provider ${providerId}` : "provider id unavailable"}</div>
      <div className="text-xs text-slate-500">{sessionId ? `session ${sessionId}` : "session id unavailable"}</div>
      <div className="text-xs text-slate-500">{processStateLabel(processId, processAlive)}</div>
      {runtimeFailureKind ? <div className="text-xs font-medium text-amber-700">{runtimeFailureText(runtimeFailureKind)}</div> : null}
      <div className="text-xs text-slate-500">{acpFrameCount ?? 0} ACP frames · {evidenceCount ?? 0} evidence refs</div>
      {silenceObserved ? <div className="text-xs font-medium text-amber-700">session is quiet or stale</div> : null}
    </div>
  );
}

export function Panel({ title, action, children }: { title: string; action?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="rounded-2xl border border-slate-200 bg-white shadow-sm">
      <div className="flex items-center justify-between gap-3 border-b border-slate-100 px-4 py-3">
        <h2 className="text-sm font-semibold uppercase tracking-wide text-slate-700">{title}</h2>
        <div className="text-xs text-slate-500">{action}</div>
      </div>
      <div className="p-3 sm:p-4">{children}</div>
    </section>
  );
}

function EmptyState({ message }: { message: string }) {
  return <div className="rounded-xl border border-dashed border-slate-300 bg-slate-50 p-4 text-sm text-slate-600">{message}</div>;
}

function Progress({ value }: { value: number }) {
  const safe = Math.max(0, Math.min(100, value));
  return <div className="mt-3 h-2 rounded-full bg-slate-100"><div className="h-2 rounded-full bg-blue-600" style={{ width: `${safe}%` }} /></div>;
}

function quotaRemainingPercent(window?: QuotaWindow): number {
  return window?.remainingPercent ?? (window?.usedPercent === undefined ? 0 : 100 - window.usedPercent);
}

type Tone = "good" | "warn" | "bad" | "idle";

export function Badge({ children, tone = "idle" }: { children: React.ReactNode; tone?: Tone }) {
  return <span className={`inline-flex rounded-full px-2 py-1 text-xs font-semibold ${badgeClass(tone)}`}>{children}</span>;
}

function statusTone(status?: string | null): Tone {
  const value = (status ?? "").toLowerCase();
  if (["active", "running", "review", "eval", "completed", "normal", "low"].some((token) => value.includes(token))) return "good";
  if (["blocked", "quota", "provider", "medium", "idle"].some((token) => value.includes(token))) return "warn";
  if (["failed", "defect", "critical", "high", "exit"].some((token) => value.includes(token))) return "bad";
  return "idle";
}

function isProblemStatus(value: string): boolean {
  return statusTone(value) === "bad" || value.toLowerCase().includes("blocked");
}

function badgeClass(tone: Tone): string {
  switch (tone) {
    case "good": return "bg-emerald-100 text-emerald-800";
    case "warn": return "bg-amber-100 text-amber-800";
    case "bad": return "bg-red-100 text-red-800";
    default: return "bg-slate-100 text-slate-700";
  }
}

function dotClass(tone: Tone): string {
  switch (tone) {
    case "good": return "h-2.5 w-2.5 rounded-full bg-emerald-500";
    case "warn": return "h-2.5 w-2.5 rounded-full bg-amber-500";
    case "bad": return "h-2.5 w-2.5 rounded-full bg-red-500";
    default: return "h-2.5 w-2.5 rounded-full bg-slate-400";
  }
}

export function providerModeLabel(mode?: string | null): string {
  if (mode === "omp_acp") return "OMP ACP";
  if (mode === "acp") return "runner ACP";
  return "provider unavailable";
}

export function runtimeFailureText(kind?: string | null): string {
  switch (kind) {
    case "provider_auth_unavailable": return "provider auth unavailable";
    case "unsupported_omp_version": return "unsupported OMP version";
    case "malformed_acp_frame": return "malformed ACP response";
    case "missing_binary": return "runtime binary missing";
    default: return kind ? kind.replaceAll("_", " ") : "none";
  }
}

export function processStateLabel(processId?: number | null, alive?: boolean | null): string {
  const pid = processId ? `pid ${processId}` : "pid unavailable";
  if (alive === true) return `${pid} live`;
  if (alive === false) return `${pid} stale/stopped`;
  return `${pid} not checked`;
}

function isLiveIssue(issue: IssueDetail): boolean {
  const session = issue.runner_sessions.at(-1);
  return issue.lifecycle_stage === "running" || session?.process_alive === true;
}

function shortTime(value?: string | null): string {
  if (!value) return "unknown";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
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

function formatNumber(value: number): string {
  return new Intl.NumberFormat("en-US").format(value);
}

type TokenBreakdown = {
  accounted: number;
  reported: number;
  nonCached: number;
  cached: number;
  cacheRead: number;
  cacheWrite: number;
  status: string;
  statusTone: Tone;
  splitProven: boolean;
};

function tokenBreakdown(total: number, cached?: number | null, metrics?: DashboardTokenMetrics | null): TokenBreakdown {
  if (metrics) {
    const status = metrics.metrics_status || "unknown";
    const normalizedStatus = status.toLowerCase();
    const cachedTokens = Math.max(0, metrics.cached_token_count);
    const cacheRead = Math.max(0, metrics.cache_read_token_count);
    const cacheWrite = Math.max(0, metrics.cache_write_token_count);
    const splitProven = normalizedStatus !== "unavailable" && (normalizedStatus !== "degraded" || cachedTokens > 0 || cacheRead > 0 || cacheWrite > 0);
    return {
      accounted: Math.max(0, metrics.accounted_total_token_count),
      reported: Math.max(0, metrics.reported_total_token_count),
      nonCached: Math.max(0, metrics.non_cached_token_count),
      cached: cachedTokens,
      cacheRead,
      cacheWrite,
      status,
      statusTone: tokenStatusTone(status),
      splitProven,
    };
  }

  const safeCached = cached === undefined || cached === null ? null : Math.max(0, cached);
  const accounted = Math.max(0, total);
  const splitProven = safeCached !== null && safeCached > 0;
  return {
    accounted,
    reported: accounted,
    nonCached: splitProven ? Math.max(0, accounted - safeCached) : accounted,
    cached: safeCached ?? 0,
    cacheRead: 0,
    cacheWrite: 0,
    status: splitProven ? "legacy" : "unavailable",
    statusTone: splitProven ? "idle" : "warn",
    splitProven,
  };
}

function tokenSummary(tokens: TokenBreakdown): string {
  return `${formatNumber(tokens.accounted)} / ${formatNumber(tokens.reported)} tokens · ${formatNumber(tokens.nonCached)} non-cache · ${cacheSummary(tokens)} · metrics ${tokens.status}`;
}

function tokenStatusTone(status: string): Tone {
  switch (status.toLowerCase()) {
    case "available":
      return "good";
    case "degraded":
    case "unavailable":
    case "missing":
    case "unknown":
    case "partial":
    case "mixed":
      return "warn";
    default:
      return "idle";
  }
}

function cacheSummary(tokens: TokenBreakdown): string {
  if (!tokens.splitProven) {
    return `${tokens.status} split`;
  }
  const evidence = tokens.cacheRead || tokens.cacheWrite ? ` (read ${formatNumber(tokens.cacheRead)} · write ${formatNumber(tokens.cacheWrite)})` : "";
  return `${formatNumber(tokens.cached)} cached${evidence}`;
}
