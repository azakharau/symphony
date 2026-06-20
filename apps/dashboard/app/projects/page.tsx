import { DashboardFrame, ProjectsSurface, UnavailablePanel } from "@/src/components";
import { getDashboardData } from "@/src/dashboard-data";

export const dynamic = "force-dynamic";

export default async function ProjectsPage() {
  const dashboard = await getDashboardData();
  return (
    <DashboardFrame>
      {dashboard.status === "available" ? (
        <ProjectsSurface dashboard={dashboard.data} />
      ) : (
        <UnavailablePanel title="Projects unavailable" message={dashboard.error.message} />
      )}
    </DashboardFrame>
  );
}
