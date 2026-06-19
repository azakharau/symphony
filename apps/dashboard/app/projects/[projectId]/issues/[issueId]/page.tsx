import { DashboardFrame, UnavailablePanel } from "@/src/components";
import { getIssueData } from "@/src/dashboard-data";
import { IssueInspector } from "@/src/issue-inspector";

export const dynamic = "force-dynamic";

export default async function IssuePage({ params }: { params: Promise<{ projectId: string; issueId: string }> }) {
  const { projectId, issueId } = await params;
  const issue = await getIssueData(projectId, issueId);
  return (
    <DashboardFrame>
      {issue.status === "available" ? (
        <IssueInspector issue={issue.data} />
      ) : (
        <UnavailablePanel title="Issue unavailable" message={issue.error.message} />
      )}
    </DashboardFrame>
  );
}
