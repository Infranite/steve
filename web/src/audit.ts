/* The audit trail as the settings read it (`GET /api/audit`): one entry is who did what to what, with the detail
 * its writer kept. Nothing here knows the actions: a detail is shown by its shape (an id by what it is called,
 * a `before`/`after` pair as what changed), so an entry of an action added later still reads. */
export type AuditEntry = { id: number; ts: number; actor: string; action: string; target: string; detail: Record<string, unknown> | null }
export type AuditPage = { entries: AuditEntry[]; /** what the ids of the entries are called now */ names: Record<string, string>; more: boolean }
type Names = Record<string, string>

/** `settings.models.migrate` → `settings_models_migrate`: an action's key among the labels */
export const actionKey = (action: string) => action.replaceAll(".", "_")

/** the keys an entry's action may have words under, the most telling first: an action on something of a `type`
 *  (`party.create` of an `org`) for that type, then for itself */
export function actionKeys(e: AuditEntry): string[] {
  const key = actionKey(e.action), type = e.detail?.type
  return typeof type === "string" && /^[a-z_]+$/.test(type) ? [`${key}_${type}`, key] : [key]
}

const isObj = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v)
const same = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null)

/** one value of a detail in words: an id by its name, a model choice as "harness · models · effort", a list joined */
export function shown(v: unknown, names: Names = {}): string {
  if (v == null) return "—"
  if (typeof v === "string") return names[v] ?? v
  if (typeof v !== "object") return String(v)
  if (Array.isArray(v)) return v.map((x) => shown(x, names)).join(", ")
  const o = v as Record<string, unknown>
  if (typeof o.harness === "string" && Array.isArray(o.models)) return [o.harness, o.models.join(", "), o.effort].filter(Boolean).join(" · ")
  return JSON.stringify(v)
}

/** the keys whose value an entry changed (its `before` and `after` differ), in the order they were written */
export function changed(e: AuditEntry): string[] {
  const b = e.detail?.before, a = e.detail?.after
  if (!isObj(b) || !isObj(a)) return []
  return [...new Set([...Object.keys(b), ...Object.keys(a)])].filter((k) => !same(b[k], a[k]))
}

/** an entry's detail as lines of `[key, text]`: a `before`/`after` pair is one "old → new" line per changed key,
 *  every other key is one line; empty lists say nothing */
export function detailLines(e: AuditEntry, names: Names = {}): [string, string][] {
  const d = e.detail ?? {}, keys = changed(e), pair = isObj(d.before) && isObj(d.after)
  const b = (d.before ?? {}) as Record<string, unknown>, a = (d.after ?? {}) as Record<string, unknown>
  const out: [string, string][] = keys.map((k) => [k, `${shown(b[k], names)} → ${shown(a[k], names)}`])
  for (const [k, v] of Object.entries(d)) {
    if (pair && (k === "before" || k === "after")) continue
    if (Array.isArray(v) && !v.length) continue
    out.push([k, shown(v, names)])
  }
  return out
}

/** `models.route` → `route`: the purpose a stored model setting is of; "" for a key that is not a purpose's
 *  (the marker a migration leaves, say) */
export const purposeOf = (key: string) => /^models\.([a-z_]+)$/.exec(key)?.[1] ?? ""
