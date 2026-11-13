/* 每天的产出（stats v2）：每天完成多少事项（柱，GET /stats/series metric=done），旁边一条线是那天的估算费用
 * （metric=cost）、Agent 回合数（metric=turns）或 Agent 时长（metric=turn_ms，回合按结束那天整段计入），各用右边自己的刻度。“今天”按小时（ST-43）。点一天（一小时），
 * 下面列出那天完成和新建的事项（overview.tsx 的下钻），和柱子同一个区间。 */
import { useState } from "react"
import { CalendarCheckIcon } from "lucide-react"
import { Panel } from "@/panel"
import { Segmented } from "@/ui"
import { stats, type Filter, type Series, type Summary } from "@/api/stats"
import { Chart, ChartBody, ChartHead, ChartTable, STATE, type Overlay } from "@/stats/chart"
import { DrillPanel, Loading, ModeSwitch, type Drill } from "@/stats/overview"
import { fmtMs, fmtN, fmtRange, fmtUsd, oneDay, zoneName, useStats } from "@/stats/format"
import { durUnit } from "@/i18n/format"
import { useT } from "@/i18n"

type LineKind = "cost" | "turns" | "turn_ms"
const LINE_COLOR = "var(--series-2)"

export function DailyChart({ s, f, fkey, live, drill, setDrill, rangeName }: { s: Summary; f: Filter; fkey: string; live: boolean; drill: Drill | null; setDrill: (d: Drill | null) => void; rangeName: string }) {
  const t = useT()
  const hourly = oneDay(f)
  const [line, setLine] = useState<LineKind>("cost")
  const [table, setTable] = useState(false)
  const [dn] = useStats(() => stats.series(f, "done", "none", hourly), [fkey], live)
  const [ln] = useStats(() => { const m = line; return stats.series(f, m, "none", hourly).then((v) => ({ ...v, m })) }, [fkey, line], live)
  /* null stays null: an hour not yet come is empty, not 0 */
  const val = (v: number | null | undefined) => (v === null ? null : v ?? 0)
  const data: Series | null = dn.s === "ok" ? { ...dn.v, series: [{ key: "done", label: t("stats:v2.daily.done"), values: dn.v.buckets.map((_, i) => val(dn.v.series[0]?.values[i])) }] } : null
  const lv = ln.s === "ok" && ln.v.m === line && data ? data.buckets.map((b) => { const j = ln.v.buckets.indexOf(b); return j < 0 ? 0 : val(ln.v.series[0]?.values[j]) }) : null
  /* agent time is drawn in the one unit its largest day needs (hours, so the axis has round steps: "4 小时"); the
   * tooltip and the table say it exactly ("4 小时 15 分") */
  const [unit, div] = durUnit(Math.max(0, ...(lv ?? []).map((v) => v ?? 0)))
  const dur = line === "turn_ms"
  const lfmt = line === "cost" ? (v: number) => (v === 0 ? "$0" : fmtUsd(v)) : dur ? (v: number) => (v === 0 ? "0" : `${fmtN(v)} ${unit}`) : (v: number) => fmtN(Math.round(v))
  const lineLabel = { cost: t("stats:v2.daily.cost"), turns: t("stats:v2.daily.turns"), turn_ms: t("stats:v2.daily.turnMs") }[line]
  const overlay: Overlay | undefined = lv ? { key: line, label: lineLabel, values: dur ? lv.map((v) => (v == null ? null : v / div)) : lv, color: LINE_COLOR, fmt: lfmt, ...(dur ? { tipFmt: (v: number) => fmtMs(v * div) } : {}) } : undefined
  const legendLine = line === "cost" ? t("stats:v2.daily.costLegend", { usd: fmtUsd(s.spend.est) }) : line === "turns" ? t("stats:v2.daily.turnsLegend", { n: fmtN(s.usage.turns) }) : t("stats:v2.daily.turnMsLegend", { dur: fmtMs(s.usage.turn_ms) })
  const picked = drill?.startsWith("day:") ? drill.slice(4) : null
  const pick = (day: string | null) => setDrill(day ? `day:${day}` : null)
  const span = fmtRange(s.range.from, s.range.to)
  const meta = hourly ? t("stats:v2.daily.metaHourly", { range: span, zone: zoneName(s.range.tz) }) : t("stats:v2.daily.metaDaily", { range: span })
  const err = dn.s === "error" ? dn.msg : null
  const empty = data && data.series[0].values.every((v) => !v) && (!lv || lv.every((v) => !v))
  const both: Series | null = data && lv ? { ...data, series: [...data.series, { key: line, label: overlay!.label, values: lv }] } : data
  return (
    <Panel k="daily" data-daily>
      <ChartHead k="daily" icon={<CalendarCheckIcon />} title={hourly ? t("stats:v2.daily.titleHourly") : t("stats:v2.daily.title")} meta={meta}
        legend={[{ label: t("stats:v2.daily.doneN", { count: s.throughput.done }), c: STATE.done }, ...(overlay ? [{ label: legendLine, c: LINE_COLOR, line: true }] : [])]}
        right={<>
          <Segmented size="sm" value={line} onChange={setLine} label={t("stats:v2.daily.lineAria")} options={[["cost", t("stats:v2.daily.cost")], ["turns", t("stats:v2.daily.turns")], ["turn_ms", t("stats:v2.daily.turnMs")]] as const} />
          <ModeSwitch table={table} set={setTable} />
        </>} />
      <ChartBody>
        {!data ? <Loading err={err} h={220} /> : table && both ? <ChartTable data={both} fmt={(v) => String(v)} fmts={[(v) => String(v), dur ? fmtMs : lfmt]} picked={picked} onPick={pick} height={220} />
          : <Chart data={data} kind="group" int fmt={(v) => String(Math.round(v))} colors={[STATE.done]} overlay={overlay} picked={picked} onPick={pick} label={hourly ? t("stats:v2.daily.ariaHourly") : t("stats:v2.daily.ariaDaily")} />}
        {empty && <div className="mt-2 type-footnote text-muted-foreground">{t("stats:v2.daily.empty")}</div>}
      </ChartBody>
      {drill?.startsWith("day:") && <DrillPanel key={drill} d={drill} f={f} fkey={fkey} live={live} rangeName={rangeName} close={() => setDrill(null)} />}
    </Panel>
  )
}
