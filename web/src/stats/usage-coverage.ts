/** Text only; the existing views own their UI. */
export function coverageValue(
  usage: { coverage: { state: "not_reported" | "unavailable" | "partial" | "complete"; total: number | null; input: number | null; output: number | null; cache_read: number | null; cache_write: number | null }; total: number; input?: number; output?: number; cache_read?: number; cache_write?: number },
  field: "total" | "input" | "output" | "cache_read" | "cache_write",
  format: (n: number) => string,
  labels: { not_reported: string; unavailable: string; partial: string },
): string {
  const c = usage.coverage
  const value = c[field]
  if (value == null) return c.state === "unavailable" ? labels.unavailable : c.state === "partial" ? labels.partial : labels.not_reported
  return c.state === "partial" ? `≥${format(value)} · ${labels.partial}` : format(value)
}
