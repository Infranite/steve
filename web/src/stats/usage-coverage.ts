/** Text only; the existing views own their UI. Older Hubs' zero totals have no coverage evidence. */
export function coverageValue(
  usage: { coverage?: { uncertain?: number; state: "not_reported" | "unavailable" | "partial" | "complete"; total: number | null; input: number | null; output: number | null; cache_read: number | null; cache_write: number | null }; total: number; input?: number; output?: number; cache_read?: number; cache_write?: number },
  field: "total" | "input" | "output" | "cache_read" | "cache_write",
  format: (n: number) => string,
  labels: { not_reported: string; unavailable: string; partial: string; uncertain?: string },
): string {
  const c = usage.coverage
  if (!c) { const n = usage[field]; return n != null && n > 0 ? format(n) : labels.not_reported }
  const partial = c.uncertain && labels.uncertain ? labels.uncertain : labels.partial
  const value = c[field]
  if (value == null) return c.state === "unavailable" ? labels.unavailable : c.state === "partial" ? partial : labels.not_reported
  return c.state === "partial" ? `≥${format(value)} · ${partial}` : format(value)
}
