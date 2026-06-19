import { readDashboardConfig } from "@/src/config";
import { readQuota } from "@/src/quota";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export async function GET() {
  const result = await readQuota(readDashboardConfig());
  return Response.json(result, { status: result.status === "available" ? 200 : 503 });
}
