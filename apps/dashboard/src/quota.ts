import { exec } from "node:child_process";
import { promisify } from "node:util";

import type { DashboardConfig } from "@/src/config";

const execAsync = promisify(exec);

const PERCENT_TOTAL = 100;

type QuotaFailureReason = "command_failed" | "malformed_json" | "timeout";

export type QuotaWindow = {
  label: string;
  resetAt?: string;
  resetAtEpoch?: number;
  usedPercent?: number;
  remainingPercent?: number;
};

export type QuotaBucket = {
  title: string;
  windows: QuotaWindow[];
};

export type QuotaData = {
  raw: unknown;
  buckets: QuotaBucket[];
  used?: number;
  limit?: number;
  remaining?: number;
  resetsAt?: string;
  model?: string;
};

export type QuotaResult =
  | {
      status: "available";
      command: string;
      quota: QuotaData;
    }
  | {
      status: "unavailable";
      command: string;
      reason: QuotaFailureReason;
      message: string;
      quota: null;
    };

export async function readQuota(config: DashboardConfig): Promise<QuotaResult> {
  try {
    const { stdout } = await execAsync(config.ocuCommand, {
      timeout: config.quotaTimeoutMs,
      maxBuffer: 1024 * 1024,
      env: process.env,
    });
    return parseQuotaJson(stdout, config.ocuCommand);
  } catch (error) {
    return unavailable(
      config.ocuCommand,
      commandTimedOut(error) ? "timeout" : "command_failed",
      error instanceof Error ? error.message : String(error),
    );
  }
}

export function parseQuotaJson(stdout: string, command = "ocu --localhost --plain"): QuotaResult {
  let raw: unknown;
  try {
    raw = JSON.parse(stdout);
  } catch (error) {
    return unavailable(
      command,
      "malformed_json",
      error instanceof Error ? error.message : "quota command did not return JSON",
    );
  }

  return {
    status: "available",
    command,
    quota: normalizeQuota(raw),
  };
}

function normalizeQuota(raw: unknown): QuotaData {
  const source = firstRecord(raw, ["quota", "usage", "data"]) ?? asRecord(raw);
  return {
    raw,
    buckets: readBuckets(raw),
    used: readNumber(source, ["used", "used_tokens", "used_credits", "quota_used"]),
    limit: readNumber(source, ["limit", "total", "total_tokens", "quota", "quota_limit"]),
    remaining: readNumber(source, ["remaining", "remaining_tokens", "remaining_credits", "quota_remaining"]),
    resetsAt: readString(source, ["resets_at", "reset_at", "resetAt"]),
    model: readString(source, ["model", "subscription", "plan"]),
  };
}

function readBuckets(raw: unknown): QuotaBucket[] {
  const source = firstRecord(raw, ["quota", "usage", "data"]) ?? asRecord(raw);
  const buckets = source?.buckets;
  if (!Array.isArray(buckets)) {
    return [];
  }

  return buckets.flatMap((bucket): QuotaBucket[] => {
    const record = asRecord(bucket);
    if (!record) {
      return [];
    }

    const title = readString(record, ["title", "name", "label"]);
    if (!title) {
      return [];
    }

    return [
      {
        title,
        windows: readWindows(record.windows),
      },
    ];
  });
}

function readWindows(value: unknown): QuotaWindow[] {
  if (!Array.isArray(value)) {
    return [];
  }

  return value.flatMap((window): QuotaWindow[] => {
    const record = asRecord(window);
    if (!record) {
      return [];
    }

    const label = readString(record, ["label", "title", "name"]);
    if (!label) {
      return [];
    }

    const usedPercent = readNumber(record, ["used_percent", "usedPercent"]);
    const remainingPercent =
      readNumber(record, ["remaining_percent", "remainingPercent"]) ??
      (usedPercent === undefined ? undefined : PERCENT_TOTAL - usedPercent);
    const reset = readReset(record);

    return [
      {
        label,
        ...reset,
        usedPercent,
        remainingPercent,
      },
    ];
  });
}

function readReset(record: Record<string, unknown>): Pick<QuotaWindow, "resetAt" | "resetAtEpoch"> {
  const value = firstValue(record, ["reset_at", "resetAt", "resets_at", "resetsAt"]);

  if (typeof value === "number" && Number.isFinite(value)) {
    return {
      resetAt: new Date(value * 1000).toISOString(),
      resetAtEpoch: value,
    };
  }

  if (typeof value === "string" && value.length > 0) {
    const parsedNumber = Number(value);
    if (Number.isFinite(parsedNumber)) {
      return {
        resetAt: new Date(parsedNumber * 1000).toISOString(),
        resetAtEpoch: parsedNumber,
      };
    }

    const parsedDate = Date.parse(value);
    return {
      resetAt: value,
      resetAtEpoch: Number.isFinite(parsedDate) ? Math.floor(parsedDate / 1000) : undefined,
    };
  }

  return {};
}

function firstRecord(raw: unknown, keys: string[]): Record<string, unknown> | undefined {
  const record = asRecord(raw);
  if (!record) {
    return undefined;
  }
  for (const key of keys) {
    const nested = asRecord(record[key]);
    if (nested) {
      return nested;
    }
  }
  return undefined;
}

function firstValue(record: Record<string, unknown>, keys: string[]): unknown {
  for (const key of keys) {
    if (key in record) {
      return record[key];
    }
  }
  return undefined;
}

function readNumber(record: Record<string, unknown> | undefined, keys: string[]): number | undefined {
  if (!record) {
    return undefined;
  }
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "number" && Number.isFinite(value)) {
      return value;
    }
    if (typeof value === "string") {
      const parsed = Number(value);
      if (Number.isFinite(parsed)) {
        return parsed;
      }
    }
  }
  return undefined;
}

function readString(record: Record<string, unknown> | undefined, keys: string[]): string | undefined {
  if (!record) {
    return undefined;
  }
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.length > 0) {
      return value;
    }
  }
  return undefined;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
}

function unavailable(command: string, reason: QuotaFailureReason, message: string): QuotaResult {
  return {
    status: "unavailable",
    command,
    reason,
    message,
    quota: null,
  };
}

function commandTimedOut(error: unknown): boolean {
  return Boolean(
    error &&
      typeof error === "object" &&
      ("killed" in error || "signal" in error) &&
      ((error as { killed?: boolean }).killed === true || (error as { signal?: string }).signal === "SIGTERM"),
  );
}
