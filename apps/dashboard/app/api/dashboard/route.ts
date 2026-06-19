import { proxyRustJson } from "@/src/rust-api";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export function GET() {
  return proxyRustJson("/api/dashboard/ui");
}
