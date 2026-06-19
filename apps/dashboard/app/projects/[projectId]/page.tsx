import { DashboardFrame, ProjectSurface, UnavailablePanel } from "@/src/components";
import { getProjectData } from "@/src/dashboard-data";

export const dynamic = "force-dynamic";

export default async function ProjectPage({ params }: { params: Promise<{ projectId: string }> }) {
  const { projectId } = await params;
  const project = await getProjectData(projectId);
  return (
    <DashboardFrame>
      {project.status === "available" ? (
        <ProjectSurface project={project.data} />
      ) : (
        <UnavailablePanel title="Project unavailable" message={project.error.message} />
      )}
    </DashboardFrame>
  );
}
