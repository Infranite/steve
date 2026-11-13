/* 对比表（stats v2）：Bot 对比（优化 Bot）和 harness · 模型对比（挑更好更便宜的）。点列头排序，再点反过来；没有数据的格子写“—”，排在最后。
 * Bot：每个这段时间有用量的 Bot 一份 GET /stats/summary?bot=…（同一套定义，和页顶的数字一致）——完成数、一次通过率、
 *      失败（异常卡，按原因）、返工、每件 Agent 时长和费用、缓存命中率、每件 tokens；Bot 名单来自 GET /stats/breakdown?by=bot。
 * harness · 模型：GET /stats/breakdown?by=model&source=agent 列出模型，再按模型取 by=harness，拼成 harness × 模型一行；
 *      只算 Agent 回合（网关调用没有回合，在“用途”里）。回合、出错、时长、估算费用、命中率、tokens 都是这段时间的。 */
import { useState, type ReactNode } from "react"
import { ArrowDownIcon, ArrowUpIcon } from "lucide-react"
import { cn } from "cn"
import { Panel } from "@/panel"
import { Spinner } from "@/components/ui/spinner"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { st } from "@/store"
import { stats, type Filter, type Summary, type UsageRow } from "@/api/stats"
import { Est } from "@/stats/kpi"
import { Table, th } from "@/stats/tables"
import { fmtMs, fmtN, fmtPct, fmtUsd, useStats } from "@/stats/format"
import { harnessName } from "@/model-picker"
import { t, useT } from "@/i18n"

/** a column: its head (with what it means on hover), the number it sorts by, and the cell */
export type Col<R> = { k: string; label: ReactNode; def?: string; v: (r: R) => number | null; cell?: (r: R) => ReactNode; fmt?: (v: number) => string; est?: boolean }

/** a table sorted by any column (descending first, then ascending; no value last either way) */
export function SortTable<R>({ rows, cols, first, rowKey, label, sort0, onRow, rowTitle, k }: {
  rows: R[]; cols: Col<R>[]; first: { label: ReactNode; cell: (r: R) => ReactNode }; rowKey: (r: R) => string; label: string; sort0: string
  onRow?: (r: R) => void; rowTitle?: string; k: string
}) {
  const [sort, setSort] = useState<[string, 1 | -1]>([sort0, -1])
  const col = cols.find((c) => c.k === sort[0]) ?? cols[0]
  const sorted = [...rows].sort((a, b) => {
    const x = col.v(a), y = col.v(b)
    if (x == null || !isFinite(x)) return y == null || !isFinite(y) ? 0 : 1
    if (y == null || !isFinite(y)) return -1
    return (x - y) * sort[1]
  })
  const head = (
    <tr>
      <th className={cn(th, "pr-3")}>{first.label}</th>
      {cols.map((c) => {
        const on = c.k === sort[0]
        const btn = (
          <button type="button" onClick={() => setSort([c.k, on ? (sort[1] === -1 ? 1 : -1) : -1])} className={cn("inline-flex items-center gap-1 rounded-md px-1 -mr-1 hover:text-foreground", on && "text-foreground")} data-sort-col={c.k}>
            {c.label}{c.est && <Est />}
            {on ? (sort[1] === -1 ? <ArrowDownIcon className="size-3" /> : <ArrowUpIcon className="size-3" />) : <span className="w-3" />}
          </button>
        )
        return (
          <th key={c.k} className={cn(th, "pl-3 text-right")} aria-sort={on ? (sort[1] === -1 ? "descending" : "ascending") : "none"}>
            {c.def ? <Tooltip><TooltipTrigger render={btn} /><TooltipContent className="max-w-64">{c.def}</TooltipContent></Tooltip> : btn}
          </th>
        )
      })}
    </tr>
  )
  return (
    <div className="pt-1 pb-2" data-compare={k} data-sorted={`${sort[0]}:${sort[1] === -1 ? "desc" : "asc"}`}>
      <Table label={label} min={150 + cols.length * 82} stick head={head}>
        {sorted.map((r) => (
          <tr key={rowKey(r)} className={cn(onRow && "cursor-pointer hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none")} tabIndex={onRow ? 0 : undefined} title={rowTitle}
            onClick={onRow ? () => onRow(r) : undefined} onKeyDown={onRow ? (e) => e.key === "Enter" && onRow(r) : undefined} data-compare-row={rowKey(r)}>
            <td className="max-w-60 truncate py-2.5 pr-3">{first.cell(r)}</td>
            {cols.map((c) => { const v = c.v(r); return <td key={c.k} className="whitespace-nowrap py-2.5 pl-3 text-right tabular-nums" data-col={c.k}>{c.cell ? c.cell(r) : v == null || !isFinite(v) ? <span className="text-tertiary">—</span> : (c.fmt ?? fmtN)(v)}</td> })}
          </tr>
        ))}
      </Table>
    </div>
  )
}

/** a figure with a grey second line (a rate under its count) */
const Two = ({ a, b }: { a: ReactNode; b?: ReactNode }) => <span className="inline-flex flex-col items-end leading-tight"><span>{a}</span>{b != null && b !== "" && <span className="type-footnote text-tertiary">{b}</span>}</span>
const dash = <span className="text-tertiary">—</span>

/* ───────── Bots ───────── */

type BotRow = { id: string; name: string; s: Summary; usage: UsageRow }
const failsOf = (s: Summary) => Object.values(s.quality.fails).reduce((a, n) => a + n, 0)
const per = (n: number, d: number) => (d > 0 ? n / d : null)

export function BotCompare({ f, fkey, live, onBot }: { f: Filter; fkey: string; live: boolean; onBot: (id: string) => void }) {
  const t = useT()
  const [d] = useStats(async (): Promise<BotRow[]> => {
    const rows = (await stats.usage(f, "bot")).rows.filter((r) => r.key && (r.turns > 0 || r.total > 0 || r.calls > 0))
    const ss = await Promise.all(rows.map((r) => stats.summary({ ...f, bot: r.key })))
    return rows.map((r, i) => ({ id: r.key, name: st.bots[r.key]?.n ?? r.label ?? r.key, s: ss[i], usage: r }))
  }, [fkey], live)
  const cols: Col<BotRow>[] = [
    { k: "done", label: t("stats:v2.bots.done"), def: t("stats:v2.bots.doneDef"), v: (r) => r.s.throughput.done },
    { k: "first_pass", label: t("stats:v2.bots.firstPass"), def: t("stats:v2.bots.firstPassDef"), v: (r) => (r.s.quality.first_pass_n ? r.s.quality.first_pass : null),
      cell: (r) => (r.s.quality.first_pass_n ? <Two a={fmtPct(r.s.quality.first_pass)} b={t("stats:v2.bots.judged", { count: r.s.quality.first_pass_n })} /> : dash) },
    { k: "fails", label: t("stats:v2.bots.fails"), def: t("stats:v2.bots.failsDef"), v: (r) => failsOf(r.s),
      cell: (r) => { const n = failsOf(r.s), p = per(n, r.s.throughput.done); return <span title={failTitle(r.s)}><Two a={<span className={n ? "text-destructive" : undefined}>{n}</span>} b={p != null ? t("stats:v2.bots.perDone", { n: fmtRate(p) }) : undefined} /></span> } },
    { k: "reworks", label: t("stats:v2.bots.reworks"), def: t("stats:v2.bots.reworksDef"), v: (r) => r.s.quality.reworks,
      cell: (r) => <Two a={<span className={r.s.quality.reworks ? "text-attention" : undefined}>{r.s.quality.reworks}</span>} b={r.s.quality.reworks_per_done != null ? t("stats:v2.bots.perDone", { n: fmtRate(r.s.quality.reworks_per_done) }) : undefined} /> },
    { k: "turn_err", label: t("stats:v2.bots.turnErr"), def: t("stats:v2.bots.turnErrDef"), v: (r) => r.s.quality.turn_error_rate,
      cell: (r) => (r.s.quality.turn_error_rate == null ? dash : <Two a={fmtPct(r.s.quality.turn_error_rate, 1)} b={t("stats:v2.bots.ofTurns", { count: r.s.usage.turns, n: fmtN(r.s.usage.turns) })} />) },
    { k: "agent_per_item", label: t("stats:v2.bots.agentPerItem"), def: t("stats:v2.bots.agentPerItemDef"), v: (r) => r.s.time.agent.avg, fmt: fmtMs },
    { k: "cost_per_item", label: t("stats:v2.bots.costPerItem"), est: true, def: t("stats:v2.bots.costPerItemDef"), v: (r) => (r.usage.unpriced_tokens && (r.s.per_item.est_avg ?? 0) < 0.01 ? null : r.s.per_item.est_avg),
      cell: (r) => (r.s.per_item.est_avg == null ? dash : <span title={r.usage.unpriced_tokens ? t("stats:word.estUnpricedTitle", { n: fmtN(r.usage.unpriced_tokens) }) : undefined}>{r.usage.unpriced_tokens && r.s.per_item.est_avg < 0.01 ? t("stats:word.unpriced") : (r.usage.unpriced_tokens ? "≥" : "") + fmtUsd(r.s.per_item.est_avg)}</span>) },
    { k: "hit", label: t("stats:v2.bots.hit"), def: t("stats:v2.bots.hitDef"), v: (r) => r.usage.hit_rate, fmt: (v) => fmtPct(v) },
    { k: "tokens_per_item", label: t("stats:v2.bots.tokensPerItem"), def: t("stats:v2.bots.tokensPerItemDef"), v: (r) => r.s.per_item.tokens.avg },
    { k: "cost", label: t("stats:v2.bots.cost"), est: true, def: t("stats:v2.bots.costDef"), v: (r) => (r.usage.unpriced_tokens && r.usage.est < 0.01 ? null : r.usage.est),
      cell: (r) => <span title={r.usage.unpriced_tokens ? t("stats:word.estUnpricedTitle", { n: fmtN(r.usage.unpriced_tokens) }) : undefined}>{r.usage.unpriced_tokens && r.usage.est < 0.01 ? t("stats:word.unpriced") : (r.usage.unpriced_tokens ? "≥" : "") + fmtUsd(r.usage.est)}</span> },
  ]
  return (
    <Panel k="bots" data-bots>
      {d.s === "loading" ? <div className="flex h-32 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
        : d.s !== "ok" ? <div className="px-5 py-6 type-callout text-muted-foreground" role="alert">{d.s === "error" ? d.msg : t("stats:usage.loadFailed")}</div>
        : !d.v.length ? <div className="px-5 py-6 type-callout text-muted-foreground" data-bots-empty>{t("stats:v2.bots.empty")}</div>
        : <SortTable k="bots" rows={d.v} cols={cols} sort0="done" rowKey={(r) => r.id} label={t("stats:v2.bots.title")} rowTitle={f.bot ? undefined : t("stats:v2.bots.rowTitle")}
          onRow={f.bot ? undefined : (r) => onBot(r.id)} first={{ label: "Bot", cell: (r) => <span className="font-medium text-foreground">{r.name}</span> }} />}
    </Panel>
  )
}
const fmtRate = (n: number) => (n === 0 ? "0" : n < 0.1 ? n.toFixed(2) : n.toFixed(1)).replace(/\.0$/, "")
const failTitle = (s: Summary) => { const fl = FAIL(); return Object.entries(s.quality.fails).filter(([, n]) => n > 0).map(([k, n]) => `${fl[k] ?? k} ${n}`).join(t("stats:word.commaSep")) || undefined }
const FAIL = (): Record<string, string> => ({ stall: t("stats:quality.fail.stall"), error: t("stats:quality.fail.error"), restart: t("stats:quality.fail.restart"), release: t("stats:quality.fail.release"), other: t("stats:quality.fail.other") })

/* ───────── harness · model ───────── */

type ModelRow = UsageRow & { harness: string; model: string; priced?: boolean }

export function ModelCompare({ f, fkey, live }: { f: Filter; fkey: string; live: boolean }) {
  const t = useT()
  const g: Filter = { ...f, source: "agent" }
  const [d] = useStats(async (): Promise<ModelRow[]> => {
    const models = (await stats.usage(g, "model")).rows.filter((r) => r.key && (r.turns > 0 || r.total > 0))
    const by = await Promise.all(models.map((m) => stats.usage({ ...g, model: m.key }, "harness")))
    return models.flatMap((m, i) => by[i].rows.filter((r) => r.turns > 0 || r.total > 0).map((r) => ({ ...r, harness: r.key, model: m.key, priced: m.priced })))
  }, [fkey], live)
  const ok = (r: ModelRow) => (r.turns > 0 ? 1 - r.turn_errors / r.turns : null)
  const cols: Col<ModelRow>[] = [
    { k: "turns", label: t("stats:v2.models.turns"), def: t("stats:v2.models.turnsDef"), v: (r) => r.turns },
    { k: "ok", label: t("stats:v2.models.ok"), def: t("stats:v2.models.okDef"), v: ok,
      cell: (r) => (r.turns ? <Two a={<span className={r.turn_errors ? "text-destructive" : undefined}>{fmtPct(ok(r), 1)}</span>} b={r.turn_errors ? t("stats:v2.models.errors", { count: r.turn_errors }) : undefined} /> : dash) },
    { k: "retries", label: t("stats:v2.models.retries"), def: t("stats:v2.models.retriesDef"), v: (r) => (r.turns ? r.retries : null) },
    { k: "avg_turn", label: t("stats:v2.models.avgTurn"), def: t("stats:v2.models.avgTurnDef"), v: (r) => per(r.turn_ms, r.turns), fmt: fmtMs },
    { k: "cost_per_turn", label: t("stats:v2.models.costPerTurn"), est: true, def: t("stats:v2.models.costPerTurnDef"), v: (r) => (r.unpriced_tokens && r.est < 0.01 ? null : per(r.est, r.turns)),
      cell: (r) => { const v = per(r.est, r.turns); return v == null ? dash : r.unpriced_tokens && r.est < 0.01 ? <a href="#/settings/stats" className="text-blue hover:underline" onClick={(e) => e.stopPropagation()}>{t("stats:word.unpriced")}</a> : <>{r.unpriced_tokens ? "≥" : ""}{fmtUsd(v)}</> } },
    { k: "cost", label: t("stats:v2.models.cost"), est: true, def: t("stats:v2.models.costDef"), v: (r) => (r.unpriced_tokens && r.est < 0.01 ? null : r.est),
      cell: (r) => (r.unpriced_tokens && r.est < 0.01 ? <a href="#/settings/stats" className="text-blue hover:underline">{t("stats:word.unpriced")}</a> : <>{r.unpriced_tokens ? "≥" : ""}{fmtUsd(r.est)}</>) },
    { k: "hit", label: t("stats:v2.models.hit"), def: t("stats:v2.models.hitDef"), v: (r) => r.hit_rate, fmt: (v) => fmtPct(v) },
    { k: "tokens", label: "tokens", def: t("stats:v2.models.tokensDef"), v: (r) => r.total },
    { k: "tokens_per_turn", label: t("stats:v2.models.tokensPerTurn"), def: t("stats:v2.models.tokensPerTurnDef"), v: (r) => per(r.total, r.turns) },
  ]
  return (
    <Panel k="models" data-models>
      {d.s === "loading" ? <div className="flex h-32 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
        : d.s !== "ok" ? <div className="px-5 py-6 type-callout text-muted-foreground" role="alert">{d.s === "error" ? d.msg : t("stats:usage.loadFailed")}</div>
        : !d.v.length ? <div className="px-5 py-6 type-callout text-muted-foreground" data-models-empty>{t("stats:v2.models.empty")}</div>
        : <SortTable k="models" rows={d.v} cols={cols} sort0="turns" rowKey={(r) => r.harness + "\u0000" + r.model} label={t("stats:v2.models.title")}
          first={{ label: t("stats:v2.models.name"), cell: (r) => <span className="flex min-w-0 flex-col leading-tight"><span className="truncate font-mono type-code text-foreground" title={r.model}>{r.model}</span><span className="truncate type-footnote text-muted-foreground">{r.harness ? harnessName(r.harness) : t("stats:word.unknown")}</span></span> }} />}
    </Panel>
  )
}
