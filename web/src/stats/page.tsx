/* 统计页（stats.md §6）：#/stats 与下钻 #/stats/items/:id。只有 Rust Hub 有这些接口；
 * 拿不到（TS Hub）时显示“还没有接入”，不显示示例数据。费用处处标“估”。 */
import { useState } from "react"
import { CalendarIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { st } from "@/store"
import { Page, Segmented, Empty } from "@/ui"
import { Select } from "@/views"
import { Unwired } from "@/unwired"
import { stats, type Filter, type RangeKind } from "@/api/stats"
import { KpiRow } from "@/stats/kpi"
import { Trend } from "@/stats/trend"
import { AgentSection, FlowTime, QualityHuman, UsageSection } from "@/stats/tables"
import { StatsItemView } from "@/stats/drill"
import { fmtUsd, hhmm, useStats } from "@/stats/format"

export function StatsRoute({ b, c }: { b?: string; c?: string }) {
  if (b === "items" && c) return <StatsItemView key={c} id={c} />
  return <StatsView />
}

const RANGES: [RangeKind, string][] = [["today", "今天"], ["7d", "7 天"], ["30d", "30 天"], ["custom", "自定义…"]]
const PREV: Record<RangeKind, string> = { today: "昨天", "7d": "上个 7 天", "30d": "上个 30 天", custom: "上一段" }
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
  const fkey = JSON.stringify(f)
  /* the page follows live changes only while its range includes today */
  const live = f.range !== "custom" || (f.to ?? "") >= localDay()
  const [s] = useStats(() => stats.summary(f), [fkey], live)
  if (s.s === "absent") return <Unwired id="stats" />
  const bots = Object.keys(st.bots)
  const toolbar = (
    <div className="flex flex-wrap items-center gap-2" data-stats-toolbar>
      <Segmented size="sm" value={f.range === "custom" ? "custom" : f.range} onChange={(v) => { setPicked(null); if (v !== "custom") setF({ ...f, range: v, from: undefined, to: undefined }); else setF({ ...f, range: "custom", from: f.from ?? localDay(new Date(Date.now() - 13 * 86400000)), to: f.to ?? localDay() }) }} options={RANGES} />
      {f.range === "custom" && <CustomRange f={f} set={(x) => { setPicked(null); setF(x) }} />}
      <Select value={f.project ?? ""} onChange={(v) => setF({ ...f, project: v || undefined })} options={[{ v: "", l: "全部项目" }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} testId="stats-project" />
      <Select value={f.bot ?? ""} onChange={(v) => setF({ ...f, bot: v || undefined })} options={[{ v: "", l: "全部 Bot" }, ...bots.map((k) => ({ v: k, l: st.bots[k].n }))]} testId="stats-bot" />
      {s.s === "ok" && <span className="type-footnote text-muted-foreground" data-updated>更新于 {hhmm(s.at)}</span>}
    </div>
  )
  return (
    <Page title="统计" sub="费用为估算" max={1040} toolbar={toolbar}>
      {s.s === "loading" ? <div className="flex justify-center py-24" role="status" aria-label="加载中"><Spinner className="size-5 text-muted-foreground" /></div>
        : s.s === "error" ? <Empty title="读不到统计" sub={s.msg} />
        : <StatsBody s={s.v} f={f} fkey={fkey} live={live} picked={picked} setPicked={setPicked} />}
    </Page>
  )
}

function StatsBody({ s, f, fkey, live, picked, setPicked }: { s: import("@/api/stats").Summary; f: Filter; fkey: string; live: boolean; picked: string | null; setPicked: (d: string | null) => void }) {
  const nothing = s.usage.total === 0 && s.usage.reported === 0 && s.usage.turns === 0 && s.throughput.created === 0 && s.throughput.done === 0 && s.human.decisions === 0 && s.throughput.wip === 0
  const since = s.since.tokens
  const [src] = useStats(() => stats.usage(f, "source"), [fkey], live)
  return (
    <div className="flex flex-col gap-12" data-stats>
      <KpiRow s={s} prevName={PREV[f.range]} bySource={src.s === "ok" ? src.v.rows : undefined} />
      {s.spend.unpriced_tokens > 0 && (
        <div className="-mt-6 type-footnote text-muted-foreground" data-unpriced-note>
          另有 {Object.keys(s.spend.unpriced).length} 个模型没有价格（{Object.keys(s.spend.unpriced).join("、") || "未知模型"}），它们的 tokens 不计入估算。<a href="#/settings/stats" className="text-blue hover:underline">去设置价格</a>
        </div>
      )}
      {nothing ? <Empty title="这段时间还没有统计" sub="Agent 跑过回合、事项有了进展，这里就会有数字。换一个时间段看看。" /> : <>
        <Trend f={f} fkey={fkey} live={live} picked={picked} onPick={setPicked} />
        <UsageSection f={f} fkey={fkey} live={live} picked={picked} />
        <FlowTime s={s} f={f} fkey={fkey} live={live} />
        <QualityHuman s={s} f={f} fkey={fkey} live={live} />
        <AgentSection s={s} f={f} fkey={fkey} live={live} />
      </>}
      <footer className="border-t pt-4 type-footnote text-muted-foreground" data-stats-foot>
        费用 = tokens × 价格表，是估算，不是账单；Kiro 和推理网关的实际计费未知。{since ? `token 明细从 ${since} 起记录` : "还没有 token 明细"}，之前只有会话级的适配器报价（下限{s.spend.by_source.backfill != null || s.usage.reported ? `，这段时间合计 ${fmtUsd(s.usage.reported)}` : ""}）。{s.since.gateway ? `网关调用从 ${s.since.gateway} 起记录。` : "网关调用从统计上线起记录。"}改价格后，历史估算随之变化。
      </footer>
    </div>
  )
}
