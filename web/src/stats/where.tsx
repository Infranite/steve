/* 钱和时间花在哪（stats v2）：一个开关（事项 / Bot / 模型 / harness / 用途）切费用或 Agent 时长的构成（affectUI Compact
 * Breakdown，最多五份，多的并成“其他”），下面是这段时间最贵的事项，点一件进它的费用去向（#/stats/items/:id，Cost Waterfall）。
 * 数字：Bot / 模型 / harness / 用途来自 GET /stats/breakdown（估算费用 `est`、回合时长 `turn_ms`）；事项来自 GET /stats/items
 * （每件事项的全部估算费用和 Agent 时长，含这段时间以外的部分，所以加起来不等于这段时间的费用，图下面写明）。 */
import { useState } from "react"
import { ChevronRightIcon } from "lucide-react"
import { cn } from "cn"
import { Panel, PanelSection } from "@/panel"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { go, ItemNo, Segmented, StatusDot } from "@/ui"
import { st } from "@/store"
import { stats, type Filter, type ItemRow, type Summary, type UsageRow } from "@/api/stats"
import { CompactBreakdown } from "@/components/affect/compact-breakdown"
import { OTHER, SERIES } from "@/stats/chart"
import { splitParts } from "@/stats/split"
import { Est } from "@/stats/kpi"
import { fmtEst, fmtMs, fmtN, fmtUsd, useStats } from "@/stats/format"
import { harnessName } from "@/model-picker"
import { t, useT } from "@/i18n"

export type WhereTab = "item" | "bot" | "model" | "harness" | "purpose"
type Measure = "cost" | "time"
const TABS = (): [WhereTab, string][] => [["item", t("stats:v2.where.tab.item")], ["bot", "Bot"], ["model", t("stats:v2.where.tab.model")], ["harness", "Harness"], ["purpose", t("stats:v2.where.tab.purpose")]]

/** the rows of the split, as the parts helper wants them: `est` (cost) and `total` (here: the measure when it isn't cost) */
type Part = { key: string; label: string; est: number; total: number }

export function WhereSection({ s, f, fkey, live }: { s: Summary; f: Filter; fkey: string; live: boolean }) {
  const t = useT()
  const [tab, setTab] = useState<WhereTab>("bot")
  const [measure, setMeasure] = useState<Measure>("cost")
  const by = tab === "item" ? null : tab
  const [u] = useStats(() => (by ? stats.usage(f, by).then((v) => ({ ...v, tab })) : Promise.resolve(null)), [fkey, tab], live)
  /* the items: the 100 most expensive (or busiest) of the range, enough for four parts and the rest */
  const [it] = useStats(() => (tab === "item" ? stats.items(f, "est", undefined, 100).then((v) => ({ ...v, tab })) : Promise.resolve(null)), [fkey, tab], live)
  let parts: Part[] | null = null
  if (tab === "item" && it.s === "ok" && it.v) parts = it.v.rows.map((r) => ({ key: r.item_id, label: `${r.item_id} ${r.title}`, est: measure === "cost" ? r.est : 0, total: measure === "cost" ? 0 : r.agent_ms ?? 0 }))
  else if (tab !== "item" && u.s === "ok" && u.v && u.v.tab === tab) parts = u.v.rows.map((r: UsageRow) => ({ key: r.key, label: label(tab, r), est: measure === "cost" ? r.est : 0, total: measure === "cost" ? 0 : r.turn_ms }))
  const err = tab === "item" ? (it.s === "error" ? it.msg : null) : u.s === "error" ? u.msg : null
  const split = parts && splitParts(parts, SERIES, { color: OTHER, label: (count) => t("stats:usage.otherN", { count }) }, t("stats:word.unknown"))
  const time = measure === "time"
  const nothing = split && !split.items.length
  /* the split's total is the range's, so it can carry the change against the range before (cost only: the item tab's
     total is the items' whole cost, not the range's) */
  const before = !time && tab !== "item" && s.prev ? s.prev.spend.est : 0
  const total = split ? split.items.reduce((a, x) => a + x.value, 0) : 0
  return (
    <Panel k="where" data-where>
      <PanelSection k="split" right={
        <div className="flex w-full flex-wrap items-center justify-between gap-2">
          <Segmented size="sm" value={tab} onChange={setTab} options={TABS()} label={t("stats:v2.where.tabAria")} />
          <Segmented size="sm" value={measure} onChange={setMeasure} options={[["cost", t("stats:v2.where.cost")], ["time", t("stats:v2.where.time")]] as const} label={t("stats:v2.where.measureAria")} />
        </div>}>
        <div data-where-split={tab} data-where-measure={measure}>
          {!split ? <div className="flex h-32 items-center justify-center type-callout text-muted-foreground" role={err ? "alert" : "status"}>{err ?? <Spinner className="size-4 text-muted-foreground" />}</div>
            : nothing ? <div className="py-6 type-callout text-muted-foreground" data-where-empty>{time ? t("stats:v2.where.emptyTime") : t("stats:v2.where.emptyCost")}</div>
            : <CompactBreakdown key={tab + measure} bare items={split.items} title={time ? t("stats:v2.where.splitTime") : t("stats:v2.where.splitCost")}
              totalLabel={tab === "item" ? (time ? t("stats:v2.where.itemsTime") : t("stats:v2.where.itemsCost")) : time ? t("stats:v2.where.totalTime") : t("stats:usage.totalCost")}
              format={time ? (n) => fmtMs(n) : (n) => (n === 0 ? "$0" : fmtUsd(n))} delta={before > 0 && total > 0 ? ((total - before) / before) * 100 : undefined} deltaTone="neutral"
              footer={<span className="whitespace-normal">{note(tab, measure, s)}</span>} rowProps={(x) => ({ "data-where-part": x.key })} />}
        </div>
      </PanelSection>
      <TopItems f={f} fkey={fkey} live={live} />
    </Panel>
  )
}

const label = (tab: WhereTab, r: UsageRow) => (tab === "harness" && r.key ? harnessName(r.key) : tab === "bot" && r.key ? st.bots[r.key]?.n ?? r.label : r.label) || t("stats:word.unknown")
/** what the split counts, said under it */
function note(tab: WhereTab, measure: Measure, s: Summary) {
  if (tab === "item") return measure === "time" ? t("stats:v2.where.itemTimeNote") : t("stats:v2.where.itemCostNote")
  if (measure === "time") return t("stats:v2.where.timeNote")
  return s.spend.unpriced_tokens > 0 ? t("stats:v2.where.unpricedNote", { n: fmtN(s.spend.unpriced_tokens) }) : t("stats:v2.where.costNote")
}

const PAGE = 8
/** the range's most expensive items, each opening where its cost went */
function TopItems({ f, fkey, live }: { f: Filter; fkey: string; live: boolean }) {
  const t = useT()
  const [limit, setLimit] = useState(PAGE)
  const [d] = useStats(() => stats.items(f, "est", undefined, limit), [fkey, limit], live)
  const rows = d.s === "ok" ? d.v.rows.filter((r) => r.est > 0 || r.tokens > 0 || r.reported > 0) : []
  const max = Math.max(0, ...rows.map((r) => r.est))
  return (
    <PanelSection k="top-items" label={t("stats:v2.where.top")} meta={d.s === "ok" ? t("stats:v2.where.topMeta", { count: d.v.total }) : undefined} className="pt-2">
      {d.s === "loading" ? <div className="flex h-24 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
        : d.s !== "ok" ? <div className="py-4 type-callout text-muted-foreground" role="alert">{d.s === "error" ? d.msg : t("stats:items.loadFailed")}</div>
        : !rows.length ? <div className="py-4 type-callout text-muted-foreground">{t("stats:items.emptyActive")}</div>
        : <>
          <div className="-mx-2 flex flex-col" role="list" data-top-items>
            <div className="hidden grid-cols-[minmax(0,1fr)_7rem_6rem_9rem] items-center gap-x-4 px-2 pb-1.5 type-footnote text-muted-foreground @3xl/stats:grid" aria-hidden>
              <span>{t("stats:col.item")}</span><span className="text-right">{t("stats:v2.where.colAgent")}</span><span className="text-right">tokens</span><span className="inline-flex items-center justify-end gap-1">{t("stats:col.cost")}<Est /></span>
            </div>
            {rows.map((r) => <TopRow key={r.item_id} r={r} max={max} />)}
          </div>
          {d.v.total > rows.length && rows.length === limit && limit < 100 && <Button size="xs" variant="ghost" className="mt-1 -ml-2 text-muted-foreground" onClick={() => setLimit(limit === PAGE ? 25 : 100)} data-top-more>{t("stats:overview.showMore", { count: Math.min(d.v.total, limit === PAGE ? 25 : 100) - limit })}</Button>}
        </>}
    </PanelSection>
  )
}

function TopRow({ r, max }: { r: ItemRow; max: number }) {
  const t = useT()
  const bot = r.bot_id ? st.bots[r.bot_id]?.n ?? r.bot_id : null
  const open = () => go("#/stats/items/" + r.item_id)
  return (
    <button type="button" role="listitem" onClick={open} className="group grid w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-1 rounded-lg px-2 py-2 text-left transition-colors hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none @3xl/stats:grid-cols-[minmax(0,1fr)_7rem_6rem_9rem]" data-top-item={r.item_id} title={t("stats:v2.where.openCost")}>
      <span className="flex min-w-0 items-center gap-2">
        <StatusDot s={r.status} /><ItemNo id={r.item_id} />
        <span className="min-w-0 truncate type-body text-foreground">{r.title}</span>
        {bot && <span className="hidden shrink-0 type-footnote text-muted-foreground @xl/stats:inline">{bot}</span>}
      </span>
      <span className="hidden text-right type-callout tabular-nums text-muted-foreground @3xl/stats:block">{fmtMs(r.agent_ms)}</span>
      <span className="hidden text-right type-callout tabular-nums text-muted-foreground @3xl/stats:block">{fmtN(r.tokens)}</span>
      <span className="flex items-center justify-end gap-2">
        <span className="relative hidden h-1.5 w-12 overflow-hidden rounded-full bg-muted @md/stats:block" aria-hidden>
          {max > 0 && r.est > 0 && <span className="absolute inset-y-0 left-0 rounded-full bg-[var(--series-1)]" style={{ width: `${Math.max(4, (r.est / max) * 100)}%` }} />}
        </span>
        <span className={cn("min-w-14 text-right type-callout font-medium tabular-nums", !r.tokens && "text-muted-foreground")} data-unpriced={r.unpriced_tokens || undefined}>{r.tokens ? fmtEst(r.est, r.unpriced_tokens) : r.reported ? "≥" + fmtUsd(r.reported) : "—"}</span>
        <ChevronRightIcon className="size-3.5 shrink-0 text-tertiary opacity-0 transition-opacity group-hover:opacity-100" />
      </span>
    </button>
  )
}
