"use client";

import { useEffect, useState } from "react";

export const LIVE_DURATION_REFRESH_MS = 1000;

export function LiveDuration({ startedAtMs, fallbackMs }: { startedAtMs?: number | null; fallbackMs?: number | null }) {
  const [nowMs, setNowMs] = useState<number | null>(null);

  useEffect(() => {
    if (startedAtMs == null || startedAtMs < 0) {
      return;
    }

    const update = () => setNowMs(Date.now());
    const firstTick = window.setTimeout(update, 0);
    const interval = window.setInterval(update, LIVE_DURATION_REFRESH_MS);
    return () => {
      window.clearTimeout(firstTick);
      window.clearInterval(interval);
    };
  }, [startedAtMs]);

  const value = startedAtMs == null || startedAtMs < 0
    ? fallbackMs ?? null
    : nowMs == null ? fallbackMs ?? null : resolveLiveDurationMs(startedAtMs, fallbackMs, nowMs);

  return <span className="tabular-nums">{formatDuration(value)}</span>;
}

export function resolveLiveDurationMs(startedAtMs?: number | null, fallbackMs?: number | null, nowMs = Date.now()): number | null {
  if (startedAtMs == null || startedAtMs < 0) return fallbackMs ?? null;
  return Math.max(0, nowMs - startedAtMs);
}

export function formatDuration(value?: number | null): string {
  if (value == null || value < 0) return "—";
  const totalSeconds = Math.floor(value / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
}
