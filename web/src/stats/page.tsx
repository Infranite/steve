/* 统计页（stats.md §6）：#/stats 与下钻 #/stats/items/:id。只有 Rust Hub 有这些接口；
 * 拿不到（TS Hub）时显示“还没有接入”，不显示示例数据。费用处处标“估”。
 * ST-40 起的层级：运行状态（原顶部横栏）→ 核心指标与下钻 → 事项（新建与完成、状态分布）→ 耗时 → 用量与费用 → 质量与介入 → Agent → 事项明细。
 * 每张图都写明单位和时间范围；时间范围、项目、Bot 只在页头选一次，下面所有数字跟着变。 */
import { useEffect, useState } from "react"
import { CalendarIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { st } from "@/store"
import { Page, Segmented, Empty } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { stats, type Filter, type RangeKind, type Summary } from "@/api/stats"
import { UsageKpis } from "@/stats/kpi"
import { Trend } from "@/stats/trend"
import { AgentSection, QualityHuman, Section, TimeSection, UsageSection } from "@/stats/tables"
import { ItemsTable } from "@/stats/items"
import { CoreKpis, DrillPanel, FlowChart, StatusBars, type Drill } from "@/stats/overview"
import { RunState } from "@/yolo/status"
import { StatsItemView } from "@/stats/drill"
import { Trans } from "react-i18next"
import { t, useT } from "@/i18n"
import { fmtN, fmtRange, fmtUsd, hhmm, oneDay, time, zoneName, useStats } from "@/stats/format"

export function StatsRoute({ b, c }: { b?: string; c?: string }) {
  if (b === "items" && c) return <StatsItemView key={c} id={c} />
  return <StatsView />
}

/* labels built at render, so they follow the language */
const RANGES = (): [RangeKind, string][] => [["today", t("stats:page.range.today")], ["7d", t("stats:page.range.d7")], ["30d", t("stats:page.range.d30")], ["custom", t("stats:page.range.custom")]]
/** “上个 7 天”: the range before, as the deltas name it */
const PREV = (r: RangeKind) => (r === "today" ? t("stats:page.prev.today") : r === "7d" ? t("stats:page.prev.d7") : r === "30d" ? t("stats:page.prev.d30") : t("stats:page.prev.custom"))
/** “近 7 天”: the range, as the subtitle names it */
const NAME = (r: RangeKind) => (r === "today" ? t("stats:page.name.today") : r === "7d" ? t("stats:page.name.d7") : r === "30d" ? t("stats:page.name.d30") : t("stats:page.name.custom"))
const localDay = (d = new Date()) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`
/** the filter of this page, kept while the app runs */
let saved: Filter = { range: "7d" }

function CustomRange({ f, set }: { f: Filter; set: (f: Filter) => void }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const today = localDay()
  const [from, setFrom] = useState(f.from ?? localDay(new Date(Date.now() - 13 * 86400000)))
  const [to, setTo] = useState(f.to ?? today)
  const bad = !from || !to || from > to || (Date.parse(to) - Date.parse(from)) / 86400000 + 1 > 366
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger render={<Button size="sm" variant="outline" className="font-normal" data-custom-range />}><CalendarIcon />{f.range === "custom" && f.from ? t("common:time.range", { from: f.from, to: f.to }) : t("stats:page.pickDates")}</PopoverTrigger>
      <PopoverContent align="start" className="w-72">
        <div className="flex flex-col gap-3">
          <label className="flex flex-col gap-1 type-footnote text-muted-foreground">{t("stats:page.from")}<Input type="date" value={from} max={today} onChange={(e) => setFrom(e.target.value)} aria-label={t("stats:page.fromAria")} /></label>
          <label className="flex flex-col gap-1 type-footnote text-muted-foreground">{t("stats:page.to")}<Input type="date" value={to} max={today} onChange={(e) => setTo(e.target.value)} aria-label={t("stats:page.toAria")} /></label>
          {bad && <div className="type-footnote text-muted-foreground">{t("stats:page.badRange")}</div>}
          <Button size="sm" disabled={bad} onClick={() => { set({ ...f, range: "custom", from, to }); setOpen(false) }} data-custom-apply>{t("stats:page.applyRange")}</Button>
        </div>
      </PopoverContent>
    </Popover>
  )
}

export function StatsView() {
  const t = useT()
  const [f, setF0] = useState<Filter>(saved)
  const setF = (x: Filter) => { saved = x; setF0(x) }
  const [picked, setPicked] = useState<string | null>(null)
  const [drill, setDrill] = useState<Drill | null>(null)
  /* the page follows live changes only while its range includes today */
  const live = f.range !== "custom" || (f.to ?? "") >= localDay()
  /* by hour (今天): reload at each quarter hour, so the running hour moves on without a statistics change
     (every zone's hours start on a quarter hour), and the day turns over at midnight */
  const [tick, setTick] = useState(0)
  const ticking = live && oneDay(f)
  useEffect(() => {
    if (!ticking) return
    const timer = setTimeout(() => setTick((x) => x + 1), 900000 - (Date.now() % 900000) + 1500)
    return () => clearTimeout(timer)
  }, [ticking, tick])
  const fkey = JSON.stringify(f) + (ticking ? "#" + tick : "")
  const [s] = useStats(() => stats.summary(f), [fkey], live)
  if (s.s === "absent") return <Unwired id="stats" />
  const bots = Object.keys(st.bots)
  const toolbar = (
    <div className="flex flex-wrap items-center gap-2" data-stats-toolbar>
      <Segmented size="sm" value={f.range === "custom" ? "custom" : f.range} onChange={(v) => { setPicked(null); setDrill(null); if (v !== "custom") setF({ ...f, range: v, from: undefined, to: undefined }); else setF({ ...f, range: "custom", from: f.from ?? localDay(new Date(Date.now() - 13 * 86400000)), to: f.to ?? localDay() }) }} options={RANGES()} />
      {f.range === "custom" && <CustomRange f={f} set={(x) => { setPicked(null); setDrill(null); setF(x) }} />}
      <Select value={f.project ?? ""} onChange={(v) => setF({ ...f, project: v || undefined })} options={[{ v: "", l: t("stats:page.allProjects") }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} testId="stats-project" />
      <Select value={f.bot ?? ""} onChange={(v) => setF({ ...f, bot: v || undefined })} options={[{ v: "", l: t("stats:page.allBots") }, ...bots.map((k) => ({ v: k, l: st.bots[k].n }))]} testId="stats-bot" />
      {s.s === "ok" && <span className="type-footnote text-muted-foreground" data-updated>{t("stats:page.updatedAt", { time: hhmm(s.at) })}</span>}
    </div>
  )
  return (
    <Page title={t("stats:page.title")} sub={s.s === "ok" ? (oneDay(f) ? t("stats:page.subHourly", { name: NAME(f.range), range: fmtRange(s.v.range.from, s.v.range.to), zone: zoneName(s.v.range.tz) }) : t("stats:page.sub", { name: NAME(f.range), range: fmtRange(s.v.range.from, s.v.range.to) })) : t("stats:page.estNote")} max={1040} toolbar={toolbar}>
      {s.s === "loading" ? <div className="flex justify-center py-24" role="status" aria-label={t("stats:word.loading")}><Spinner className="size-5 text-muted-foreground" /></div>
        : s.s === "error" ? <Empty title={t("stats:page.loadFailed")} sub={s.msg} />
        : <StatsBody s={s.v} f={f} fkey={fkey} live={live} picked={picked} setPicked={setPicked} drill={drill} setDrill={setDrill} />}
    </Page>
  )
}

function StatsBody({ s, f, fkey, live, picked, setPicked, drill, setDrill }: { s: import("@/api/stats").Summary; f: Filter; fkey: string; live: boolean; picked: string | null; setPicked: (d: string | null) => void; drill: Drill | null; setDrill: (d: Drill | null) => void }) {
  const t = useT()
  const nothing = s.usage.total === 0 && s.usage.reported === 0 && s.usage.turns === 0 && s.throughput.created === 0 && s.throughput.done === 0 && s.human.decisions === 0 && s.throughput.wip === 0
  const since = s.since.tokens
  const range = fmtRange(s.range.from, s.range.to), rangeName = NAME(f.range), prevName = PREV(f.range)
  const [src] = useStats(() => stats.usage(f, "source"), [fkey], live)
  return (
    <div className="flex flex-col gap-14" data-stats>
      <div className="flex flex-col gap-5" data-overview>
        <RunState className="-mt-4 border-b pb-3" />
        <CoreKpis s={s} f={f} prevName={prevName} rangeName={rangeName} drill={drill} setDrill={setDrill} />
        {drill && <DrillPanel key={drill} d={drill} f={f} fkey={fkey} live={live} rangeName={rangeName} close={() => setDrill(null)} />}
      </div>
      <Section k="items" title={t("stats:page.items")} meta={oneDay(f) ? t("stats:page.itemsMetaHourly", { range }) : t("stats:page.itemsMetaDaily", { range })}>
        <div className="grid grid-cols-1 gap-10 md:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
          <FlowChart s={s} f={f} fkey={fkey} live={live} drill={drill} setDrill={setDrill} />
          <StatusBars f={f} drill={drill} setDrill={setDrill} />
        </div>
      </Section>
      {nothing ? <Empty title={t("stats:page.nothing")} sub={t("stats:page.nothingSub")} /> : <>
        <TimeSection s={s} f={f} fkey={fkey} live={live} range={range} />
        <Section k="usage" title={t("stats:page.usage")} meta={range}>
          <UsageKpis s={s} prevName={prevName} bySource={src.s === "ok" ? src.v.rows : undefined} />
          <PriceNote spend={s.spend} />
          <Trend f={f} fkey={fkey} live={live} range={[s.range.from, s.range.to]} tz={s.range.tz} picked={picked} onPick={setPicked} />
          <UsageSection f={f} fkey={fkey} live={live} picked={picked} />
        </Section>
        <QualityHuman s={s} f={f} fkey={fkey} live={live} range={range} prevName={prevName} />
        <AgentSection s={s} f={f} fkey={fkey} live={live} range={range} />
        <Section k="item-table" title={t("stats:page.itemTable")} meta={t("stats:page.itemTableMeta", { range })}>
          <ItemsTable f={f} fkey={fkey} live={live} />
        </Section>
      </>}
      <footer className="border-t pt-4 type-footnote text-muted-foreground" data-stats-foot>
        {t("stats:page.foot.all", {
          cost: t("stats:page.foot.cost"),
          tokens: s.spend.by_source.backfill != null || s.usage.reported
            ? (since ? t("stats:page.foot.tokensSinceTotal", { since, usd: fmtUsd(s.usage.reported) }) : t("stats:page.foot.noTokensTotal", { usd: fmtUsd(s.usage.reported) }))
            : (since ? t("stats:page.foot.tokensSince", { since }) : t("stats:page.foot.noTokens")),
          gateway: s.since.gateway ? t("stats:page.foot.gatewaySince", { since: s.since.gateway }) : t("stats:page.foot.gatewayLaunch"),
          reprice: t("stats:page.foot.reprice"),
        })}
      </footer>
    </div>
  )
}

/** 费用下面一行：没有价格的模型（费用不完整，未计入的 tokens），以及单价来自哪里 */
function PriceNote({ spend }: { spend: Summary["spend"] }) {
  const t = useT()
  const unpriced = Object.keys(spend.unpriced)
  const by = spend.priced_by ?? {}, c = spend.catalog
  const md = by.models_dev ? (c?.synced ? t("stats:page.priceNote.mdSynced", { time: time(c.synced) }) : "Models.dev") : ""
  const failed = !!md && !!c?.error
  const from = by.table && md ? t(failed ? "stats:page.priceNote.fromBothFailed" : "stats:page.priceNote.fromBoth", { md }) : by.table ? t("stats:page.priceNote.fromTable") : md ? t(failed ? "stats:page.priceNote.fromMdFailed" : "stats:page.priceNote.fromMd", { md }) : ""
  if (!spend.unpriced_tokens && !from) return null
  return (
    <div className="-mt-3 flex flex-col gap-0.5 type-footnote text-muted-foreground" data-price-note>
      {spend.unpriced_tokens > 0 && (
        <div data-unpriced-note>
          <Trans t={t} i18nKey="stats:page.priceNote.unpriced" count={unpriced.length} values={{ models: unpriced.join(t("stats:word.listSep")) || t("stats:word.unknownModel"), tokens: fmtN(spend.unpriced_tokens) }} components={{ warn: <span className="text-attention" /> }} />
        </div>
      )}
      <div data-price-source>
        {from}
        <a href="#/settings/stats" className="text-blue hover:underline">{t("stats:page.priceNote.manage")}</a>
      </div>
    </div>
  )
}
