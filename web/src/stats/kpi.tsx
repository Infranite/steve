/* 指标数字（stats.md §6.3 第 2 条）：小标签、等宽数位的数字、一行灰色脚注“比上个 7 天 ↑ 18%”；悬停给定义。放在面板的数字条里（panel.tsx）。
 * ST-40 起页面顶部只放核心指标（overview.tsx）；这里是“用量与费用”一节的三个：估算费用 · 总 tokens · 缓存命中率。 */
import type { ReactNode } from "react"
import { cn } from "cn"
import { STAT_CELL, STAT_VALUE, STRIP } from "@/panel"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import type { Summary, UsageRow } from "@/api/stats"
import { t, useT } from "@/i18n"
import { delta, fmtN, fmtPct, fmtUsd } from "@/stats/format"

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

/** “Agent 62% · 网关 18%”: the total hides that a few large gateway calls can sink it */
function HitSplit({ rows }: { rows: UsageRow[] }) {
  const t = useT()
  const part = (key: string, label: string) => { const r = rows.find((x) => x.key === key); return r && r.hit_rate != null ? <span data-hit-source={key}>{t("stats:kpi.hitPart", { label, pct: fmtPct(r.hit_rate) })}</span> : null }
  const a = part("agent", "Agent"), g = part("gateway", t("stats:word.gateway"))
  if (!a && !g) return null
  return <>{a}{a && g && " · "}{g}</>
}

/** “比上个 7 天 ↑ 18%” */
export const versus = (s: Summary, prevName: string) => (now: number | null | undefined, before: number | null | undefined) => { const d = delta(now, before); return d ? t("stats:delta.vs", { prev: prevName, delta: d }) : before === 0 ? t("stats:delta.prevZero", { prev: prevName }) : s.prev ? t("stats:delta.prevNone", { prev: prevName }) : "" }

export function UsageKpis({ s, prevName, bySource }: { s: Summary; prevName: string; bySource?: UsageRow[] }) {
  const t = useT()
  const p = s.prev, vs = versus(s, prevName)
  return (
    <div className={kpiRow(3)} data-kpis>
      <Kpi k="est" label={t("stats:col.cost")} badge={<Est />} value={s.usage.total === 0 && s.spend.reported > 0 ? "—" : fmtUsd(s.spend.est)}
        foot={s.usage.total === 0 && s.spend.reported > 0 ? t("stats:word.adapterQuoteFloor", { usd: fmtUsd(s.spend.reported) }) : vs(s.spend.est, p?.spend.est)}
        def={t("stats:kpi.estDef")} />
      <Kpi k="tokens" label={t("stats:kpi.totalTokens")} value={fmtN(s.usage.total)} foot={vs(s.usage.total, p?.usage.total)} def={t("stats:kpi.tokensDef")} />
      <Kpi k="hit" label={t("stats:kpi.hitRate")} value={fmtPct(s.usage.hit_rate)}
        foot={s.usage.hit_rate == null ? t("stats:kpi.noCacheSplit") : (bySource && bySource.some((r) => r.hit_rate != null) ? <HitSplit rows={bySource} /> : vs(s.usage.hit_rate, p?.usage.hit_rate))}
        def={s.usage.hit_rate != null && vs(s.usage.hit_rate, p?.usage.hit_rate) ? t("stats:kpi.hitDefTotal", { vs: vs(s.usage.hit_rate, p?.usage.hit_rate) }) : t("stats:kpi.hitDef")} />
    </div>
  )
}
