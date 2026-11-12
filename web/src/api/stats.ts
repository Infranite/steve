/* 统计（Rust Hub，steve-stats，stats.md §4）：汇总、趋势、分组用量、事项表和下钻、会话用量、价格表。
 * TS Hub 没有这些接口：404、HTML 回落或网络错误一律是 NotWired，页面显示“还没有接入”。费用全是估算。 */
import { modReq as req, qs } from "@/api"
import type { TreeStat } from "@/agent-tree"

export type RangeKind = "today" | "7d" | "30d" | "custom"
/** `hour` (0–23): one local hour of a one-day range — the drill-down of an hourly bar (items, usage) */
export type Filter = { range: RangeKind; from?: string; to?: string; project?: string; bot?: string; model?: string; source?: string; hour?: number }
export type P = { p50: number | null; p85: number | null; avg: number | null; sum: number; n: number }
export type UsageTotals = {
  input: number; output: number; cache_read: number; cache_write: number; reasoning: number; total: number; hit_rate: number | null
  est: number; reported: number; unpriced_tokens: number; saved_by_cache: number; calls: number; failed_calls: number
  turns: number; turn_ms: number; turn_errors: number; retries: number; interrupted: number; tools: number; tool_errors: number
}
export type Summary = {
  range: { from: string; to: string; tz: string }
  usage: UsageTotals
  /** `priced_by`: priced tokens per price source; `catalog`: the Models.dev sync (absent on older Hubs) */
  spend: { est: number; reported: number; unpriced_tokens: number; unpriced: Record<string, number>; saved_by_cache: number; by_source: Record<string, number>; gateway_by_purpose: Record<string, number>; priced_by?: Partial<Record<PriceSource, number>>; catalog?: SyncState }
  throughput: { created: number; created_by_source: Record<string, number>; done: number; dropped: number; drop_rate: number | null; wip: number; delivered: number; delivered_auto: number; delivered_manual: number; net: number }
  time: { lead: P; cycle: P; first_deliver: P; wait: P; agent: P; other: P; releases: number; released: number; release_success_rate: number | null; not_released: number; accept_to_release: P }
  quality: { first_pass: number | null; first_pass_n: number; reworks: number; reworks_per_done: number | null; fails: Record<string, number>; stalled_then_done: number | null; rollbacks: number; release_unknown: number; turn_error_rate: number | null; reopened: number }
  human: { decisions: number; by_kind: Record<string, number>; response: P; response_by_kind: Record<string, P>; expired: number; yolo: { decided: number; declined: number; undone: number; rate: number | null } | null; messages: number; per_done: number | null }
  agents: { turns: number; turn_ms: P; tools: number; tool_error_rate: number | null; tool_kinds: Record<string, number>; retries: number; retry_success: number | null; gateway_fallbacks: number; gateway_failed: number; permissions: number; interrupted: number; cancelled: number; tokens_per_turn: number | null; tools_per_turn: number | null }
  per_item: { tokens: P; est_p50: number | null; est_avg: number | null; est_top: [string, number][] }
  context_over_80: number
  since: { tokens: string | null; gateway: string | null }
  prev: Omit<Summary, "prev"> | null
}
export type Metric = "tokens" | "cost" | "done" | "created" | "wip" | "lead" | "hit_rate" | "turns" | "decisions"
export type Group = "none" | "model" | "bot" | "project" | "source" | "purpose" | "token_type"
/** `bucket: "hour"` (a one-day range, ST-43): buckets `YYYY-MM-DDTHH:00` in the stats zone `tz`, `spans` their [start, end) in ms,
 *  `current` the hour that holds now (still running); hours after it are `null` — not yet, not 0 */
export type Series = { bucket?: "day" | "hour"; buckets: string[]; series: { key: string; label: string; values: (number | null)[] }[]; spans?: [number, number][]; current?: number; tz?: string }
/** `models`: by purpose only, the models the purpose actually ran on (calls, busiest first) */
export type UsageRow = UsageTotals & { key: string; label: string; priced?: boolean; price_source?: PriceSource | null; models?: { harness?: string; model: string; calls: number }[] }
export type CountRow = { key: string; label: string; count: number; dim?: "kind" | "tool" | "mcp"; response?: P; subjects?: Record<string, number> }
export type ItemRow = {
  item_id: string; no: number; title: string; status: string; bot_id: string | null; project_id: string | null; created: number; done_at: number | null
  lead_ms: number | null; cycle_ms: number | null; wait_ms: number | null; agent_ms: number | null; turns: number; reworks: number; first_pass: boolean | null; tokens: number; est: number; reported: number
}
export type ItemSort = "est" | "tokens" | "lead" | "wait" | "reworks" | "done_at"
export type ItemOnly = "done" | "created"
export type Usage = { input: number; output: number; cache_read: number; cache_write: number; total: number; hit_rate: number | null; est: number; reported: number; unpriced_tokens: number; rough: boolean }
export type ItemFacts = {
  created: number; first_turn: number | null; first_deliver: number | null; last_deliver: number | null; accepted: number | null; done_at: number | null; dropped_at: number | null
  lead_ms: number | null; cycle_ms: number | null; wait_ms: number | null; agent_ms: number | null; turns: number; delivers: number; reworks: number; first_pass: boolean | null
  fails: number; stalls: number; releases: number; release_fails: number; decisions: number; yolo_decided: number
}
export type ItemDetail = {
  item_id: string; no: number; title: string; status: string; facts: ItemFacts; usage: Usage; age_ms: number
  turns?: { id: string; session_id: string; started: number; ended: number | null; stop_reason: string | null; attempt: number; transient: boolean; tools: number; tool_errors: number; usage: Usage }[]
  marks?: { ts: number; kind: string; from_v: string | null; to_v: string | null; actor: string; ref: string | null }[]
  decisions?: { id: string; kind: string; status: string; created: number; resolved: number | null; by: "me" | "yolo" | null; choice: string | null }[]
  by_model?: (Usage & { model: string; label: string })[]
}
/** `GET /stats/tree/{id}`: the item and everything started from it, each with its own figures */
export type TreeStats = { nodes: TreeStat[]; truncated: boolean }
export type SessionTotals = { session_id: string; turns: number; usage: Usage }
export type Price = { match: string; input: number; output: number; cache_read: number; cache_write: number; note?: string }
/** `table`: the price table (own rows or the built-in defaults), wins; `models_dev`: synced from Models.dev */
export type PriceSource = "table" | "models_dev"
export type SyncState = { synced: number | null; tried: number | null; error: string | null; models: number }
/** a model's price today: `match` is the table row, or `provider/model` of the catalog; `since` 0 = first sync */
export type Quote = Price & { source: PriceSource; via: string | null; since: number }
export type Prices = {
  prices: Price[]; custom: boolean; defaults: Price[]; unpriced: { model: string; tokens: number }[]
  models: { model: string; tokens: number; match: string | null; via: string | null; price?: Quote | null }[]
  catalog?: SyncState & { source: string; url: string; enabled: boolean }
  tz: string | null; hub_tz: string | null
}

const f = (x: Filter) => qs({ range: x.range, from: x.range === "custom" ? x.from : undefined, to: x.range === "custom" ? x.to : undefined, project: x.project, bot: x.bot, model: x.model, source: x.source, hour: x.hour })
/** the filter (it always has `range`) plus more parameters */
const and = (x: Filter, o: Record<string, string | number | undefined>) => { const a = qs(o); return f(x) + (a ? "&" + a.slice(1) : "") }

export const stats = {
  summary: (x: Filter) => req<Summary>("GET", "/stats/summary" + f(x)),
  /** `hourly`: per local hour (only for a one-day range) */
  series: (x: Filter, metric: Metric, group: Group, hourly = false) => req<Series>("GET", "/stats/series" + and({ ...x, hour: undefined }, { metric, group, bucket: hourly ? "hour" : undefined })),
  usage: (x: Filter, by: "model" | "bot" | "project" | "source" | "purpose" | "harness") => req<{ by: string; rows: UsageRow[] }>("GET", "/stats/breakdown" + and(x, { by })),
  counts: (x: Filter, by: "tool" | "decision_kind" | "fail_reason") => req<{ by: string; rows: CountRow[] }>("GET", "/stats/breakdown" + and(x, { by })),
  /** `only`: just the items behind `throughput.done` / `throughput.created` (the drill-down lists) */
  items: (x: Filter, sort: ItemSort, cursor?: string, limit = 50, only?: ItemOnly) => req<{ rows: ItemRow[]; total: number; next: string | null }>("GET", "/stats/items" + and(x, { sort, cursor, limit, only })),
  item: (id: string, brief = false) => req<ItemDetail>("GET", `/stats/items/${encodeURIComponent(id)}` + (brief ? "?brief=1" : "")),
  tree: (id: string) => req<TreeStats>("GET", `/stats/tree/${encodeURIComponent(id)}`),
  session: (id: string) => req<SessionTotals>("GET", `/stats/sessions/${encodeURIComponent(id)}`),
  prices: () => req<Prices>("GET", "/stats/prices"),
  setPrices: (prices: Price[] | null) => req<{ ok: true; prices: Price[] }>("PUT", "/stats/prices", { prices }),
  syncPrices: () => req<{ ok: true; models?: number; changed?: number; unchanged?: boolean }>("POST", "/stats/prices/sync", {}),
  setTz: (tz: string | null) => req<{ ok: true; tz: string | null }>("PUT", "/stats/tz", { tz }),
  rebuild: () => req<{ ok: true }>("POST", "/stats/rebuild", {}),
}
