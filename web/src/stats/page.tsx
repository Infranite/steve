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
import { fmtN, fmtRange, fmtUsd, hhmm, oneDay, time, zoneName, useStats } from "@/stats/format"

export function StatsRoute({ b, c }: { b?: string; c?: string }) {
  if (b === "items" && c) return <StatsItemView key={c} id={c} />
  return <StatsView />
}

const RANGES: [RangeKind, string][] = [["today", "今天"], ["7d", "7 天"], ["30d", "30 天"], ["custom", "自定义…"]]
const PREV: Record<RangeKind, string> = { today: "昨天", "7d": "上个 7 天", "30d": "上个 30 天", custom: "上一段" }
const NAME: Record<RangeKind, string> = { today: "今天", "7d": "近 7 天", "30d": "近 30 天", custom: "所选日期" }
const localDay = (d = new Date()) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`
/** the filter of this page, kept while the app runs */
let saved: Filter = { range: "7d" }

function CustomRange({ f, set }: { f: Filter; set: (f: Filter) => void }) {
  const [open, setOpen] = useState(false)
  const today = localDay()
  const [from, setFrom] = useState(f.from ?? localDay(new Date(Date.now() - 13 * 86400000)))
  const [to, setTo] = useState(f.to ?? today)
  const bad = !from || !to || from > to || (Date.parse(to) - Date.parse(from)) / 86400000 + 1 > 366
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger render={<Button size="sm" variant="outline" className="font-normal" data-custom-range />}><CalendarIcon />{f.range === "custom" && f.from ? `${f.from} – ${f.to}` : "选日期"}</PopoverTrigger>
      <PopoverContent align="start" className="w-72">
        <div className="flex flex-col gap-3">
          <label className="flex flex-col gap-1 type-footnote text-muted-foreground">从<Input type="date" value={from} max={today} onChange={(e) => setFrom(e.target.value)} aria-label="开始日期" /></label>
          <label className="flex flex-col gap-1 type-footnote text-muted-foreground">到<Input type="date" value={to} max={today} onChange={(e) => setTo(e.target.value)} aria-label="结束日期" /></label>
          {bad && <div className="type-footnote text-muted-foreground">开始不能晚于结束，最长 366 天。</div>}
          <Button size="sm" disabled={bad} onClick={() => { set({ ...f, range: "custom", from, to }); setOpen(false) }} data-custom-apply>看这段时间</Button>
        </div>
      </PopoverContent>
    </Popover>
  )
}

export function StatsView() {
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
    const t = setTimeout(() => setTick((x) => x + 1), 900000 - (Date.now() % 900000) + 1500)
    return () => clearTimeout(t)
  }, [ticking, tick])
  const fkey = JSON.stringify(f) + (ticking ? "#" + tick : "")
  const [s] = useStats(() => stats.summary(f), [fkey], live)
  if (s.s === "absent") return <Unwired id="stats" />
  const bots = Object.keys(st.bots)
  const toolbar = (
    <div className="flex flex-wrap items-center gap-2" data-stats-toolbar>
      <Segmented size="sm" value={f.range === "custom" ? "custom" : f.range} onChange={(v) => { setPicked(null); setDrill(null); if (v !== "custom") setF({ ...f, range: v, from: undefined, to: undefined }); else setF({ ...f, range: "custom", from: f.from ?? localDay(new Date(Date.now() - 13 * 86400000)), to: f.to ?? localDay() }) }} options={RANGES} />
      {f.range === "custom" && <CustomRange f={f} set={(x) => { setPicked(null); setDrill(null); setF(x) }} />}
      <Select value={f.project ?? ""} onChange={(v) => setF({ ...f, project: v || undefined })} options={[{ v: "", l: "全部项目" }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} testId="stats-project" />
      <Select value={f.bot ?? ""} onChange={(v) => setF({ ...f, bot: v || undefined })} options={[{ v: "", l: "全部 Bot" }, ...bots.map((k) => ({ v: k, l: st.bots[k].n }))]} testId="stats-bot" />
      {s.s === "ok" && <span className="type-footnote text-muted-foreground" data-updated>更新于 {hhmm(s.at)}</span>}
    </div>
  )
  return (
    <Page title="统计" sub={s.s === "ok" ? `${NAME[f.range]} · ${fmtRange(s.v.range.from, s.v.range.to)}${oneDay(f) ? ` · 按小时，${zoneName(s.v.range.tz)}` : ""} · 费用为估算` : "费用为估算"} max={1040} toolbar={toolbar}>
      {s.s === "loading" ? <div className="flex justify-center py-24" role="status" aria-label="加载中"><Spinner className="size-5 text-muted-foreground" /></div>
        : s.s === "error" ? <Empty title="读不到统计" sub={s.msg} />
        : <StatsBody s={s.v} f={f} fkey={fkey} live={live} picked={picked} setPicked={setPicked} drill={drill} setDrill={setDrill} />}
    </Page>
  )
}

function StatsBody({ s, f, fkey, live, picked, setPicked, drill, setDrill }: { s: import("@/api/stats").Summary; f: Filter; fkey: string; live: boolean; picked: string | null; setPicked: (d: string | null) => void; drill: Drill | null; setDrill: (d: Drill | null) => void }) {
  const nothing = s.usage.total === 0 && s.usage.reported === 0 && s.usage.turns === 0 && s.throughput.created === 0 && s.throughput.done === 0 && s.human.decisions === 0 && s.throughput.wip === 0
  const since = s.since.tokens
  const range = fmtRange(s.range.from, s.range.to), rangeName = NAME[f.range]
  const [src] = useStats(() => stats.usage(f, "source"), [fkey], live)
  return (
    <div className="flex flex-col gap-14" data-stats>
      <div className="flex flex-col gap-5" data-overview>
        <RunState className="-mt-4 border-b pb-3" />
        <CoreKpis s={s} f={f} prevName={PREV[f.range]} rangeName={rangeName} drill={drill} setDrill={setDrill} />
        {drill && <DrillPanel key={drill} d={drill} f={f} fkey={fkey} live={live} rangeName={rangeName} close={() => setDrill(null)} />}
      </div>
      <Section k="items" title="事项" meta={`趋势按${oneDay(f) ? "小时" : "天"} · ${range}；状态分布是现在`}>
        <div className="grid grid-cols-1 gap-10 md:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
          <FlowChart s={s} f={f} fkey={fkey} live={live} drill={drill} setDrill={setDrill} />
          <StatusBars f={f} drill={drill} setDrill={setDrill} />
        </div>
      </Section>
      {nothing ? <Empty title="这段时间还没有统计" sub="Agent 跑过回合、事项有了进展，这里就会有数字。换一个时间段看看。" /> : <>
        <TimeSection s={s} f={f} fkey={fkey} live={live} range={range} />
        <Section k="usage" title="用量与费用" meta={range}>
          <UsageKpis s={s} prevName={PREV[f.range]} bySource={src.s === "ok" ? src.v.rows : undefined} />
          <PriceNote spend={s.spend} />
          <Trend f={f} fkey={fkey} live={live} range={[s.range.from, s.range.to]} tz={s.range.tz} picked={picked} onPick={setPicked} />
          <UsageSection f={f} fkey={fkey} live={live} picked={picked} />
        </Section>
        <QualityHuman s={s} f={f} fkey={fkey} live={live} range={range} prevName={PREV[f.range]} />
        <AgentSection s={s} f={f} fkey={fkey} live={live} range={range} />
        <Section k="item-table" title="事项明细" meta={`区间内完成或有活动的事项 · ${range}；点一行看这件事项的统计`}>
          <ItemsTable f={f} fkey={fkey} live={live} />
        </Section>
      </>}
      <footer className="border-t pt-4 type-footnote text-muted-foreground" data-stats-foot>
        费用 = tokens × 单价（先用价格表，表里没有的用 Models.dev），是估算，不是账单，实际以服务商账单为准；Kiro 和推理网关的实际计费未知。{since ? `token 明细从 ${since} 起记录` : "还没有 token 明细"}，之前只有会话级的适配器报价（下限{s.spend.by_source.backfill != null || s.usage.reported ? `，这段时间合计 ${fmtUsd(s.usage.reported)}` : ""}）。{s.since.gateway ? `网关调用从 ${s.since.gateway} 起记录。` : "网关调用从统计上线起记录。"}改价格表会重算历史估算；Models.dev 的价格变动从同步后的第二天起生效，已经统计的日子不变。
      </footer>
    </div>
  )
}

/** 费用下面一行：没有价格的模型（费用不完整，未计入的 tokens），以及单价来自哪里 */
function PriceNote({ spend }: { spend: Summary["spend"] }) {
  const unpriced = Object.keys(spend.unpriced)
  const by = spend.priced_by ?? {}, c = spend.catalog
  const md = by.models_dev ? `Models.dev${c?.synced ? `（${time(c.synced)} 同步）` : ""}` : ""
  const from = by.table && md ? `价格表和 ${md}` : by.table ? "价格表" : md ? ` ${md}` : ""
  if (!spend.unpriced_tokens && !from) return null
  return (
    <div className="-mt-3 flex flex-col gap-0.5 type-footnote text-muted-foreground" data-price-note>
      {spend.unpriced_tokens > 0 && (
        <div data-unpriced-note>
          <span className="text-attention">费用未完整估算</span>：{unpriced.length} 个模型没有价格（{unpriced.join("、") || "未知模型"}），{fmtN(spend.unpriced_tokens)} tokens 未计入。
        </div>
      )}
      <div data-price-source>
        {from && <>单价来自{from}{md && c?.error ? "，最近一次同步失败，用的是上次的价格" : ""}。</>}
        <a href="#/settings/stats" className="text-blue hover:underline">管理价格</a>
      </div>
    </div>
  )
}
