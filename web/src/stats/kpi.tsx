/* KPI 一行（stats.md §6.3 第 2 条）：估算费用 · 总 tokens · 缓存命中率 · 完成事项 · 平均完成时间 · 首次通过率。
 * 12px 标签、26/600 数字、一行灰色脚注“比上个 7 天 ↑ 18%”；悬停给定义。不加卡片框。 */
import type { ReactNode } from "react"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import type { Summary, UsageRow } from "@/api/stats"
import { delta, fmtMs, fmtN, fmtPct, fmtUsd } from "@/stats/format"

/** “估”: the grey outlined badge every estimated dollar figure wears */
export const Est = () => <span className="inline-flex h-4 shrink-0 items-center rounded border border-border px-1 text-[10.5px] font-medium leading-none text-muted-foreground" title="估算：tokens × 价格表，不是账单" aria-label="估算">估</span>

export function Kpi({ label, value, foot, def, badge, k }: { label: string; value: ReactNode; foot?: ReactNode; def: string; badge?: ReactNode; k: string }) {
  return (
    <Tooltip>
      <TooltipTrigger render={<div tabIndex={0} className="min-w-0 rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring" data-kpi={k} />}>
        <div className="flex items-center gap-1.5 type-label text-muted-foreground">{label}{badge}</div>
        <div className="mt-1 truncate text-[26px] font-semibold leading-8 tracking-[-0.02em] text-foreground">{value}</div>
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

export function KpiRow({ s, prevName, bySource }: { s: Summary; prevName: string; bySource?: UsageRow[] }) {
  const p = s.prev
  const vs = (now: number | null | undefined, before: number | null | undefined) => { const d = delta(now, before); return d ? `比${prevName} ${d}` : before === 0 ? `${prevName}为 0` : p ? `${prevName}没有数据` : "" }
  return (
    <div className="grid grid-cols-2 gap-x-6 gap-y-6 sm:grid-cols-3 lg:grid-cols-6" data-kpis>
      <Kpi k="est" label="估算费用" badge={<Est />} value={s.usage.total === 0 && s.spend.reported > 0 ? "—" : fmtUsd(s.spend.est)}
        foot={s.usage.total === 0 && s.spend.reported > 0 ? `适配器报价 ≥${fmtUsd(s.spend.reported)}` : vs(s.spend.est, p?.spend.est)}
        def="Σ 按模型（输入×输入价 + 输出×输出价 + 缓存读×缓存读价 + 缓存写×缓存写价）÷ 100 万；查不到价格的模型不计入，另列“未定价”。" />
      <Kpi k="tokens" label="总 tokens" value={fmtN(s.usage.total)} foot={vs(s.usage.total, p?.usage.total)} def="输入 + 输出 + 缓存读 + 缓存写。输入不含缓存，输出含思考。" />
      <Kpi k="hit" label="缓存命中率" value={fmtPct(s.usage.hit_rate)}
        foot={s.usage.hit_rate == null ? "没有上报缓存分项" : (bySource && bySource.some((r) => r.hit_rate != null) ? <HitSplit rows={bySource} /> : vs(s.usage.hit_rate, p?.usage.hit_rate))}
        def={`Σ 缓存读 ÷ Σ（输入 + 缓存读 + 缓存写），只算上报了缓存分项的调用。下面分开列 Agent 回合和网关调用：几次大的网关调用就能把总数拉低。${s.usage.hit_rate != null && vs(s.usage.hit_rate, p?.usage.hit_rate) ? `总数${vs(s.usage.hit_rate, p?.usage.hit_rate)}。` : ""}`} />
      <Kpi k="done" label="完成事项" value={s.throughput.done} foot={vs(s.throughput.done, p?.throughput.done)} def="区间内状态变成“完成”的事项数；同一件事项在一个区间里只算一次。" />
      <Kpi k="lead" label="平均完成时间" value={fmtMs(s.time.lead.avg)} foot={s.time.lead.p50 != null ? `中位 ${fmtMs(s.time.lead.p50)}` : "还没有完成的事项"}
        def="区间内完成的事项，从新建到完成的时间（前置时间）的平均数；旁边是中位数，长尾事项会把平均拉偏。" />
      <Kpi k="first_pass" label="首次通过率" value={fmtPct(s.quality.first_pass)} foot={s.quality.first_pass_n ? `${s.quality.first_pass_n} 件已裁决` : "还没有裁决的交付"}
        def="第一次交付就验收通过（人工通过或自动验收）的事项比例；被新交付取代的那次不算裁决。" />
    </div>
  )
}
