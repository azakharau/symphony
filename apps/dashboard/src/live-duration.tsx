"use client";

import { useEffect, useState } from "react";

export function LiveDuration({ startedAtMs, fallbackMs }: { startedAtMs?: number | null; fallbackMs?: number | null }) {
  const [elapsed, setElapsed] = useState<number | null>(() => fallbackMs ?? null);

  useEffect(() => {
    if (startedAtMs == null || startedAtMs < 0) {
      return;
    }

    const update = () => setElapsed(Math.max(0, Date.now() - startedAtMs));
    const interval = window.setInterval(update, 1000);
    return () => window.clearInterval(interval);
  }, [startedAtMs]);

  const value = startedAtMs == null || startedAtMs < 0
    ? fallbackMs ?? null
    : elapsed;

  return <span className="tabular-nums">{formatDuration(value)}</span>;
}

function formatDuration(value?: number | null): string {
  if (value == null || value < 0) return "—";
  const totalSeconds = Math.floor(value / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
}
