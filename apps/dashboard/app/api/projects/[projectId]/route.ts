import { proxyRustJson } from "@/src/rust-api";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export async function GET(_request: Request, context: { params: Promise<{ projectId: string }> }) {
  const { projectId } = await context.params;
  return proxyRustJson(`/api/projects/${encodeURIComponent(projectId)}/ui`);
}
