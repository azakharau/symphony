"use client";

import { useRouter } from "next/navigation";
import { useEffect, useRef } from "react";

export function LiveRefresh({ eventsPath = "/api/dashboard/events", refreshMs = 5_000 }: { eventsPath?: string; refreshMs?: number }) {
  const router = useRouter();
  const pendingRefresh = useRef<number | null>(null);

  useEffect(() => {
    const refresh = () => {
      if (pendingRefresh.current !== null) return;
      pendingRefresh.current = window.setTimeout(() => {
        pendingRefresh.current = null;
        router.refresh();
      }, 250);
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
