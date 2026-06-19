import { readDashboardConfig } from "@/src/config";
import {
  normalizeDashboardEventStream,
  normalizeDashboardPayload,
} from "@/src/dashboard-contract";

export type RustApiErrorPayload = {
  status: "unavailable";
  reason: "rust_api_unavailable" | "malformed_json";
  message: string;
};

export async function proxyRustJson(path: string): Promise<Response> {
  const config = readDashboardConfig();
  let upstream: globalThis.Response;

  try {
    upstream = await fetch(`${config.symphonyApiBase}${path}`, {
      headers: { accept: "application/json" },
      cache: "no-store",
    });
  } catch (error) {
    return Response.json(unavailable("rust_api_unavailable", error), { status: 503 });
  }

  const text = await upstream.text();
  let payload: unknown;
  try {
    payload = text ? JSON.parse(text) : null;
  } catch (error) {
    return Response.json(unavailable("malformed_json", error), { status: 502 });
  }

  return Response.json(normalizeDashboardPayload(payload), { status: upstream.status });
}

export async function proxyRustEventStream(): Promise<Response> {
  const config = readDashboardConfig();
  let upstream: globalThis.Response;

  try {
    upstream = await fetch(`${config.symphonyApiBase}/api/dashboard/events`, {
      headers: { accept: "text/event-stream" },
      cache: "no-store",
    });
  } catch (error) {
    return Response.json(unavailable("rust_api_unavailable", error), { status: 503 });
  }

  const body = normalizeDashboardEventStream(await upstream.text());
  return new Response(body, {
    status: upstream.status,
    headers: {
      "content-type": upstream.headers.get("content-type") || "text/event-stream; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

function unavailable(
  reason: RustApiErrorPayload["reason"],
  error: unknown,
): RustApiErrorPayload {
  return {
    status: "unavailable",
    reason,
    message: error instanceof Error ? error.message : String(error),
  };
}
