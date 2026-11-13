/* 指标数字（stats.md §6.3 第 2 条）：小标签、等宽数位的数字、一行灰色脚注“比上个 7 天 ↑ 18%”；悬停给定义。
 * 统计页顶上一行关键数字（stats v2，KeyNumbers）：完成事项、估算费用、Agent 工作时长、等你的时间、一次通过率、每件平均费用，
 * 每个都和上一个等长区间比；没有数据的不显示（一次通过率要有验收结论，每件费用要有完成的事项）。数字全取自 GET /stats/summary。 */
import type { ReactNode } from "react"
import { cn } from "cn"
import { STAT_CELL, STAT_VALUE, STRIP } from "@/panel"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import type { Summary } from "@/api/stats"
import { t, useT } from "@/i18n"
import { delta, durUnit, fmtMs, fmtPct, fmtUsd } from "@/stats/format"

/** “估”: the small grey badge every estimated dollar figure wears */
export function Est() {
  const t = useT()
  return <span className="inline-flex h-4 shrink-0 items-center rounded-md bg-muted px-1 type-label text-muted-foreground" title={t("stats:word.estTitle")} aria-label={t("stats:word.estAria")}>{t("stats:word.est")}</span>
}

/** a figure in a panel's strip (panel.tsx `StatStrip`, with its definition on hover): grey label, figure, grey foot */
export function Kpi({ label, value, foot, def, badge, k, tone }: { label: string; value: ReactNode; foot?: ReactNode; def: string; badge?: ReactNode; k: string; tone?: "bad" | "wait" | "ok" }) {
  return (
    <Tooltip>
      <TooltipTrigger render={<div tabIndex={0} className={cn(KPI_CELL, "outline-none hover:bg-muted/50 focus-visible:bg-muted/60")} data-kpi={k} />}>
        <div className="flex items-center gap-1.5 truncate type-footnote text-muted-foreground">{label}{badge}</div>
        <div className={cn(STAT_VALUE, tone === "bad" ? "text-destructive" : tone === "wait" ? "text-attention" : tone === "ok" ? "text-success" : "text-foreground")}>{value}</div>
        <div className="min-h-[18px] truncate type-footnote text-tertiary">{foot}</div>
      </TooltipTrigger>
      <TooltipContent className="max-w-64">{def}</TooltipContent>
    </Tooltip>
  )
}
/** a strip's cell and its row: space between cells (no rules), wrapping to two per row on a phone */
export const KPI_CELL = STAT_CELL
/*  The columns follow the panel's width, not the window's (the panel is an `@container`): a panel in a two-column row is
 *  narrow on a wide screen, and four durations side by side would be cut off there. */
export const kpiRow = (n: number) => cn(STRIP, "gap-y-1", n === 3 ? "grid-cols-3" : n === 6 ? "grid-cols-2 @lg:grid-cols-3 @4xl:grid-cols-6" : "grid-cols-2 @xl:grid-cols-4")

/** “比上个 7 天 ↑ 18%” */
export const versus = (s: Summary, prevName: string) => (now: number | null | undefined, before: number | null | undefined) => { const d = delta(now, before); return d ? t("stats:delta.vs", { prev: prevName, delta: d }) : before === 0 ? t("stats:delta.prevZero", { prev: prevName }) : s.prev ? t("stats:delta.prevNone", { prev: prevName }) : "" }

/** a duration in one unit, one decimal (“26.3 小时”): a key number has to fit its column; the exact one is in its tooltip */
const dur1 = (ms: number) => { const [unit, div] = durUnit(ms); const x = ms / div; return `${x >= 100 ? Math.round(x) : x.toFixed(1).replace(/\.0$/, "")} ${unit}` }
type Key = { k: string; label: string; value: ReactNode; foot: ReactNode; def: string; badge?: ReactNode }
/** the range's key numbers in one row, each against the range before */
export function KeyNumbers({ s, prevName }: { s: Summary; prevName: string }) {
  const t = useT()
  const p = s.prev, vs = versus(s, prevName)
  /* a cost known only from the adapter's own quote (no tokens counted) is a floor, not an estimate */
  const quoteOnly = s.usage.total === 0 && s.spend.reported > 0
  const xs: (Key | false)[] = [
    { k: "done", label: t("stats:v2.kpi.done"), value: s.throughput.done, foot: vs(s.throughput.done, p?.throughput.done), def: t("stats:v2.kpi.doneDef") },
    (s.spend.est > 0 || s.spend.reported > 0 || s.usage.total > 0) && { k: "est", label: t("stats:v2.kpi.cost"), badge: <Est />, value: quoteOnly ? "≥" + fmtUsd(s.spend.reported) : fmtUsd(s.spend.est),
      foot: quoteOnly ? t("stats:word.adapterQuote", { usd: fmtUsd(s.spend.reported) }) : vs(s.spend.est, p?.spend.est), def: t("stats:kpi.estDef") },
    s.usage.turns > 0 && { k: "agent", label: t("stats:v2.kpi.agent"), value: dur1(s.usage.turn_ms), foot: vs(s.usage.turn_ms, p?.usage.turn_ms) || t("stats:v2.kpi.agentFoot", { count: s.usage.turns }), def: t("stats:v2.kpi.exact", { dur: fmtMs(s.usage.turn_ms), what: t("stats:v2.kpi.agentFoot", { count: s.usage.turns }), def: t("stats:v2.kpi.agentDef") }) },
    s.time.wait.n > 0 && { k: "waiting", label: t("stats:v2.kpi.wait"), value: dur1(s.time.wait.sum), foot: vs(s.time.wait.sum, p?.time.wait.sum) || t("stats:v2.kpi.waitFoot", { count: s.time.wait.n }), def: t("stats:v2.kpi.exact", { dur: fmtMs(s.time.wait.sum), what: t("stats:v2.kpi.waitFoot", { count: s.time.wait.n }), def: t("stats:v2.kpi.waitDef") }) },
    s.quality.first_pass_n > 0 && { k: "first_pass", label: t("stats:v2.kpi.firstPass"), value: fmtPct(s.quality.first_pass), foot: vs(s.quality.first_pass, p && p.quality.first_pass_n ? p.quality.first_pass : null) || t("stats:v2.kpi.firstPassFoot", { count: s.quality.first_pass_n }), def: t("stats:v2.kpi.firstPassDef", { count: s.quality.first_pass_n }) },
    s.per_item.est_avg != null && s.per_item.est_avg > 0 && { k: "per_item", label: t("stats:v2.kpi.perItem"), badge: <Est />, value: fmtUsd(s.per_item.est_avg), foot: vs(s.per_item.est_avg, p?.per_item.est_avg) || (s.per_item.est_p50 != null ? t("stats:v2.kpi.perItemFoot", { usd: fmtUsd(s.per_item.est_p50) }) : ""), def: t("stats:v2.kpi.perItemDef") },
  ]
  const shown = xs.filter(Boolean) as Key[]
  return (
    <div className={cn("grid grid-cols-2 gap-x-2 gap-y-1 @2xl/stats:grid-cols-3", shown.length > 3 && (shown.length === 4 ? "@5xl/stats:grid-cols-4" : shown.length === 5 ? "@5xl/stats:grid-cols-5" : "@5xl/stats:grid-cols-6"))} data-key-numbers={shown.length}>
      {shown.map((x) => (
        <Tooltip key={x.k}>
          <TooltipTrigger render={<div tabIndex={0} className="min-w-0 rounded-lg px-3 py-3 outline-none hover:bg-muted/50 focus-visible:bg-muted/60" data-kpi={x.k} />}>
            <div className="flex items-center gap-1.5 truncate type-footnote text-muted-foreground">{x.label}{x.badge}</div>
            <div className="mt-1 truncate type-display tabular-nums text-foreground">{x.value}</div>
            <div className="min-h-[18px] truncate type-footnote text-tertiary">{x.foot}</div>
          </TooltipTrigger>
          <TooltipContent className="max-w-72">{x.def}</TooltipContent>
        </Tooltip>
      ))}
    </div>
  )
}
