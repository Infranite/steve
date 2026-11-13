/* 统计页（stats.md §6）：#/stats 与下钻 #/stats/items/:id。只有 Rust Hub 有这些接口；
 * 拿不到（TS Hub）时显示“还没有接入”，不显示示例数据。费用处处标“估”。
 * stats v2 按用途从上到下：筛选（时间、项目、Bot）→ 一行“现在”（进行中 · 等你 · 失败，YOLO 状态）→ 关键数字（和上一段比）
 * → 每天的产出（完成数和费用）→ 钱和时间花在哪（构成 + 最贵的事项，点进费用去向）→ Bot 对比 → harness · 模型对比
 * → 事件浏览器（默认收起）。一节一个标题，卡片只用来装图和表。时间范围、项目、Bot 只在页头选一次，下面所有数字跟着变。 */
import { useEffect, useState } from "react"
import { CalendarIcon } from "lucide-react"
import { Tag } from "@/panel"
import { Button } from "@/components/ui/button"
import { DateField } from "@/components/ui/pickers"
import { Spinner } from "@/components/ui/spinner"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { st } from "@/store"
import { Page, Segmented, Empty } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { stats, statsSeed, type Filter, type RangeKind, type Summary } from "@/api/stats"
import { KeyNumbers } from "@/stats/kpi"
import { Section } from "@/stats/tables"
import { DrillPanel, NowLine, type Drill } from "@/stats/overview"
import { DailyChart } from "@/stats/daily"
import { WhereSection } from "@/stats/where"
import { BotCompare, ModelCompare } from "@/stats/compare"
import { RunState } from "@/yolo/status"
import { StatsItemView } from "@/stats/drill"
import { EventsSection } from "@/stats/events"
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
      <PopoverTrigger render={<Button size="sm" variant="secondary" className="rounded-lg font-normal" data-custom-range />}><CalendarIcon className="text-muted-foreground" />{f.range === "custom" && f.from && f.to ? fmtRange(f.from, f.to) : t("stats:page.pickDates")}</PopoverTrigger>
      <PopoverContent align="start" className="w-72">
        <div className="flex flex-col gap-3">
          <label className="flex flex-col gap-1 type-footnote text-muted-foreground">{t("stats:page.from")}<DateField value={from} max={today} onChange={setFrom} aria-label={t("stats:page.fromAria")} /></label>
          <label className="flex flex-col gap-1 type-footnote text-muted-foreground">{t("stats:page.to")}<DateField value={to} max={today} onChange={setTo} aria-label={t("stats:page.toAria")} /></label>
          {bad && <div className="type-footnote text-muted-foreground">{t("stats:page.badRange")}</div>}
          <Button size="sm" disabled={bad} onClick={() => { set({ ...f, range: "custom", from, to }); setOpen(false) }} data-custom-apply>{t("stats:page.applyRange")}</Button>
        </div>
      </PopoverContent>
    </Popover>
  )
}

export function StatsView() {
  const t = useT()
  const [f, setF0] = useState<Filter>(() => { if (statsSeed.next) { saved = statsSeed.next; statsSeed.next = null } return saved })
  const setF = (x: Filter) => { saved = x; setF0(x) }
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
      <Segmented size="sm" value={f.range === "custom" ? "custom" : f.range} onChange={(v) => { setDrill(null); if (v !== "custom") setF({ ...f, range: v, from: undefined, to: undefined }); else setF({ ...f, range: "custom", from: f.from ?? localDay(new Date(Date.now() - 13 * 86400000)), to: f.to ?? localDay() }) }} options={RANGES()} />
      {f.range === "custom" && <CustomRange f={f} set={(x) => { setDrill(null); setF(x) }} />}
      <Select value={f.project ?? ""} onChange={(v) => { setDrill(null); setF({ ...f, project: v || undefined }) }} options={[{ v: "", l: t("stats:page.allProjects") }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} testId="stats-project" />
      <Select value={f.bot ?? ""} onChange={(v) => { setDrill(null); setF({ ...f, bot: v || undefined }) }} options={[{ v: "", l: t("stats:page.allBots") }, ...bots.map((k) => ({ v: k, l: st.bots[k].n }))]} testId="stats-bot" />
      {s.s === "ok" && <span className="type-footnote tabular-nums text-tertiary" data-updated>{t("stats:page.updatedAt", { time: hhmm(s.at) })}</span>}
    </div>
  )
  return (
    <Page title={t("stats:page.title")} sub={s.s === "ok" ? (oneDay(f) ? t("stats:page.subHourly", { name: NAME(f.range), range: fmtRange(s.v.range.from, s.v.range.to), zone: zoneName(s.v.range.tz) }) : t("stats:page.sub", { name: NAME(f.range), range: fmtRange(s.v.range.from, s.v.range.to) })) : t("stats:page.estNote")} max={1280} toolbar={toolbar}>
      {s.s === "loading" ? <div className="flex justify-center py-24" role="status" aria-label={t("stats:word.loading")}><Spinner className="size-5 text-muted-foreground" /></div>
        : s.s === "error" ? <Empty title={t("stats:page.loadFailed")} sub={s.msg} />
        : <StatsBody s={s.v} f={f} setF={setF} fkey={fkey} live={live} drill={drill} setDrill={setDrill} />}
    </Page>
  )
}

function StatsBody({ s, f, setF, fkey, live, drill, setDrill }: { s: Summary; f: Filter; setF: (f: Filter) => void; fkey: string; live: boolean; drill: Drill | null; setDrill: (d: Drill | null) => void }) {
  const t = useT()
  const nothing = s.usage.total === 0 && s.usage.reported === 0 && s.usage.turns === 0 && s.throughput.created === 0 && s.throughput.done === 0 && s.human.decisions === 0 && s.throughput.wip === 0
  const since = s.since.tokens
  const rangeName = NAME(f.range), prevName = PREV(f.range)
  /* the “现在” drill-downs open under the status line; a day's opens under its chart */
  const nowDrill = drill && !drill.startsWith("day:") ? drill : null
  return (
    <div className="@container/stats flex flex-col gap-10" data-stats>
      <div className="flex flex-col gap-3" data-overview>
        <div className="flex flex-col gap-1">
        <NowLine f={f} drill={drill} setDrill={setDrill}>
          {live ? <Tag tone="ok" data-live><span className="size-1.5 dot-running rounded-full bg-current" />{t("stats:panel.live")}</Tag> : <Tag>{t("stats:panel.past")}</Tag>}
        </NowLine>
        <RunState className="px-2" />
        </div>
        {nowDrill && <DrillPanel key={nowDrill} d={nowDrill} f={f} fkey={fkey} live={live} rangeName={rangeName} close={() => setDrill(null)} />}
        <KeyNumbers s={s} prevName={prevName} />
      </div>
      {nothing ? <Empty title={t("stats:page.nothing")} sub={t("stats:page.nothingSub")} /> : <>
        <Section k="daily" title={t("stats:v2.daily.section")} sub={t("stats:v2.daily.sectionSub")}>
          <DailyChart s={s} f={f} fkey={fkey} live={live} drill={drill} setDrill={setDrill} rangeName={rangeName} />
        </Section>
        <Section k="where" title={t("stats:v2.where.section")} sub={t("stats:v2.where.sectionSub")}>
          <WhereSection s={s} f={f} fkey={fkey} live={live} />
          <PriceNote spend={s.spend} />
        </Section>
        <Section k="bots" title={t("stats:v2.bots.title")} sub={f.bot ? t("stats:v2.bots.subOne") : t("stats:v2.bots.sub")}
          right={f.bot ? <Button size="xs" variant="ghost" className="text-blue" onClick={() => setF({ ...f, bot: undefined })} data-bots-all>{t("stats:v2.bots.showAll")}</Button> : undefined}>
          <BotCompare f={f} fkey={fkey} live={live} onBot={(bot) => { setDrill(null); setF({ ...f, bot }) }} />
        </Section>
        <Section k="models" title={t("stats:v2.models.title")} sub={t("stats:v2.models.sub")}>
          <ModelCompare f={f} fkey={fkey} live={live} />
        </Section>
      </>}
      <EventsSection f={f} fkey={fkey} live={live} />
      <footer className="max-w-[72ch] type-footnote text-tertiary" data-stats-foot>
        {t("stats:page.foot.all", {
          cost: t("stats:page.foot.cost"),
          tokens: s.usage.reported
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
    <div className="flex flex-col gap-0.5 px-1 type-footnote text-muted-foreground" data-price-note>
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
