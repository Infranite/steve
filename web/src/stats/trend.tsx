/* 用量趋势（stats.md §6.3 第 3 条，ST-40 重整）：tokens / 费用 / 命中率，分组不分组 / token 类型 / 模型 / Bot / 项目 / 来源。
 * tokens、费用是按天堆叠柱，命中率是折线；分组的颜色按顺序取 SERIES，超过 5 组并入“其他”。点一天，下面的用量明细切到那一天。
 * 单日范围（今天）按小时（ST-43）：点一个小时，用量明细切到那个小时。
 * 完成数和耗时不在这里：它们在概览和“耗时”里，用状态色画。 */
import { useState, type ReactNode } from "react"
import { CoinsIcon } from "lucide-react"
import { Panel } from "@/panel"
import { Button } from "@/components/ui/button"
import { stats, type Filter, type Group, type Metric } from "@/api/stats"
import { Segmented } from "@/ui"
import { Select } from "@/view-shared"
import { Chart, ChartBody, ChartHead, ChartTable, fold, seriesColor, type Kind } from "@/stats/chart"
import { Loading, ModeSwitch } from "@/stats/overview"
import { t, useT } from "@/i18n"
import { bucketName, fmtN, fmtPct, fmtRange, fmtUsd, oneDay, zoneName, useStats } from "@/stats/format"

type Tab = "tokens" | "cost" | "hit_rate"
const TABS = () => [["tokens", "tokens"], ["cost", t("stats:trend.tab.cost")], ["hit_rate", t("stats:trend.tab.hitRate")]] as const
/** “tokens 趋势”: the chart's title and its label */
const TITLE = (tab: Tab) => (tab === "tokens" ? t("stats:trend.title.tokens") : tab === "cost" ? t("stats:trend.title.cost") : t("stats:trend.title.hitRate"))
const KIND: Record<Tab, Kind> = { tokens: "stack", cost: "stack", hit_rate: "line" }
const FMT: Record<Tab, (v: number) => string> = { tokens: fmtN, cost: (v) => (v === 0 ? "$0" : fmtUsd(v)), hit_rate: (v) => fmtPct(v) }
const UNIT = (): Record<Tab, [string, string]> => ({ tokens: [t("stats:trend.unit.tokensDay"), t("stats:trend.unit.tokensHour")], cost: [t("stats:trend.unit.costDay"), t("stats:trend.unit.costHour")], hit_rate: [t("stats:trend.unit.hitRateDay"), t("stats:trend.unit.hitRateHour")] })
/** the groups each tab can use (the Hub refuses the rest) */
const GROUP_KEYS: Record<Tab, Group[]> = {
  tokens: ["token_type", "none", "model", "bot", "project", "source"],
  cost: ["none", "token_type", "model", "bot", "project", "source"],
  hit_rate: ["none", "model", "bot", "project", "source"],
}
const groupLabel = (g: Group) => (g === "token_type" ? t("stats:trend.group.tokenType") : g === "model" ? t("stats:trend.group.model") : g === "bot" ? t("stats:trend.group.bot") : g === "project" ? t("stats:trend.group.project") : g === "source" ? t("stats:trend.group.source") : t("stats:trend.group.none"))
const TYPE_LABEL = (): Record<string, string> => ({ input: t("stats:col.input"), output: t("stats:col.output"), cache_read: t("stats:col.cacheRead"), cache_write: t("stats:col.cacheWrite") })

/** the usage trend as a panel; `children` (its figures and notes) follow the chart */
export function Trend({ f, fkey, live, range, tz, picked, onPick, children }: { f: Filter; fkey: string; live: boolean; range: [string, string]; tz?: string; picked: string | null; onPick: (d: string | null) => void; children?: ReactNode }) {
  const t = useT()
  const hourly = oneDay(f)
  const [tab, setTab] = useState<Tab>("tokens")
  const [group, setGroup] = useState<Group>("token_type")
  const [table, setTable] = useState(false)
  const g = GROUP_KEYS[tab].includes(group) ? group : GROUP_KEYS[tab][0]
  const [data] = useStats(() => stats.series(f, tab as Metric, g, hourly), [fkey, tab, g], live)
  const fmt = FMT[tab], kind = KIND[tab], types = TYPE_LABEL()
  const raw = data.s === "ok" ? { ...data.v, series: data.v.series.map((l) => ({ ...l, label: types[l.key] && g === "token_type" ? types[l.key] : l.label || t("stats:word.unknown") })) } : null
  /* a rate can't be summed into 其他: lines keep the first five and say how many are left out */
  const d = raw && (kind === "stack" ? fold(raw) : { ...raw, series: raw.series.slice(0, 5) })
  const cut = raw && kind !== "stack" ? raw.series.length - 5 : 0
  const empty = d && d.series.every((l) => l.values.every((v) => !v))
  const one = !!d && d.series.length === 1
  const unit = UNIT()[tab][hourly ? 1 : 0], span = fmtRange(range[0], range[1])
  const meta = hourly ? t("stats:trend.metaHourly", { unit, range: span, zone: zoneName(tz) }) : t("stats:trend.meta", { unit, range: span })
  return (
    <Panel k="usage" data-trend>
      <ChartHead k="usage" icon={<CoinsIcon />} title={TITLE(tab)} sub={t("stats:panel.usageSub")} meta={picked ? t("stats:trend.metaPicked", { meta, bucket: bucketName(picked) }) : meta}
        legend={d && !one ? d.series.map((l, i) => ({ label: l.label, c: seriesColor(i, l.key), line: kind === "line" })) : undefined}
        right={<>
          <Segmented size="sm" value={tab} onChange={(v) => { setTab(v); setGroup(GROUP_KEYS[v][0]) }} options={TABS()} />
          {GROUP_KEYS[tab].length > 1 && <Select value={g} onChange={(v) => setGroup(v as Group)} options={GROUP_KEYS[tab].map((v) => ({ v, l: groupLabel(v) }))} testId="trend-group" />}
          <ModeSwitch table={table} set={setTable} />
        </>} />
      <ChartBody>
      {picked && <Button variant="ghost" size="xs" className="mb-2 text-blue" onClick={() => onPick(null)} data-picked>{hourly ? t("stats:trend.pickedDay", { bucket: bucketName(picked) }) : t("stats:trend.pickedAll", { bucket: bucketName(picked) })}</Button>}
      {data.s === "loading" ? <Loading h={220} />
        : data.s !== "ok" || !d ? <Loading h={220} err={data.s === "error" ? data.msg : t("stats:trend.loadFailed")} />
        : <>
          {table ? <ChartTable data={raw!} fmt={fmt} picked={picked} onPick={onPick} /> : <Chart data={d} kind={kind} fmt={fmt} picked={picked} onPick={onPick} label={TITLE(tab)} />}
          {cut > 0 && <div className="mt-2 type-footnote text-muted-foreground">{t("stats:trend.cut", { count: cut })}</div>}
          {empty && <div className="mt-2 type-footnote text-muted-foreground">{tab === "hit_rate" ? t("stats:trend.emptyHit") : t("stats:trend.emptyUsage")}</div>}
        </>}
      </ChartBody>
      {children}
    </Panel>
  )
}
