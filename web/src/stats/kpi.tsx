/* 指标数字（stats.md §6.3 第 2 条）：12px 标签、数字、一行灰色脚注“比上个 7 天 ↑ 18%”；悬停给定义。不加卡片框。
 * ST-40 起页面顶部只放核心指标（overview.tsx）；这里是“用量与费用”一节的三个：估算费用 · 总 tokens · 缓存命中率。 */
import type { ReactNode } from "react"
import { cn } from "cn"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import type { Summary, UsageRow } from "@/api/stats"
import { delta, fmtN, fmtPct, fmtUsd } from "@/stats/format"

/** “估”: the grey outlined badge every estimated dollar figure wears */
export const Est = () => <span className="inline-flex h-4 shrink-0 items-center rounded border border-border px-1 text-[10.5px] font-medium leading-none text-muted-foreground" title="估算：tokens × 价格表，不是账单" aria-label="估算">估</span>

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
  const part = (key: string, label: string) => { const r = rows.find((x) => x.key === key); return r && r.hit_rate != null ? <span data-hit-source={key}>{label} {fmtPct(r.hit_rate)}</span> : null }
  const a = part("agent", "Agent"), g = part("gateway", "网关")
  if (!a && !g) return null
  return <>{a}{a && g && " · "}{g}</>
}

/** “比上个 7 天 ↑ 18%” */
export const versus = (s: Summary, prevName: string) => (now: number | null | undefined, before: number | null | undefined) => { const d = delta(now, before); return d ? `比${prevName} ${d}` : before === 0 ? `${prevName}为 0` : s.prev ? `${prevName}没有数据` : "" }

export function UsageKpis({ s, prevName, bySource }: { s: Summary; prevName: string; bySource?: UsageRow[] }) {
  const p = s.prev, vs = versus(s, prevName)
  return (
    <div className="grid grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-3" data-kpis>
      <Kpi small k="est" label="估算费用" badge={<Est />} value={s.usage.total === 0 && s.spend.reported > 0 ? "—" : fmtUsd(s.spend.est)}
        foot={s.usage.total === 0 && s.spend.reported > 0 ? `适配器报价 ≥${fmtUsd(s.spend.reported)}` : vs(s.spend.est, p?.spend.est)}
        def="Σ 按模型（输入×输入价 + 输出×输出价 + 缓存读×缓存读价 + 缓存写×缓存写价）÷ 100 万；查不到价格的模型不计入，另列“未定价”。" />
      <Kpi small k="tokens" label="总 tokens" value={fmtN(s.usage.total)} foot={vs(s.usage.total, p?.usage.total)} def="输入 + 输出 + 缓存读 + 缓存写。输入不含缓存，输出含思考。" />
      <Kpi small k="hit" label="缓存命中率" value={fmtPct(s.usage.hit_rate)}
        foot={s.usage.hit_rate == null ? "没有上报缓存分项" : (bySource && bySource.some((r) => r.hit_rate != null) ? <HitSplit rows={bySource} /> : vs(s.usage.hit_rate, p?.usage.hit_rate))}
        def={`Σ 缓存读 ÷ Σ（输入 + 缓存读 + 缓存写），只算上报了缓存分项的调用。下面分开列 Agent 回合和网关调用：几次大的网关调用就能把总数拉低。${s.usage.hit_rate != null && vs(s.usage.hit_rate, p?.usage.hit_rate) ? `总数${vs(s.usage.hit_rate, p?.usage.hit_rate)}。` : ""}`} />
    </div>
  )
}
