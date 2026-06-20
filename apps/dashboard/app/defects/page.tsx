import { DashboardFrame, DefectsSurface, UnavailablePanel } from "@/src/components";
import { getDefectsData } from "@/src/dashboard-data";

export const dynamic = "force-dynamic";

export default async function DefectsPage() {
  const defects = await getDefectsData();
  return (
    <DashboardFrame>
      {defects.status === "available" ? (
        <DefectsSurface defects={defects.data} />
      ) : (
        <UnavailablePanel title="Defects unavailable" message={defects.error.message} />
      )}
    </DashboardFrame>
  );
}
