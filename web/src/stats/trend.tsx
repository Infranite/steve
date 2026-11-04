/* 用量趋势（stats.md §6.3 第 3 条，ST-40 重整）：tokens / 费用 / 命中率，分组不分组 / token 类型 / 模型 / Bot / 项目 / 来源。
 * tokens、费用是按天堆叠柱，命中率是折线；分组的颜色按顺序取 SERIES，超过 5 组并入“其他”。点一天，下面的用量明细切到那一天。
 * 单日范围（今天）按小时（ST-43）：点一个小时，用量明细切到那个小时。
 * 完成数和耗时不在这里：它们在概览和“耗时”里，用状态色画。 */
import { useState } from "react"
import { Button } from "@/components/ui/button"
import { stats, type Filter, type Group, type Metric } from "@/api/stats"
import { Segmented } from "@/ui"
import { Select } from "@/view-shared"
import { Chart, ChartHead, ChartTable, fold, seriesColor, type Kind } from "@/stats/chart"
import { Loading, ModeSwitch } from "@/stats/overview"
import { bucketName, fmtN, fmtPct, fmtRange, fmtUsd, oneDay, zoneName, useStats } from "@/stats/format"

const TABS = [["tokens", "tokens"], ["cost", "费用"], ["hit_rate", "命中率"]] as const
type Tab = (typeof TABS)[number][0]
const KIND: Record<Tab, Kind> = { tokens: "stack", cost: "stack", hit_rate: "line" }
const FMT: Record<Tab, (v: number) => string> = { tokens: fmtN, cost: (v) => (v === 0 ? "$0" : fmtUsd(v)), hit_rate: (v) => fmtPct(v) }
const UNIT: Record<Tab, [string, string]> = { tokens: ["tokens / 天", "tokens / 小时"], cost: ["美元（估算）/ 天", "美元（估算）/ 小时"], hit_rate: ["缓存命中率，每天", "缓存命中率，每小时"] }
/** the groups each tab can use (the Hub refuses the rest) */
const GROUPS: Record<Tab, [Group, string][]> = {
  tokens: [["token_type", "按 token 类型"], ["none", "不分组"], ["model", "按模型"], ["bot", "按 Bot"], ["project", "按项目"], ["source", "按来源"]],
  cost: [["none", "不分组"], ["token_type", "按 token 类型"], ["model", "按模型"], ["bot", "按 Bot"], ["project", "按项目"], ["source", "按来源"]],
  hit_rate: [["none", "不分组"], ["model", "按模型"], ["bot", "按 Bot"], ["project", "按项目"], ["source", "按来源"]],
}
const TYPE_LABEL: Record<string, string> = { input: "输入", output: "输出", cache_read: "缓存读", cache_write: "缓存写" }

export function Trend({ f, fkey, live, range, tz, picked, onPick }: { f: Filter; fkey: string; live: boolean; range: [string, string]; tz?: string; picked: string | null; onPick: (d: string | null) => void }) {
  const hourly = oneDay(f)
  const [tab, setTab] = useState<Tab>("tokens")
  const [group, setGroup] = useState<Group>("token_type")
  const [table, setTable] = useState(false)
  const g = GROUPS[tab].some(([v]) => v === group) ? group : GROUPS[tab][0][0]
  const [data] = useStats(() => stats.series(f, tab as Metric, g, hourly), [fkey, tab, g], live)
  const fmt = FMT[tab], kind = KIND[tab]
  const raw = data.s === "ok" ? { ...data.v, series: data.v.series.map((l) => ({ ...l, label: TYPE_LABEL[l.key] && g === "token_type" ? TYPE_LABEL[l.key] : l.label || "（未知）" })) } : null
  /* a rate can't be summed into 其他: lines keep the first five and say how many are left out */
  const d = raw && (kind === "stack" ? fold(raw) : { ...raw, series: raw.series.slice(0, 5) })
  const cut = raw && kind !== "stack" ? raw.series.length - 5 : 0
  const empty = d && d.series.every((l) => l.values.every((v) => !v))
  const one = !!d && d.series.length === 1
  return (
    <section data-trend>
      <ChartHead k="usage" title={`${TABS.find((t) => t[0] === tab)![1]}趋势`} meta={`${UNIT[tab][hourly ? 1 : 0]} · ${fmtRange(range[0], range[1])}${hourly ? ` 00:00–24:00 · ${zoneName(tz)}` : ""}${picked ? ` · 已选 ${bucketName(picked)}` : ""}`}
        legend={d && !one ? d.series.map((l, i) => ({ label: l.label, c: seriesColor(i, l.key), line: kind === "line" })) : undefined}
        right={<>
          <Segmented size="sm" value={tab} onChange={(v) => { setTab(v); setGroup(GROUPS[v][0][0]) }} options={TABS} />
          {GROUPS[tab].length > 1 && <Select value={g} onChange={(v) => setGroup(v as Group)} options={GROUPS[tab].map(([v, l]) => ({ v, l }))} testId="trend-group" />}
          <ModeSwitch table={table} set={setTable} />
        </>} />
      {picked && <Button variant="ghost" size="xs" className="-mt-1 mb-2 text-blue" onClick={() => onPick(null)} data-picked>{bucketName(picked)} · {hourly ? "看全天" : "看全部日期"}</Button>}
      {data.s === "loading" ? <Loading h={220} />
        : data.s !== "ok" || !d ? <Loading h={220} err={data.s === "error" ? data.msg : "读不到趋势"} />
        : <>
          {table ? <ChartTable data={raw!} fmt={fmt} picked={picked} onPick={onPick} /> : <Chart data={d} kind={kind} fmt={fmt} picked={picked} onPick={onPick} label={TABS.find((t) => t[0] === tab)![1] + "趋势"} />}
          {cut > 0 && <div className="mt-2 type-footnote text-muted-foreground">另有 {cut} 组没画出来，切到“表格”或看下面的用量明细。</div>}
          {empty && <div className="mt-2 type-footnote text-muted-foreground">这段时间还没有{tab === "hit_rate" ? "上报缓存分项的调用" : "用量记录"}。</div>}
        </>}
    </section>
  )
}
