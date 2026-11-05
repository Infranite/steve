/* 指标数字（stats.md §6.3 第 2 条）：12px 标签、数字、一行灰色脚注“比上个 7 天 ↑ 18%”；悬停给定义。不加卡片框。
 * ST-40 起页面顶部只放核心指标（overview.tsx）；这里是“用量与费用”一节的三个：估算费用 · 总 tokens · 缓存命中率。 */
import type { ReactNode } from "react"
import { cn } from "cn"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import type { Summary, UsageRow } from "@/api/stats"
import { t, useT } from "@/i18n"
import { delta, fmtN, fmtPct, fmtUsd } from "@/stats/format"

/** “估”: the grey outlined badge every estimated dollar figure wears */
export function Est() {
  const t = useT()
  return <span className="inline-flex h-4 shrink-0 items-center rounded border border-border px-1 text-[10.5px] font-medium leading-none text-muted-foreground" title={t("stats:word.estTitle")} aria-label={t("stats:word.estAria")}>{t("stats:word.est")}</span>
}

export function Kpi({ label, value, foot, def, badge, k, small }: { label: string; value: ReactNode; foot?: ReactNode; def: string; badge?: ReactNode; k: string; small?: boolean }) {
  return (
    <Tooltip>
      <TooltipTrigger render={<div tabIndex={0} className="min-w-0 rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring" data-kpi={k} />}>
        <div className="flex items-center gap-1.5 type-label text-muted-foreground">{label}{badge}</div>
        <div className={cn("mt-1 truncate font-semibold tracking-[-0.02em] text-foreground", small ? "text-[20px] leading-7" : "text-[26px] leading-8")}>{value}</div>
        <div className="mt-0.5 min-h-4 truncate type-footnote text-muted-foreground">{foot}</div>
      </TooltipTrigger>
      <TooltipContent className="max-w-64">{def}</TooltipContent>
    </Tooltip>
  )
}

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
    <div className="grid grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-3" data-kpis>
      <Kpi small k="est" label={t("stats:word.estCost")} badge={<Est />} value={s.usage.total === 0 && s.spend.reported > 0 ? "—" : fmtUsd(s.spend.est)}
        foot={s.usage.total === 0 && s.spend.reported > 0 ? t("stats:word.adapterQuoteFloor", { usd: fmtUsd(s.spend.reported) }) : vs(s.spend.est, p?.spend.est)}
        def={t("stats:kpi.estDef")} />
      <Kpi small k="tokens" label={t("stats:kpi.totalTokens")} value={fmtN(s.usage.total)} foot={vs(s.usage.total, p?.usage.total)} def={t("stats:kpi.tokensDef")} />
      <Kpi small k="hit" label={t("stats:kpi.hitRate")} value={fmtPct(s.usage.hit_rate)}
        foot={s.usage.hit_rate == null ? t("stats:kpi.noCacheSplit") : (bySource && bySource.some((r) => r.hit_rate != null) ? <HitSplit rows={bySource} /> : vs(s.usage.hit_rate, p?.usage.hit_rate))}
        def={s.usage.hit_rate != null && vs(s.usage.hit_rate, p?.usage.hit_rate) ? t("stats:kpi.hitDefTotal", { vs: vs(s.usage.hit_rate, p?.usage.hit_rate) }) : t("stats:kpi.hitDef")} />
    </div>
  )
}
