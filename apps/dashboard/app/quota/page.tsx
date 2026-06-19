import { DashboardFrame, QuotaSurface } from "@/src/components";
import { getQuotaData } from "@/src/dashboard-data";

export const dynamic = "force-dynamic";

export default async function QuotaPage() {
  const quota = await getQuotaData();
  return (
    <DashboardFrame>
      <QuotaSurface quota={quota} />
    </DashboardFrame>
  );
}
