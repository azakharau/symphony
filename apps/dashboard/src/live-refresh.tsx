"use client";

import { useRouter } from "next/navigation";
import { useEffect, useRef } from "react";

const DEFAULT_REFRESH_MS = 200;
const REFRESH_COALESCE_MS = 50;

export function LiveRefresh({ eventsPath = "/api/dashboard/events", refreshMs = DEFAULT_REFRESH_MS }: { eventsPath?: string; refreshMs?: number }) {
  const router = useRouter();
  const pendingRefresh = useRef<number | null>(null);

  useEffect(() => {
    const refresh = () => {
      if (pendingRefresh.current !== null) return;
      pendingRefresh.current = window.setTimeout(() => {
        pendingRefresh.current = null;
        router.refresh();
      }, REFRESH_COALESCE_MS);
    };

    const interval = window.setInterval(refresh, refreshMs);
    const source = new EventSource(eventsPath);
    source.onmessage = refresh;
    source.addEventListener("dashboard.snapshot", refresh);
    source.addEventListener("dashboard.heartbeat", refresh);
    source.onerror = () => {
      source.close();
    };

    return () => {
      window.clearInterval(interval);
      if (pendingRefresh.current !== null) window.clearTimeout(pendingRefresh.current);
      source.close();
    };
  }, [eventsPath, refreshMs, router]);

  return null;
}
