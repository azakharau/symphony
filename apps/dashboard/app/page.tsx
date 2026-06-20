import { DashboardFrame, OverviewSurface, UnavailablePanel } from "@/src/components";
import { getDashboardData, getQuotaData } from "@/src/dashboard-data";

export const dynamic = "force-dynamic";

export default async function OverviewPage() {
  const [dashboard, quota] = await Promise.all([getDashboardData(), getQuotaData()]);

  return (
    <DashboardFrame>
      {dashboard.status === "available" ? (
        <OverviewSurface dashboard={dashboard.data} quota={quota} />
      ) : (
        <UnavailablePanel title="Dashboard unavailable" message={dashboard.error.message} />
      )}
    </DashboardFrame>
  );
}
