/* 趋势（stats.md §6.3 第 3 条）：tokens / 费用 / 完成 / 前置时间 / 命中率，分组不分组 / 模型 / Bot / 项目 / 来源。
 * tokens 默认按 token 类型堆叠；完成是柱；前置时间是中位数折线带 p85 浅带。点一天，下面的用量明细切到那一天。 */
import { useState } from "react"
import { TableIcon, ChartColumnIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { stats, type Filter, type Group, type Metric } from "@/api/stats"
import { Segmented, SectionLabel } from "@/ui"
import { Select } from "@/view-shared"
import { Chart, ChartTable, Swatch, type Kind } from "@/stats/chart"
import { fmtMs, fmtN, fmtPct, fmtUsd, useStats } from "@/stats/format"

const TABS = [["tokens", "tokens"], ["cost", "费用"], ["done", "完成"], ["lead", "前置时间"], ["hit_rate", "命中率"]] as const
type Tab = (typeof TABS)[number][0]
const KIND: Record<Tab, Kind> = { tokens: "stack", cost: "stack", done: "stack", lead: "band", hit_rate: "line" }
const FMT: Record<Tab, (v: number) => string> = { tokens: fmtN, cost: (v) => (v === 0 ? "$0" : fmtUsd(v)), done: (v) => String(Math.round(v)), lead: fmtMs, hit_rate: (v) => fmtPct(v) }
/** the groups each tab can use (the Hub refuses the rest) */
const GROUPS: Record<Tab, [Group, string][]> = {
  tokens: [["token_type", "按 token 类型"], ["none", "不分组"], ["model", "模型"], ["bot", "Bot"], ["project", "项目"], ["source", "来源"]],
  cost: [["none", "不分组"], ["token_type", "按 token 类型"], ["model", "模型"], ["bot", "Bot"], ["project", "项目"], ["source", "来源"]],
  done: [["none", "不分组"], ["bot", "Bot"], ["project", "项目"]],
  lead: [["none", "不分组"]],
  hit_rate: [["none", "不分组"], ["model", "模型"], ["bot", "Bot"], ["project", "项目"], ["source", "来源"]],
}
const TYPE_LABEL: Record<string, string> = { input: "输入", output: "输出", cache_read: "缓存读", cache_write: "缓存写" }

export function Trend({ f, fkey, live, picked, onPick }: { f: Filter; fkey: string; live: boolean; picked: string | null; onPick: (d: string | null) => void }) {
  const [tab, setTab] = useState<Tab>("tokens")
  const [group, setGroup] = useState<Group>("token_type")
  const [table, setTable] = useState(false)
  const g = GROUPS[tab].some(([v]) => v === group) ? group : GROUPS[tab][0][0]
  const [data] = useStats(() => stats.series(f, tab as Metric, g), [fkey, tab, g], live)
  const fmt = FMT[tab]
  const d = data.s === "ok" ? { ...data.v, series: data.v.series.map((l) => ({ ...l, label: TYPE_LABEL[l.key] && g === "token_type" ? TYPE_LABEL[l.key] : l.label })) } : null
  const empty = d && d.series.every((l) => l.values.every((v) => !v))
  return (
    <section data-trend>
      <SectionLabel right={<Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => setTable(!table)} aria-pressed={table} data-chart-mode>{table ? <><ChartColumnIcon />图表</> : <><TableIcon />表格</>}</Button>}>趋势</SectionLabel>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <Segmented size="sm" value={tab} onChange={(v) => { setTab(v); setGroup(GROUPS[v][0][0]) }} options={TABS} />
        {GROUPS[tab].length > 1 && <Select value={g} onChange={(v) => setGroup(v as Group)} options={GROUPS[tab].map(([v, l]) => ({ v, l }))} testId="trend-group" />}
        {picked && <Button variant="ghost" size="xs" className="text-blue" onClick={() => onPick(null)} data-picked>{picked} · 看全部日期</Button>}
      </div>
      {data.s === "loading" ? <div className="flex h-[220px] items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
        : data.s !== "ok" || !d ? <div className="flex h-[220px] items-center justify-center type-callout text-muted-foreground" role="alert">{data.s === "error" ? data.msg : "读不到趋势"}</div>
        : <>
          {table ? <ChartTable data={d} fmt={fmt} picked={picked} onPick={onPick} /> : <Chart data={d} kind={KIND[tab]} fmt={fmt} picked={picked} onPick={onPick} label={TABS.find((t) => t[0] === tab)![1] + "趋势"} />}
          {(d.series.length > 1 || KIND[tab] === "band") && (
            <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 type-footnote text-muted-foreground" data-legend>
              {KIND[tab] === "band" ? <><span className="inline-flex items-center gap-1.5"><Swatch i={0} line />中位数</span><span className="inline-flex items-center gap-1.5"><span className="inline-block h-2.5 w-3 rounded-sm bg-foreground/10" />中位数到 p85</span></>
                : d.series.map((l, i) => <span key={l.key} className="inline-flex items-center gap-1.5"><Swatch i={i} line={KIND[tab] === "line"} />{l.label}</span>)}
            </div>
          )}
          {empty && <div className="mt-2 type-footnote text-muted-foreground">这段时间还没有{tab === "lead" ? "完成的事项" : tab === "done" ? "完成的事项" : tab === "hit_rate" ? "上报缓存分项的调用" : "记录"}。</div>}
        </>}
    </section>
  )
}
