import Link from "next/link";

import { currentRunnerSession } from "@/src/current-runner-session";
import { LiveDuration } from "@/src/live-duration";
import type { AggregateDashboard, DashboardProjectCard, DashboardTokenMetrics, IssueDetail, ProjectDetail, RunningIssueSummary, SelfDefectRouteSummary } from "@/src/types";
import type { QuotaBucket, QuotaResult, QuotaWindow } from "@/src/quota";

const RECENT_HISTORY_LIMIT = 5;

export function DashboardFrame({ children }: { children: React.ReactNode }) {
  return (
    <main className="mx-auto flex min-h-screen w-full max-w-7xl flex-col gap-4 px-3 py-4 text-slate-950 sm:gap-5 sm:px-6 sm:py-5 lg:px-8">
      <header className="flex min-w-0 flex-col gap-3 border-b border-slate-200 pb-4 lg:flex-row lg:items-end lg:justify-between">
        <div className="min-w-0">
          <p className="text-xs font-semibold uppercase tracking-[0.22em] text-slate-500 sm:tracking-[0.28em]">Symphony operations</p>
          <h1 className="mt-1 text-2xl font-semibold tracking-tight sm:text-3xl">Observability console</h1>
        </div>
        <nav aria-label="Dashboard sections" className="-mx-1 flex max-w-full gap-2 overflow-x-auto px-1 pb-1 text-sm font-medium">
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
    <Link className="shrink-0 rounded-full border border-slate-200 bg-white px-3 py-2 text-slate-700 shadow-sm hover:border-slate-400" href={href}>
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
  const attentionProjects = dashboard.projects.filter(hasOverviewAttention);
  const defectCount = dashboard.projects.reduce((total, project) => total + (project.self_defect_routes?.length ?? 0), 0);
  const runningTokens = tokenBreakdown(dashboard.totals.running_tokens, dashboard.totals.running_cached_tokens, dashboard.totals.token_metrics);

  return (
    <div className="flex flex-col gap-5">
      <Panel
        title="Running now"
        action={
          <div className="flex flex-wrap items-center justify-end gap-2">
            <span>{running.length ? `${running.length} live` : "empty"}</span>
            <QuotaCompact quota={quota} />
          </div>
        }
      >
        <div className="mb-3 flex flex-wrap gap-2 text-xs text-slate-600">
          <span className="rounded-full border bg-slate-50 px-2 py-1">sessions {dashboard.totals.running_issue_count}/{dashboard.totals.max_sessions}</span>
          <span className="rounded-full border bg-slate-50 px-2 py-1">{dashboard.totals.available_sessions} slots available</span>
          <span className="rounded-full border bg-slate-50 px-2 py-1">{tokenSummary(runningTokens)}</span>
        </div>
        {running.length ? <RunningTable issues={running} /> : <EmptyState message="No runner sessions are running. Project rows below still show idle reasons." />}
      </Panel>

      <Panel title="Project health and capacity" action={<span>{dashboard.projects.length} projects · {defectCount} defects</span>}>
        <ProjectHealthCapacityTable projects={dashboard.projects} />
      </Panel>

      <Panel title="Blockers and idle reasons">
        {attentionProjects.length ? <ProjectReasonTable projects={attentionProjects} /> : <EmptyState message="No blockers reported. Idle projects are waiting for eligible work or capacity." />}
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
      <Panel title={`${project.name} current execution`} action={<span>{runningIssues.length ? `${runningIssues.length} running` : "idle"}</span>}>
        {runningIssues.length ? <IssueTable issues={runningIssues} projectId={project.project_id} /> : <EmptyState message="No live execution is currently reported for this project. Queue state is shown below." />}
      </Panel>

      <Panel title="Queue and blockers" action={<span>{project.selected_candidate ? `next ${project.selected_candidate.identifier}` : "no selected candidate"}</span>}>
        {project.selected_candidate || blockers.length || project.suppression_reasons.length ? (
          <BlockerTable selectedCandidate={project.selected_candidate} issues={blockers} suppressions={project.suppression_reasons} projectId={project.project_id} />
        ) : (
          <EmptyState message="No blockers or suppression reasons are currently reported. The project is waiting for eligible work." />
        )}
      </Panel>

      <section className="grid gap-3 lg:grid-cols-4">
        <MetricCard title="Runtime" value={humanizeLabel(project.liveness.status)} detail={humanizeLabel(project.liveness.primary_reason_detail || project.liveness.reason)} tone={statusTone(project.liveness.status)} />
        <MetricCard title="Capacity" value={`${project.capacity.running_sessions}/${project.capacity.max_sessions}`} detail={`${project.capacity.available_sessions} slots available`} />
        <MetricCard title="Queue" value={project.selected_candidate?.identifier ?? "idle"} detail={humanizeLabel(project.selected_candidate?.reason ?? "no selected candidate")} />
        <MetricCard title="Cleanup" value={humanizeLabel(project.cleanup_status)} detail={project.enabled ? "enabled" : "disabled"} />
      </section>

      <Panel title="Recent run history" action={<span>{historySummary(project.history_issues.length)}</span>}>
        {project.history_issues.length ? <BoundedHistory issues={project.history_issues} projectId={project.project_id} /> : <EmptyState message="No terminal runs recorded yet." />}
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
      <Panel title="Quota" action={<QuotaHealthLine quota={quota} />}>
        <div className="rounded-xl border border-amber-200 bg-amber-50 p-4 text-sm text-amber-950">
          <p className="font-semibold">Quota unavailable</p>
          <p className="mt-2">Quota data is temporarily unavailable.</p>
          <p className="mt-1">Reason: {quota.reason}</p>
          <p className="mt-1">Command: <code>{quota.command}</code></p>
        </div>
      </Panel>
    );
  }

  return (
    <Panel title="Quota windows" action={<QuotaHealthLine quota={quota} />}>
      {quota.quota.buckets.length ? (
        <div className="grid gap-4">
          {quota.quota.buckets.map((bucket) => <QuotaBucketBlock key={bucket.title} bucket={bucket} />)}
        </div>
      ) : (
        <EmptyState message="Quota data is available, but no window buckets were reported." />
      )}
    </Panel>
  );
}

export function DefectsSurface({ defects }: { defects: SelfDefectRouteSummary[] }) {
  const groups = groupDefects(defects);
  return (
    <Panel title="Deduped defects" action={<span>{groups.length} fingerprints · {defects.length} routed records</span>}>
      {groups.length ? (
        <div className="overflow-x-auto sm:-mx-1 sm:px-1">
          <table className="defect-table responsive-table w-full min-w-[900px] text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-slate-500">
              <tr>
                <th className="px-3 py-2">fingerprint</th>
                <th className="px-3 py-2">severity</th>
                <th className="px-3 py-2">kind / relation</th>
                <th className="px-3 py-2">source issues</th>
                <th className="px-3 py-2">managed issues</th>
                <th className="px-3 py-2">occurrences</th>
                <th className="px-3 py-2">first / last</th>
                <th className="px-3 py-2">status / action</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-slate-100">
              {groups.map((group) => (
                <tr key={group.fingerprint}>
                  <td className="px-3 py-3 font-mono text-xs">{group.fingerprint}</td>
                  <td className="px-3 py-3"><Badge tone={statusTone(group.severity)}>{humanizeLabel(group.severity) ?? "unknown"}</Badge></td>
                  <td className="px-3 py-3">{group.kind}<div className="text-xs text-slate-500">{group.relation}</div></td>
                  <td className="px-3 py-3">{joinIssueSet(group.sourceIssues)}</td>
                  <td className="px-3 py-3">{joinIssueSet(group.managedIssues)}</td>
                  <td className="px-3 py-3">{group.occurrences}<div className="text-xs text-slate-500">{group.records} routes</div></td>
                  <td className="px-3 py-3 text-xs text-slate-600">{shortTime(group.firstSeenAt)} / {shortTime(group.lastSeenAt)}</td>
                  <td className="px-3 py-3"><Badge tone={statusTone(group.status)}>{humanizeLabel(group.status)}</Badge><div className="mt-1 text-xs text-slate-600">{group.nextAction}</div></td>
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
    <div className="overflow-x-auto sm:-mx-1 sm:px-1">
      <table className="running-table responsive-table w-full min-w-[760px] text-left text-sm">
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
              <td className="px-3 py-3"><Badge tone={statusTone(issue.stage)}>{humanizeLabel(issue.stage ?? issue.display_status)}</Badge></td>
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
    <div className="overflow-x-auto sm:-mx-1 sm:px-1">
      <table className="project-table responsive-table w-full min-w-[780px] text-left text-sm" data-detailed={detailed ? "true" : "false"}>
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
            <th className="px-3 py-2">cleanup</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {projects.map((project) => (
            <tr key={project.project_id}>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${project.project_id}`}>{project.name}</Link></td>
              <td className="px-3 py-3"><Badge tone={statusTone(project.runner_health)}>{humanizeLabel(project.runner_health)}</Badge></td>
              <td className="px-3 py-3">{project.enabled ? "yes" : "no"}</td>
              <td className="w-16 whitespace-nowrap px-2 py-3 text-center tabular-nums" aria-label="running sessions / max slots">{project.capacity.running_sessions}/{project.capacity.max_sessions}</td>
              <td className="px-3 py-3">{project.active_count}</td>
              <td className="px-3 py-3">{project.parked_count}</td>
              {detailed ? <td className="px-3 py-3">{project.terminal_count}</td> : null}
              <td className="px-3 py-3">{humanizeLabel(project.liveness.primary_reason_detail || project.liveness.reason)}</td>
              <td className="px-3 py-3">{humanizeLabel(project.cleanup_status)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ProjectHealthCapacityTable({ projects }: { projects: DashboardProjectCard[] }) {
  return (
    <div className="overflow-x-auto sm:-mx-1 sm:px-1">
      <table className="health-table responsive-table w-full min-w-[720px] text-left text-sm">
        <thead className="text-xs uppercase tracking-wide text-slate-500">
          <tr>
            <th className="px-3 py-2">project</th>
            <th className="px-3 py-2">health</th>
            <th className="px-3 py-2">enabled</th>
            <th className="w-16 whitespace-nowrap px-2 py-2 text-center tabular-nums">slots</th>
            <th className="px-3 py-2">active</th>
            <th className="px-3 py-2">blocked</th>
            <th className="px-3 py-2">primary reason</th>
            <th className="px-3 py-2">running tokens</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {projects.map((project) => (
            <tr key={project.project_id}>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${project.project_id}`}>{project.name}</Link></td>
              <td className="px-3 py-3"><Badge tone={statusTone(project.runner_health)}>{humanizeLabel(project.runner_health)}</Badge></td>
              <td className="px-3 py-3">{project.enabled ? "yes" : "no"}</td>
              <td className="w-16 whitespace-nowrap px-2 py-3 text-center tabular-nums" aria-label="running sessions / max slots">{project.capacity.running_sessions}/{project.capacity.max_sessions}</td>
              <td className="px-3 py-3">{project.active_count}</td>
              <td className="px-3 py-3">{project.parked_count}</td>
              <td className="px-3 py-3">{humanizeLabel(project.liveness.primary_reason_detail || project.liveness.reason)}</td>
              <td className="px-3 py-3"><TokenCell total={project.running_tokens} cached={project.running_cached_tokens} metrics={project.token_metrics} /></td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ProjectReasonTable({ projects }: { projects: DashboardProjectCard[] }) {
  return (
    <div className="overflow-x-auto sm:-mx-1 sm:px-1">
      <table className="reason-table responsive-table w-full min-w-[680px] text-left text-sm">
        <thead className="text-xs uppercase tracking-wide text-slate-500">
          <tr>
            <th className="px-3 py-2">project</th>
            <th className="px-3 py-2">health</th>
            <th className="px-3 py-2">enabled</th>
            <th className="px-3 py-2">primary reason</th>
            <th className="px-3 py-2">detail</th>
            <th className="px-3 py-2">cleanup</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {projects.map((project) => (
            <tr key={project.project_id}>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${project.project_id}`}>{project.name}</Link></td>
              <td className="px-3 py-3"><Badge tone={statusTone(project.runner_health)}>{humanizeLabel(project.runner_health)}</Badge></td>
              <td className="px-3 py-3">{project.enabled ? "yes" : "no"}</td>
              <td className="px-3 py-3">{humanizeLabel(project.liveness.primary_reason_detail || project.liveness.reason)}</td>
              <td className="px-3 py-3">{secondaryLivenessDetail(project)}</td>
              <td className="px-3 py-3">{humanizeLabel(project.cleanup_status)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function IssueTable({ issues, projectId }: { issues: IssueDetail[]; projectId: string }) {
  return (
    <div className="overflow-x-auto sm:-mx-1 sm:px-1">
      <table className="issue-table responsive-table w-full min-w-[860px] text-left text-sm">
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
            <th className="px-3 py-2">operational detail</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {issues.map((issue) => {
            const session = currentRunnerSession(issue);
            return (
              <tr key={issue.issue_id}>
                <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${projectId}/issues/${issue.issue_id}`}>{issue.identifier}</Link><div className="text-xs text-slate-500">{issue.title}</div></td>
                <td className="px-3 py-3"><Badge tone={statusTone(issue.lifecycle_stage)}>{humanizeLabel(issue.display_status)}</Badge></td>
                <td className="px-3 py-3">{session?.active_agent ?? session?.agent ?? "—"}<div className="text-xs text-slate-500">{session?.active_model ?? session?.model ?? "model unknown"}</div></td>
                <td className="px-3 py-3">{session ? <ProviderStateBlock providerMode={session.provider_mode} providerId={session.provider_id} sessionId={session.runner_session_id} processId={session.process_id} processAlive={session.process_alive} runtimeFailureKind={session.runtime_failure_kind} acpFrameCount={session.acp_frame_count} evidenceCount={session.session_evidence_refs.length} silenceObserved={session.silence_observed} /> : "—"}</td>
                <td className="px-3 py-3">{session ? processStateLabel(session.process_id, session.process_alive) : "—"}</td>
                <td className="px-3 py-3"><TokenCell total={session?.token_count ?? 0} cached={session?.cached_token_count} metrics={session?.token_metrics} /></td>
                <td className="px-3 py-3">{session?.activity?.running_tool_count ?? 0}/{session?.activity?.pending_tool_count ?? 0}</td>
                <td className="px-3 py-3">{session?.todo_count ?? 0}</td>
                <td className="px-3 py-3"><LiveDuration startedAtMs={session?.started_at_ms} fallbackMs={session?.duration_ms} /></td>
                <td className="px-3 py-3">{issueOperationalDetail(issue, session)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function BlockerTable({
  selectedCandidate,
  issues,
  suppressions,
  projectId,
}: {
  selectedCandidate: ProjectDetail["selected_candidate"];
  issues: IssueDetail[];
  suppressions: ProjectDetail["suppression_reasons"];
  projectId: string;
}) {
  const representedIssueIds = new Set([
    ...(selectedCandidate ? [selectedCandidate.issue_id] : []),
    ...issues.map((issue) => issue.issue_id),
  ]);
  const supplementalSuppressions = suppressions.filter((suppression) => !representedIssueIds.has(suppression.issue_id));

  return (
    <div className="overflow-x-auto sm:-mx-1 sm:px-1">
      <table className="blocker-table responsive-table w-full min-w-[760px] text-left text-sm">
        <thead className="text-xs uppercase tracking-wide text-slate-500">
          <tr>
            <th className="px-3 py-2">issue</th>
            <th className="px-3 py-2">state</th>
            <th className="px-3 py-2">reason</th>
            <th className="px-3 py-2">next action</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-slate-100">
          {selectedCandidate ? (
            <tr>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${projectId}/issues/${selectedCandidate.issue_id}`}>{selectedCandidate.identifier}</Link></td>
              <td className="px-3 py-3"><Badge tone={statusTone(selectedCandidate.lifecycle_stage)}>next eligible</Badge></td>
              <td className="px-3 py-3">{selectedCandidate.reason}</td>
              <td className="px-3 py-3">run when capacity is available</td>
            </tr>
          ) : null}
          {issues.map((issue) => (
            <tr key={issue.issue_id}>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${projectId}/issues/${issue.issue_id}`}>{issue.identifier}</Link><div className="text-xs text-slate-500">{issue.title}</div></td>
              <td className="px-3 py-3"><Badge tone={statusTone(issue.lifecycle_stage)}>{humanizeLabel(issue.display_status)}</Badge></td>
              <td className="px-3 py-3">{issue.blocker?.message ?? issue.stop_reason ?? "blocked"}</td>
              <td className="px-3 py-3">{issue.runtime_defect?.next_action ?? issue.self_defect_routing?.next_action ?? issue.failure?.message ?? "inspect issue evidence"}</td>
            </tr>
          ))}
          {supplementalSuppressions.map((suppression) => (
            <tr key={`${suppression.issue_id}-${suppression.reason_kind}`}>
              <td className="px-3 py-3"><Link className="font-semibold text-blue-700" href={`/projects/${projectId}/issues/${suppression.issue_id}`}>{suppression.identifier}</Link></td>
              <td className="px-3 py-3"><Badge tone="warn">{humanizeLabel(suppression.reason_kind)}</Badge></td>
              <td className="px-3 py-3">{suppression.reason}</td>
              <td className="px-3 py-3">waiting for eligibility</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function BoundedHistory({ issues, projectId }: { issues: IssueDetail[]; projectId: string }) {
  const visibleIssues = issues.slice(0, RECENT_HISTORY_LIMIT);
  return (
    <div className="grid gap-3">
      {issues.length > RECENT_HISTORY_LIMIT ? (
        <p className="text-sm text-slate-600">Showing newest {RECENT_HISTORY_LIMIT} of {issues.length} terminal runs. Open an issue for full runner history.</p>
      ) : null}
      <IssueTable issues={visibleIssues} projectId={projectId} />
    </div>
  );
}

function DefectIssueList({ issues, projectId }: { issues: IssueDetail[]; projectId: string }) {
  return (
    <div className="grid gap-2 text-sm">
      {issues.map((issue) => (
        <Link key={issue.issue_id} className="rounded-xl border border-red-200 bg-red-50 p-3 text-red-950" href={`/projects/${projectId}/issues/${issue.issue_id}`}>
          <span className="font-semibold">{issue.identifier}</span> {humanizeLabel(issue.runtime_defect?.classification ?? issue.self_defect_routing?.kind ?? issue.self_defect_routing?.defect_kind ?? issue.failure?.kind)}: {issue.runtime_defect?.next_action ?? issue.self_defect_routing?.next_action ?? issue.failure?.message}
        </Link>
      ))}
    </div>
  );
}

function QuotaCompact({ quota }: { quota: QuotaResult }) {
  if (quota.status === "unavailable") {
    return (
      <Link className="rounded-full border border-amber-200 bg-amber-50 px-2 py-1 font-medium text-amber-900 hover:border-amber-400" href="/quota">
        5h quota unavailable
      </Link>
    );
  }

  const window = quota.quota.buckets.flatMap((bucket) => bucket.windows).find((entry) => entry.label.toLowerCase() === "5h");
  const remaining = quotaRemainingPercent(window);
  return (
    <Link className="rounded-full border border-slate-200 bg-slate-50 px-2 py-1 font-medium text-slate-700 hover:border-slate-400" href="/quota">
      5h quota {remaining}% remaining
    </Link>
  );
}

function QuotaBucketBlock({ bucket }: { bucket: QuotaBucket }) {
  return (
    <section className="rounded-xl border border-slate-200 bg-slate-50 p-3">
      <div className="mb-2 flex items-center justify-between gap-2">
        <div>
          <h3 className="font-semibold">{bucket.title}</h3>
          <p className="text-xs uppercase tracking-wide text-slate-500">{bucket.scope === "general" ? "general limit" : "model-specific limit"}</p>
        </div>
      </div>
      <div className="grid gap-3 md:grid-cols-2">
        {bucket.windows.map((window) => <QuotaWindowBar key={`${bucket.title}-${window.label}`} window={window} />)}
      </div>
    </section>
  );
}

function QuotaWindowBar({ window }: { window: QuotaWindow }) {
  const remaining = quotaRemainingPercent(window);
  const used = window.usedPercent ?? Math.max(0, 100 - remaining);
  return (
    <div className="rounded-xl border border-slate-200 bg-white p-4">
      <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between">
        <div><div className="font-semibold">{window.label} window</div><div className="text-sm text-slate-500">{remaining}% remaining</div></div>
        <div className="text-sm text-slate-600">reset {shortTime(window.resetAt)}</div>
      </div>
      <Progress value={remaining} />
      <div className="mt-2 text-sm text-slate-600">{remaining}% remaining · {used}% used</div>
    </div>
  );
}

function QuotaHealthLine({ quota }: { quota: QuotaResult }) {
  return (
    <span>
      {quota.command} · source {quota.sourceHealth} · parse {quota.parseHealth} · fetched {shortTime(quota.fetchedAt)}
    </span>
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
      <div className="text-xs text-slate-500">{tokens.splitProven ? `${formatNumber(tokens.nonCached)} non-cache · ${cacheSummary(tokens)}` : cacheSummary(tokens)}</div>
      <div className={`text-xs ${tokens.statusTone === "warn" || tokens.statusTone === "bad" ? "font-medium text-amber-700" : "text-slate-500"}`}>{metricsSummary(tokens)}</div>
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
      <div className="flex min-w-0 flex-col gap-1 border-b border-slate-100 px-4 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-3">
        <h2 className="min-w-0 text-sm font-semibold uppercase tracking-wide text-slate-700">{title}</h2>
        <div className="min-w-0 break-words text-xs text-slate-500 sm:text-right">{action}</div>
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
function severityRank(value?: string | null): number {
  switch (value) {
    case "critical": return 4;
    case "high": return 3;
    case "medium": return 2;
    case "low": return 1;
    default: return 0;
  }
}


type Tone = "good" | "warn" | "bad" | "idle";

export function Badge({ children, tone = "idle" }: { children: React.ReactNode; tone?: Tone }) {
  return <span className={`inline-flex max-w-full rounded-full px-2 py-1 text-xs font-semibold leading-tight ${badgeClass(tone)}`}>{children}</span>;
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

function isBlockedProject(project: DashboardProjectCard): boolean {
  const health = project.runner_health.toLowerCase();
  const liveness = project.liveness.status.toLowerCase();
  return isProblemStatus(project.runner_health) || health.includes("quota") || liveness.includes("blocked") || liveness.includes("failed");
}

function hasOverviewAttention(project: DashboardProjectCard): boolean {
  const reasonCode = project.liveness.primary_reason_code.toLowerCase();
  if (reasonCode.startsWith("active_")) return false;
  if (isActiveProject(project) && !isBlockedProject(project)) return false;
  return isBlockedProject(project) || hasUsefulIdleReason(project);
}

function isActiveProject(project: DashboardProjectCard): boolean {
  const health = project.runner_health.toLowerCase();
  const liveness = project.liveness.status.toLowerCase();
  const reasonCode = project.liveness.primary_reason_code.toLowerCase();
  return health.includes("active") || liveness.includes("active") || liveness.includes("running") || reasonCode.startsWith("active_") || project.running_issues.length > 0;
}

function hasUsefulIdleReason(project: DashboardProjectCard): boolean {
  const status = project.liveness.status.toLowerCase();
  const reasonCode = project.liveness.primary_reason_code.toLowerCase();
  const reason = `${project.liveness.primary_reason_detail} ${project.liveness.reason}`.toLowerCase();
  return (status.includes("idle") || reasonCode === "idle" || reasonCode === "no_runnable_candidate") && !reason.includes("polling normally") && !reason.includes("no running sessions");
}

function secondaryLivenessDetail(project: DashboardProjectCard): string {
  const primary = project.liveness.primary_reason_detail.trim();
  const reason = project.liveness.reason.trim();
  if (!reason || reason === primary) return "—";
  return humanizeLabel(reason);
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
  const session = currentRunnerSession(issue);
  return issue.lifecycle_stage === "running" || session?.process_alive === true;
}

type DefectGroup = {
  fingerprint: string;
  severity?: string | null;
  kind: string;
  relation: string;
  sourceIssues: string[];
  managedIssues: string[];
  occurrences: number;
  records: number;
  firstSeenAt?: string | null;
  lastSeenAt?: string | null;
  status: string;
  nextAction: string;
};

function groupDefects(defects: SelfDefectRouteSummary[]): DefectGroup[] {
  const groups = new Map<string, DefectGroup>();
  for (const defect of defects) {
    const current = groups.get(defect.fingerprint);
    const sourceIssue = defect.source_issue_identifier ?? defect.source_issue_id;
    const managedIssue = defect.managed_issue_identifier ?? defect.managed_issue_id;
    const occurrences = Math.max(1, defect.occurrence_count ?? 1);
    const status = defect.source_status ?? "active";
    if (!current) {
      groups.set(defect.fingerprint, {
        fingerprint: defect.fingerprint,
        severity: defect.severity,
        kind: humanizeLabel(defect.kind ?? defect.defect_kind),
        relation: humanizeLabel(defect.relation ?? defect.relation_mode ?? "unrelated"),
        sourceIssues: sourceIssue ? [sourceIssue] : [],
        managedIssues: managedIssue ? [managedIssue] : [],
        occurrences,
        records: 1,
        firstSeenAt: defect.first_seen_at,
        lastSeenAt: defect.last_seen_at,
        status,
        nextAction: defect.next_action ?? "inspect evidence",
      });
      continue;
    }
    if (sourceIssue && !current.sourceIssues.includes(sourceIssue)) current.sourceIssues.push(sourceIssue);
    if (managedIssue && !current.managedIssues.includes(managedIssue)) current.managedIssues.push(managedIssue);
    current.occurrences += occurrences;
    current.records += 1;
    const currentActive = isActiveStatus(current.status);
    const defectActive = isActiveStatus(status);
    if (defectActive && !currentActive) {
      current.status = status;
      current.severity = defect.severity;
      current.nextAction = defect.next_action ?? current.nextAction;
    } else if (defectActive === currentActive && severityRank(defect.severity) > severityRank(current.severity)) {
      current.severity = defect.severity;
    }
    if (!current.firstSeenAt || (defect.first_seen_at && defect.first_seen_at < current.firstSeenAt)) current.firstSeenAt = defect.first_seen_at;
    if (!current.lastSeenAt || (defect.last_seen_at && defect.last_seen_at > current.lastSeenAt)) {
      current.lastSeenAt = defect.last_seen_at;
      if (defectActive || !currentActive) current.nextAction = defect.next_action ?? current.nextAction;
    }
  }
  return [...groups.values()].sort((left, right) => {
    const active = Number(isActiveStatus(right.status)) - Number(isActiveStatus(left.status));
    if (active !== 0) return active;
    const severity = severityRank(right.severity) - severityRank(left.severity);
    if (severity !== 0) return severity;
    return String(right.lastSeenAt ?? "").localeCompare(String(left.lastSeenAt ?? ""));
  });
}

function joinIssueSet(values: string[]): string {
  return values.length ? values.join(", ") : "—";
}

function isActiveStatus(value?: string | null): boolean {
  const normalized = (value ?? "").toLowerCase();
  return normalized !== "completed" && normalized !== "canceled" && normalized !== "cancelled" && normalized !== "resolved";
}

function historySummary(total: number): string {
  if (total === 0) return "empty";
  if (total <= RECENT_HISTORY_LIMIT) return `${total} shown`;
  return `${RECENT_HISTORY_LIMIT} of ${total} shown`;
}

function issueOperationalDetail(issue: IssueDetail, session: ReturnType<typeof currentRunnerSession>): string {
  if (issue.blocker?.message) return issue.blocker.message;
  if (issue.runtime_defect?.next_action) return issue.runtime_defect.next_action;
  if (issue.self_defect_routing?.next_action) return issue.self_defect_routing.next_action;
  if (issue.failure?.message) return issue.failure.message;
  if (session?.activity_error) return session.activity_error;
  if (session?.current_stage) return `stage ${humanizeLabel(session.current_stage)}`;
  return issue.cleanup_status === "clean" ? "ready for follow-up" : `cleanup ${humanizeLabel(issue.cleanup_status)}`;
}

function humanizeLabel(value?: string | null): string {
  return value ? value.replaceAll("_", " ") : "unknown";
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
  freshness: string;
  reason?: string | null;
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
    const freshness = metrics.metrics_freshness || "unknown";
    const splitProven = normalizedStatus !== "unavailable" && (normalizedStatus !== "degraded" || cachedTokens > 0 || cacheRead > 0 || cacheWrite > 0);
    return {
      accounted: Math.max(0, metrics.accounted_total_token_count),
      reported: Math.max(0, metrics.reported_total_token_count),
      nonCached: Math.max(0, metrics.non_cached_token_count),
      cached: cachedTokens,
      cacheRead,
      cacheWrite,
      status,
      freshness,
      reason: metrics.metrics_reason,
      statusTone: tokenStatusTone(status, freshness),
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
    freshness: splitProven ? "unknown" : "unavailable",
    reason: splitProven ? null : "no split metrics collected",
    statusTone: splitProven ? "idle" : "warn",
    splitProven,
  };
}

function tokenSummary(tokens: TokenBreakdown): string {
  const split = tokens.splitProven ? `${formatNumber(tokens.nonCached)} non-cache · ${cacheSummary(tokens)}` : cacheSummary(tokens);
  return `${formatNumber(tokens.accounted)} / ${formatNumber(tokens.reported)} tokens · ${split} · ${metricsSummary(tokens)}`;
}

function metricsSummary(tokens: TokenBreakdown): string {
  const freshness = tokens.freshness && tokens.freshness !== "fresh" && tokens.freshness !== tokens.status ? ` · ${tokens.freshness}` : "";
  const reason = tokens.reason ? ` · ${tokens.reason}` : "";
  return `metrics ${tokens.status}${freshness}${reason}`;
}

function tokenStatusTone(status: string, freshness = "fresh"): Tone {
  if (freshness === "stale") return "warn";
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
