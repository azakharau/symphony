import { readDashboardConfig } from "@/src/config";
import { normalizeDashboardPayload } from "@/src/dashboard-contract";
import {
  acceptanceDashboard,
  defectRoutesFromFixtures,
  emptyDashboard,
  issueFixture,
  projectFixture,
  quotaNormal,
  quotaUnavailable,
} from "@/src/fixtures";
import { readQuota, type QuotaResult } from "@/src/quota";
import type {
  AggregateDashboard,
  DashboardProjectCard,
  DashboardUnavailable,
  IssueDetail,
  ProjectDetail,
  SelfDefectRouteSummary,
} from "@/src/types";

export type DataResult<T> =
  | { status: "available"; data: T }
  | { status: "unavailable"; error: DashboardUnavailable };

export async function getDashboardData(): Promise<DataResult<AggregateDashboard>> {
  const fixture = fixtureState();
  if (fixture === "empty") return { status: "available", data: emptyDashboard };
  if (fixture === "acceptance") return { status: "available", data: acceptanceDashboard };
  return fetchRustJson<AggregateDashboard>("/api/dashboard");
}

export async function getProjectData(projectId: string): Promise<DataResult<ProjectDetail>> {
  const fixture = fixtureState();
  if (fixture) {
    const project = fixture === "empty" ? emptyProject(projectId) : projectFixture(projectId);
    return project
      ? { status: "available", data: project }
      : unavailable("not_found", `Project ${projectId} is not available in dashboard data.`);
  }
  return fetchRustJson<ProjectDetail>(`/api/projects/${encodeURIComponent(projectId)}`);
}

export async function getIssueData(projectId: string, issueId: string): Promise<DataResult<IssueDetail>> {
  const fixture = fixtureState();
  if (fixture) {
    const issue = fixture === "empty" ? undefined : issueFixture(projectId, issueId);
    return issue
      ? { status: "available", data: issue }
      : unavailable("not_found", `Issue ${issueId} is not available in dashboard data.`);
  }
  return fetchRustJson<IssueDetail>(
    `/api/projects/${encodeURIComponent(projectId)}/issues/${encodeURIComponent(issueId)}`,
  );
}

export async function getQuotaData(): Promise<QuotaResult> {
  const fixture = fixtureState();
  if (fixture === "acceptance") return quotaNormal;
  if (fixture === "empty") return quotaUnavailable;
  return readQuota(readDashboardConfig());
}

export async function getDefectsData(): Promise<DataResult<SelfDefectRouteSummary[]>> {
  const fixture = fixtureState();
  if (fixture === "acceptance") return { status: "available", data: defectRoutesFromFixtures() };
  if (fixture === "empty") return { status: "available", data: [] };

  const dashboard = await getDashboardData();
  if (dashboard.status === "unavailable") return dashboard;

  const aggregateRoutes = dashboard.data.projects.flatMap((project) => project.self_defect_routes ?? []);
  const projectDetails = await Promise.all(
    dashboard.data.projects.map((project) => getProjectData(project.project_id)),
  );
  const issueRoutes = projectDetails.flatMap((result) => {
    if (result.status === "unavailable") return [];
    return result.data.active_issues
      .concat(result.data.history_issues)
      .flatMap((issue): SelfDefectRouteSummary[] => {
        const routing = issue.self_defect_routing;
        if (!routing) return [];
        return [{
          source_issue_id: routing.source_context.issue_id,
          source_issue_identifier: routing.source_context.issue_identifier,
          managed_issue_id: routing.managed_bug.issue_id,
          managed_issue_identifier: routing.managed_bug.identifier,
          managed_issue_url: routing.managed_bug.url,
          fingerprint: routing.fingerprint,
          defect_kind: routing.defect_kind,
          severity: routing.severity,
          relation_mode: routing.relation_mode,
          occurrence_count: routing.occurrence_count,
          first_seen_at: routing.first_seen_at,
          last_seen_at: routing.last_seen_at,
          next_action: routing.next_action,
          skipped_blocker_reason: routing.skipped_blocker_reason,
          deadlock_skipped_blocker: routing.deadlock_skipped_blocker,
        }];
      });
  });

  return { status: "available", data: dedupeDefects(aggregateRoutes.concat(issueRoutes)) };
}

export function allRunningIssues(projects: DashboardProjectCard[]) {
  return projects.flatMap((project) => project.running_issues ?? []);
}

export function blockedIssues(projects: ProjectDetail[]) {
  return projects.flatMap((project) =>
    project.active_issues
      .concat(project.history_issues)
      .filter((issue) => issue.blocker || issue.lifecycle_stage === "blocked"),
  );
}

export function dedupeDefects(routes: SelfDefectRouteSummary[]): SelfDefectRouteSummary[] {
  const seen = new Map<string, SelfDefectRouteSummary>();
  for (const route of routes) {
    const key = `${route.fingerprint}:${route.managed_issue_id ?? route.managed_issue_identifier ?? "unmanaged"}`;
    const previous = seen.get(key);
    if (!previous || String(route.last_seen_at ?? "") > String(previous.last_seen_at ?? "")) {
      seen.set(key, route);
    }
  }
  return [...seen.values()].sort((left, right) => {
    const severity = severityRank(right.severity) - severityRank(left.severity);
    if (severity !== 0) return severity;
    return String(right.last_seen_at ?? "").localeCompare(String(left.last_seen_at ?? ""));
  });
}

function severityRank(value?: string | null): number {
  switch (value) {
    case "critical":
      return 4;
    case "high":
      return 3;
    case "medium":
      return 2;
    case "low":
      return 1;
    default:
      return 0;
  }
}

function emptyProject(projectId: string): ProjectDetail | undefined {
  const card = emptyDashboard.projects.find((project) => project.project_id === projectId);
  if (!card) return undefined;
  return {
    metadata: emptyDashboard.metadata,
    project_id: card.project_id,
    name: card.name,
    enabled: card.enabled,
    lifecycle_stage: "queued",
    cleanup_status: card.cleanup_status,
    capacity: card.capacity,
    liveness: card.liveness,
    selected_candidate: null,
    suppression_reasons: [],
    active_issues: [],
    history_issues: [],
  };
}

async function fetchRustJson<T>(path: string): Promise<DataResult<T>> {
  const config = readDashboardConfig();
  let upstream: Response;
  try {
    upstream = await fetch(`${config.symphonyApiBase}${path}`, {
      headers: { accept: "application/json" },
      cache: "no-store",
    });
  } catch (error) {
    return unavailable("rust_api_unavailable", errorMessage(error));
  }

  const text = await upstream.text();
  try {
    const payload = text ? normalizeDashboardPayload(JSON.parse(text)) : null;
    if (!upstream.ok) {
      return unavailable("rust_api_error", JSON.stringify(payload));
    }
    return { status: "available", data: payload as T };
  } catch (error) {
    return unavailable("malformed_json", errorMessage(error));
  }
}

function unavailable<T = never>(reason: string, message: string): DataResult<T> {
  return { status: "unavailable", error: { status: "unavailable", reason, message } };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function fixtureState(): "acceptance" | "empty" | undefined {
  const value = process.env.DASHBOARD_FIXTURE_STATE;
  return value === "acceptance" || value === "empty" ? value : undefined;
}
