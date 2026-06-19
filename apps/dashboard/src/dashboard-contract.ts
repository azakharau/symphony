const COST_FIELD_PATTERN = /cost/i;

export type JsonRecord = Record<string, unknown>;

export function normalizeDashboardPayload(payload: unknown): unknown {
  return rewriteDashboardMetadata(removeCostFields(payload));
}

export function normalizeDashboardEventStream(body: string): string {
  return body
    .split("\n")
    .map((line) => {
      if (!line.startsWith("data: ")) {
        return line;
      }
      const raw = line.slice("data: ".length);
      try {
        return `data: ${JSON.stringify(normalizeDashboardPayload(JSON.parse(raw)))}`;
      } catch {
        return line;
      }
    })
    .join("\n");
}

export function removeCostFields(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(removeCostFields);
  }

  if (!isPlainObject(value)) {
    return value;
  }

  return Object.fromEntries(
    Object.entries(value)
      .filter(([key]) => !COST_FIELD_PATTERN.test(key))
      .map(([key, nested]) => [key, removeCostFields(nested)]),
  );
}

function rewriteDashboardMetadata(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(rewriteDashboardMetadata);
  }

  if (!isPlainObject(value)) {
    return value;
  }

  const rewritten: JsonRecord = {};
  for (const [key, nested] of Object.entries(value)) {
    if (key === "metadata" && isPlainObject(nested)) {
      rewritten[key] = {
        ...nested,
        polling_fallback_endpoint: "/api/dashboard",
        live_events_endpoint: "/api/dashboard/events",
      };
      continue;
    }
    rewritten[key] = rewriteDashboardMetadata(nested);
  }
  return rewritten;
}

function isPlainObject(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
