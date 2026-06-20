export type DashboardConfig = {
  symphonyApiBase: string;
  ocuCommand: string;
  quotaTimeoutMs: number;
  refreshMs: number;
  sseEnabled: boolean;
};

const DEFAULT_API_BASE = "http://127.0.0.1:4115";
const DEFAULT_OCU_COMMAND = "ocu --localhost --plain";
const DEFAULT_QUOTA_TIMEOUT_MS = 5_000;
const DEFAULT_REFRESH_MS = 1_000;

export function readDashboardConfig(env: NodeJS.ProcessEnv = process.env): DashboardConfig {
  return {
    symphonyApiBase: trimTrailingSlash(env.SYMPHONY_API_BASE || DEFAULT_API_BASE),
    ocuCommand: env.OCU_COMMAND || DEFAULT_OCU_COMMAND,
    quotaTimeoutMs: readPositiveInt(env.OCU_TIMEOUT_MS, DEFAULT_QUOTA_TIMEOUT_MS),
    refreshMs: readPositiveInt(env.NEXT_PUBLIC_DASHBOARD_REFRESH_MS, DEFAULT_REFRESH_MS),
    sseEnabled: readBoolean(env.NEXT_PUBLIC_DASHBOARD_SSE_ENABLED, true),
  };
}

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

function readPositiveInt(value: string | undefined, fallback: number): number {
  if (!value) {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function readBoolean(value: string | undefined, fallback: boolean): boolean {
  if (!value) {
    return fallback;
  }
  return ["1", "true", "yes", "on"].includes(value.toLowerCase())
    ? true
    : ["0", "false", "no", "off"].includes(value.toLowerCase())
      ? false
      : fallback;
}
