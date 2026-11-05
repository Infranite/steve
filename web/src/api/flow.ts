/* Signals, flows, schedules and event sources (steve-flow, signals-automation.md §4 and schedules.md §8).
 * Only the Rust Hub serves these. The TS Hub answers an unknown GET /api path with the SPA's index.html, and
 * other methods with a 404: both come back as NotWired, and the pages show the module's Unwired placeholder. */
import { useCallback, useEffect, useRef, useState } from "react"
import { modReq as req, Unauthenticated, NotWired, qs } from "@/api"
import { refresh, useHubEvents } from "@/store"
import { lazy } from "@/api/accounts"
import { t } from "@/i18n"

export type Outcome = "pending" | "need" | "item" | "linked" | "digest" | "ignored" | "recorded" | "failed"
export type Signal = {
  id: string; source: string; type: string; summary: string; group_key: string | null; group_label: string | null; thread_key: string | null
  connection: { id: string; name: string } | null; actor: { party_id: string; name: string } | null
  outcome: Outcome; outcome_label: string; reason: string; handled_by: string | null
  flow: { id: string; name: string; kind: string } | null; work_item_id: string | null; decision_id: string | null
  raw_available: boolean; occurred: number; received: number
}
export type SignalList = { stats: { total: number; need: number; item: number; quiet: number }; items: Signal[] }
export type Step = { seq: number; node_id: string; status: "ok" | "skipped" | "waiting" | "failed"; branch: string | null; input: any; output: any; ms: number; error: string | null }
export type Run = {
  id: string; flow_id: string; flow_version: number; status: "running" | "waiting" | "done" | "failed" | "cancelled" | "skipped"; dry_run: boolean; event_id: string | null
  outcome: Outcome | null; outcome_label: string; error?: string | null; steps?: Step[]; started: number; ended: number | null
}
export type SignalDetail = Signal & { runs: Run[] }

export type GNode = { id: string; kind: string; title: string; note?: string; config: Record<string, any>; on_fail?: string | null }
export type GEdge = { from: string; to: string; label?: string }
export type Graph = { nodes: GNode[]; edges: GEdge[] }
export type FlowSummary = {
  id: string; kind: "flow" | "schedule" | "response"; name: string; description: string; enabled: boolean; version: number; origin: string; origin_label: string
  trigger: { source: string | null; types: string[] } | null; uses_ai: boolean; outcomes: number; runs24: number; last_run: number | null; created: number; updated: number
}
export type FlowVersion = { version: number; author: string; note: string; created: number }
export type Flow = FlowSummary & { graph: Graph; versions: FlowVersion[] }

export type SourceType = { key: string; label: string; desc: string; on: boolean; param: string | null; n24: number; flows: number }
export type ScopeMode = "receive" | "record" | "ignore"
export type Source = {
  adapter: string; source: string; name: string; paused: boolean; triage_bot: string | null
  connections: { id: string; name: string; status: string; last_sync: number | null }[]
  scopes: { connection_id: string; ext_id: string; name: string; mode: ScopeMode }[]
  types: SourceType[]
}
export type SourcePatch = { paused?: boolean; triage_bot?: string | null; types?: Record<string, { on?: boolean; param?: string }>; scopes?: { connection_id: string; ext_id: string; mode: ScopeMode }[] }

export type ScheduleMode = "item_auto" | "item" | "propose" | "report"
export type ScheduleRun = { id: string; at: number; started: number; ended: number | null; trigger: "time" | "manual" | "catch_up"; status: "done" | "failed" | "skipped" | "running" | "waiting"; outcome_label: string; item_id: string | null; cost_usd: number; error: string | null }
export type Schedule = {
  id: string; name: string; when: string; describe: string; cron: string; tz: string; what: string; bot: string; mode: ScheduleMode
  project_id: string | null; tags: string[]; missed: "catch_up_once" | "skip"; overlap: "skip" | "run"; budget_usd: number; enabled: boolean; next_at: number | null
  last: { at: number; status: string; text: string; item_id: string | null; cost_usd: number } | null; version: number; created: number; updated: number
}
export type ScheduleBody = Partial<{ name: string; when: string; cron: string; tz: string; what: string; bot: string; mode: ScheduleMode; project_id: string | null; tags: string[]; missed: Schedule["missed"]; overlap: Schedule["overlap"]; budget_usd: number; enabled: boolean }>
export type Parsed = { cron: string; tz: string; describe: string; next: number[]; assumed?: string; notes?: string[]; by: "rule" | "model" }

const get = <T>(p: string) => req<T>("GET", p), post = <T = any>(p: string, b: unknown = {}) => req<T>("POST", p, b), patch = <T = any>(p: string, b: unknown) => req<T>("PATCH", p, b), put = <T = any>(p: string, b: unknown) => req<T>("PUT", p, b), del = <T = any>(p: string) => req<T>("DELETE", p)
const enc = encodeURIComponent

export const flowApi = {
  signals: (p: { range?: "today" | "7d"; q?: string; source?: string; outcome?: string }) => get<SignalList>("/signals" + qs(p)),
  signal: (id: string) => get<SignalDetail>(`/signals/${enc(id)}`),
  raw: (id: string) => get<{ payload: any }>(`/signals/${enc(id)}/raw`),
  correct: (id: string, action: "remind" | "ignore" | "to_item", scope: "one" | "group" = "group") => post<{ ok: true; correction_id: string; changeset_id?: string; item_id?: string; flow_id?: string }>(`/signals/${enc(id)}/correct`, { action, scope }),
  undoConfig: (changeset: string) => post(`/config/${enc(changeset)}/undo`),

  flows: (kind: "flow" | "schedule" = "flow") => get<FlowSummary[]>("/flows" + qs({ kind })),
  flow: (id: string) => get<Flow>(`/flows/${enc(id)}`),
  newFlow: (p: { name: string; description?: string; graph?: Graph }) => post<Flow>("/flows", p),
  draftFlow: (source: string, text: string) => post<Flow>("/flows/draft", { source, text }),
  saveFlow: (id: string, graph: Graph, note?: string) => put<Flow>(`/flows/${enc(id)}`, { graph, note }),
  patchFlow: (id: string, p: { name?: string; description?: string; enabled?: boolean }) => patch<Flow>(`/flows/${enc(id)}`, p),
  editDraft: (id: string, text: string, graph: Graph) => post<{ graph: Graph; note: string }>(`/flows/${enc(id)}/edit-draft`, { text, graph }),
  duplicate: (id: string) => post<Flow>(`/flows/${enc(id)}/duplicate`),
  rollback: (id: string, version: number) => post<Flow>(`/flows/${enc(id)}/rollback`, { version }),
  deleteFlow: (id: string) => del(`/flows/${enc(id)}`),
  dryRun: (id: string, p: { event_id?: string; sample?: object; prefer?: string[]; graph?: Graph }) => post<Run>(`/flows/${enc(id)}/dry-run`, p),
  runFlow: (id: string) => post<{ run_id: string }>(`/flows/${enc(id)}/run`),
  flowRuns: (id: string, limit = 10) => get<Run[]>(`/flows/${enc(id)}/runs` + qs({ limit })),
  run: (id: string) => get<Run>(`/runs/${enc(id)}`),
  cancelRun: (id: string) => post<Run>(`/runs/${enc(id)}/cancel`),

  schedules: () => get<Schedule[]>("/schedules"),
  schedule: (id: string) => get<Schedule & { runs: ScheduleRun[] }>(`/schedules/${enc(id)}`),
  newSchedule: (p: ScheduleBody) => post<Schedule>("/schedules", p),
  patchSchedule: (id: string, p: ScheduleBody) => patch<Schedule>(`/schedules/${enc(id)}`, p),
  deleteSchedule: (id: string) => del(`/schedules/${enc(id)}`),
  runSchedule: (id: string) => post<{ run_id: string }>(`/schedules/${enc(id)}/run`),
  scheduleRuns: (id: string, before?: number, limit = 20) => get<ScheduleRun[]>(`/schedules/${enc(id)}/runs` + qs({ before, limit })),
  parseWhen: (when: string, tz?: string) => post<Parsed>("/schedules/parse", { when, tz }),

  sources: () => get<Source[]>("/sources"),
  patchSource: (adapter: string, p: SourcePatch) => patch<Source>(`/sources/${enc(adapter)}`, p),
  refreshScopes: (adapter: string) => post<Source>(`/sources/${enc(adapter)}/refresh-scopes`),
  /** connected accounts, for the automation chain; the shape belongs to the accounts module */
  connections: () => get<any>("/connections").then((x) => (Array.isArray(x) ? x : x?.connections ?? x?.items ?? []) as unknown[]),
}

export type Load<T> = { data: T | undefined; error: Error | null; unwired: boolean; loading: boolean; reload: () => Promise<void>; set: (v: T) => void }
/** load once on mount (and when `deps` change); `unwired` when the Hub has no such module */
export function useLoad<T>(f: () => Promise<T>, deps: unknown[], live: string[] = []): Load<T> {
  const [data, setData] = useState<T>(), [error, setError] = useState<Error | null>(null), [loading, setLoading] = useState(true)
  const seq = useRef(0)
  const reload = useCallback(async () => {
    const n = ++seq.current
    try { const v = await f(); if (n === seq.current) { setData(v); setError(null) } }
    catch (e) { if (n !== seq.current) return; if (e instanceof Unauthenticated) { void refresh(); return } setError(e as Error) }
    finally { if (n === seq.current) setLoading(false) }
  }, deps)
  useEffect(() => { setLoading(true); void reload() }, [reload])
  useHubEvents(live, () => void reload())
  const unwired = !data && error instanceof NotWired
  return { data, error, unwired, loading, reload, set: setData }
}

/* labels shared by the pages */
export const SOURCE_LABEL: Record<string, string> = lazy({
  mail: () => t("api:source.mail"), cal: () => t("api:source.cal"), im: () => t("api:source.im"), github: () => t("api:source.github"),
  webhook: () => t("api:source.webhook"), time: () => t("api:source.time"), item: () => t("api:source.item"), manual: () => t("api:source.manual"),
})
export const sourceLabel = (s: string | null | undefined) => (s ? SOURCE_LABEL[s] ?? s : "")
export const errText = (e: unknown) => (e instanceof NotWired ? t("api:error.unreachable") : (e as Error)?.message || t("api:error.generic"))
