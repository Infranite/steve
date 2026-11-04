/* 单件事项的下钻（stats.md §6.4）：KPI 一行，一条时间轴（建 → 回合 → 决定 → 交付 → 验收 → 发布），
 * 回合表和按模型的用量表。事项页的“用量 / 用时”两行也从这里取数（brief）。 */
import { useEffect, useRef, useState } from "react"
import { cn } from "cn"
import { ChevronRightIcon } from "lucide-react"
import { Spinner } from "@/components/ui/spinner"
import { stats, type ItemDetail } from "@/api/stats"
import { st } from "@/store"
import { go, Page, SectionLabel, Empty, ItemNo, Meta } from "@/ui"
import { Unwired } from "@/unwired"
import { Kpi, Est } from "@/stats/kpi"
import { Table, num, th } from "@/stats/tables"
import { fmtMs, fmtN, fmtPct, fmtUsd, time, useStats } from "@/stats/format"

const STOP: Record<string, string> = { end_turn: "正常结束", cancelled: "你中断", max_tokens: "超出长度", refusal: "拒绝", error: "出错", interrupted: "Hub 重启中断" }
const DECISION: Record<string, string> = { accept: "验收", ask: "提问", approve: "批准", fail: "失败卡", propose: "提议", confirm: "确认" }
const CHOICE: Record<string, string> = { pass: "通过", rework: "返工" }
const RELEASE: Record<string, string> = { released: "已发布", failed: "发布失败", not_released: "未发布", rolled_back: "已回滚", unknown: "结果不明" }

export function StatsItemView({ id }: { id: string }) {
  const [d] = useStats(() => stats.item(id), [id])
  if (d.s === "absent") return <Unwired id="stats" />
  const title = d.s === "ok" ? d.v.title : st.items.find((i) => i.id === id)?.t ?? id
  return (
    <Page title={<span className="inline-flex min-w-0 items-baseline gap-2"><ItemNo id={id} className="text-[15px]" /><span className="truncate">{title}</span></span>} back={["#/stats", "统计"]} max={1040}
      sub={<a href={"#/items/" + id} className="inline-flex items-center gap-0.5 text-blue hover:underline" data-item-link>打开事项<ChevronRightIcon className="size-3.5" /></a>}>
      {d.s === "loading" ? <div className="flex justify-center py-24"><Spinner className="size-5 text-muted-foreground" /></div>
        : d.s === "error" ? <Empty title="读不到这件事项的统计" sub={d.msg} />
        : <Drill d={d.v} />}
    </Page>
  )
}

function Drill({ d }: { d: ItemDetail }) {
  const f = d.facts, u = d.usage
  const turns = d.turns ?? []
  return (
    <div className="flex flex-col gap-12" data-drill>
      <div className="grid grid-cols-2 gap-6 sm:grid-cols-3 lg:grid-cols-6">
        <Kpi k="tokens" label="tokens" value={fmtN(u.total)} foot={u.hit_rate != null ? `缓存命中 ${fmtPct(u.hit_rate)}` : d.backfill_only ? "只有会话级报价" : ""} def="这件事项全生命周期的输入 + 输出 + 缓存读 + 缓存写。" />
        <Kpi k="est" label="估算费用" badge={<Est />} value={u.total ? fmtUsd(u.est) : u.reported ? "≥" + fmtUsd(u.reported) : "—"} foot={u.unpriced_tokens ? `不完整：${fmtN(u.unpriced_tokens)} tokens 未定价` : u.reported ? `适配器报价 ${fmtUsd(u.reported)}` : ""} def="tokens × 单价（先价格表，再 Models.dev），是估算；未定价的 tokens 不计入；回填的事项只有会话级的适配器报价（下限）。" />
        <Kpi k="lead" label="前置" value={fmtMs(f.lead_ms ?? d.age_ms)} foot={f.lead_ms != null ? "新建到完成" : "还没完成，已经过去"} def="从新建到完成的时间；没完成时是到现在为止。" />
        <Kpi k="agent" label="Agent 工作" value={fmtMs(f.agent_ms)} foot={`${f.turns} 个回合`} def="这件事项所有回合时间的并集。" />
        <Kpi k="wait" label="等你" value={fmtMs(f.wait_ms)} foot={`${f.decisions} 个决定`} def="需要你处理的卡片挂着的时间的并集（YOLO 代办的不算）。" />
        <Kpi k="reworks" label="返工" value={f.reworks} foot={f.first_pass == null ? "还没有裁决" : f.first_pass ? "第一次交付就通过" : "第一次交付没通过"} def="验收时选“返工”的次数。" />
      </div>
      <Timeline d={d} />
      <section>
        <SectionLabel>回合</SectionLabel>
        {!turns.length ? <Empty title="这件事项还没有回合" /> : (
          <Table label="回合" min={820} head={<tr><th className={th}>开始</th><th className={cn(th, "pl-4 text-right")}>时长</th><th className={cn(th, "pl-4")}>停止原因</th><th className={cn(th, "pl-4 text-right")}>工具</th><th className={cn(th, "pl-4 text-right")}>失败</th><th className={cn(th, "pl-4 text-right")}>输入</th><th className={cn(th, "pl-4 text-right")}>输出</th><th className={cn(th, "pl-4 text-right")}>缓存读</th><th className={cn(th, "pl-4 text-right")}>缓存写</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">估算<Est /></span></th></tr>}>
            {turns.map((t) => (
              <tr key={t.id} className="cursor-pointer hover:bg-accent" onClick={() => go("#/sessions/" + t.session_id)} data-turn={t.id}>
                <td className="py-2 tabular-nums">{time(t.started)}{t.attempt > 0 && <span className="ml-1 type-footnote text-muted-foreground">重试 {t.attempt}</span>}</td>
                <td className={num}>{t.ended ? fmtMs(t.ended - t.started) : "进行中"}</td>
                <td className="py-2 pl-4"><span className="inline-flex items-center gap-1.5">{(t.stop_reason === "error" || t.transient) && <span aria-hidden className="size-2 rounded-full bg-destructive" />}{t.transient ? "网关暂时不可用" : STOP[t.stop_reason ?? ""] ?? t.stop_reason ?? "—"}</span></td>
                <td className={num}>{t.tools}</td><td className={num}>{t.tool_errors || "—"}</td>
                <td className={num}>{fmtN(t.usage.input)}</td><td className={num}>{fmtN(t.usage.output)}</td><td className={num}>{fmtN(t.usage.cache_read)}</td><td className={num}>{fmtN(t.usage.cache_write)}</td>
                <td className={num}>{t.usage.total ? fmtUsd(t.usage.est) : "—"}</td>
              </tr>
            ))}
          </Table>
        )}
      </section>
      {!!d.by_model?.length && (
        <section>
          <SectionLabel>按模型</SectionLabel>
          <Table label="按模型" min={640} head={<tr><th className={th}>模型</th><th className={cn(th, "pl-4 text-right")}>输入</th><th className={cn(th, "pl-4 text-right")}>输出</th><th className={cn(th, "pl-4 text-right")}>缓存读</th><th className={cn(th, "pl-4 text-right")}>缓存写</th><th className={cn(th, "pl-4 text-right")}>命中率</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">估算费用<Est /></span></th></tr>}>
            {d.by_model.map((m) => (
              <tr key={m.model} data-model={m.model}><td className="py-2">{m.label}</td><td className={num}>{fmtN(m.input)}</td><td className={num}>{fmtN(m.output)}</td><td className={num}>{fmtN(m.cache_read)}</td><td className={num}>{fmtN(m.cache_write)}</td><td className={num}>{fmtPct(m.hit_rate)}</td>
                <td className={num}>{m.total ? (m.unpriced_tokens ? <a href="#/settings/stats" className="text-blue hover:underline">未定价</a> : fmtUsd(m.est)) : m.reported ? <span title="适配器报价（下限）">≥{fmtUsd(m.reported)}</span> : "—"}</td></tr>
            ))}
          </Table>
        </section>
      )}
      {d.backfill_only && <div className="type-footnote text-muted-foreground">这件事项在 token 明细开始记录之前完成，只有会话级的适配器报价，是下限。</div>}
    </div>
  )
}

/** one axis, left to right: created → turns (thin bars) → decisions (diamonds; orange = waiting for you now) → deliveries, verdicts, releases */
function Timeline({ d }: { d: ItemDetail }) {
  const box = useRef<HTMLDivElement>(null)
  const [w, setW] = useState(800)
  const [tip, setTip] = useState<{ x: number; text: string } | null>(null)
  useEffect(() => { const el = box.current; if (!el) return; const ro = new ResizeObserver(() => setW(el.clientWidth || 800)); ro.observe(el); setW(el.clientWidth || 800); return () => ro.disconnect() }, [])
  const f = d.facts, now = Date.now()
  const t0 = f.created, t1 = Math.max(t0 + 60000, f.done_at ?? f.dropped_at ?? now)
  const L = 8, R = 8, W = Math.max(1, w - L - R)
  const x = (t: number) => L + ((Math.min(Math.max(t, t0), t1) - t0) / (t1 - t0)) * W
  const turns = d.turns ?? [], decs = d.decisions ?? [], marks = d.marks ?? []
  const ticks = marks.filter((m) => m.kind === "deliver" || m.kind === "release" || (m.kind === "status" && (m.to_v === "done" || m.to_v === "drop")))
  const show = (t: number, text: string) => setTip({ x: x(t), text })
  return (
    <section data-timeline>
      <SectionLabel>时间轴</SectionLabel>
      <div ref={box} className="relative" onMouseLeave={() => setTip(null)}>
        <svg width={w} height={92} role="img" aria-label="事项时间轴" className="block overflow-visible">
          <line x1={L} x2={L + W} y1={46} y2={46} stroke="var(--border)" />
          <g>{turns.map((t) => {
            const a = x(t.started), b = x(t.ended ?? now)
            return <rect key={t.id} x={a} y={40} width={Math.max(2, b - a)} height={12} rx={2} className={cn("cursor-default", t.stop_reason === "error" ? "fill-destructive/70" : "fill-foreground/45 hover:fill-blue")}
              onMouseEnter={() => show(t.started, `回合 ${time(t.started)} · ${t.ended ? fmtMs(t.ended - t.started) : "进行中"} · ${fmtN(t.usage.total)} tokens · 工具 ${t.tools}`)} />
          })}</g>
          <g>{decs.map((c) => {
            const cx = x(c.created), open = c.status === "open"
            return <path key={c.id} d={`M${cx},12 l6,6 l-6,6 l-6,-6Z`} className={open ? "fill-attention" : "fill-foreground/35 hover:fill-blue"} stroke="var(--background)" strokeWidth={2}
              onMouseEnter={() => show(c.created, `${DECISION[c.kind] ?? c.kind} ${time(c.created)}${c.resolved ? ` · ${fmtMs(c.resolved - c.created)} 后处理${c.by === "yolo" ? "（YOLO）" : ""}${c.choice && CHOICE[c.choice] ? " · " + CHOICE[c.choice] : ""}` : open ? " · 等你" : " · 已过期"}`)} />
          })}</g>
          <g>{ticks.map((m, i) => {
            const cx = x(m.ts), bad = m.kind === "release" && m.to_v !== "released"
            const label = m.kind === "deliver" ? (m.to_v === "auto" ? "交付（自动验收）" : "交付") : m.kind === "release" ? RELEASE[m.to_v ?? ""] ?? "发布" : m.to_v === "done" ? "完成" : "放弃"
            return <g key={i} onMouseEnter={() => show(m.ts, `${label} ${time(m.ts)}${m.backfill ? "（回填，近似）" : ""}`)}>
              <line x1={cx} x2={cx} y1={58} y2={72} stroke={bad ? "var(--destructive)" : "var(--muted-foreground)"} strokeWidth={2} />
              <circle cx={cx} cy={76} r={4} className={bad ? "fill-destructive" : m.kind === "status" ? "fill-foreground" : "fill-muted-foreground"} stroke="var(--background)" strokeWidth={2} />
            </g>
          })}</g>
        </svg>
        {tip && <div className="pointer-events-none absolute -top-8 z-10 whitespace-nowrap rounded-md border bg-popover px-2 py-1 type-footnote text-popover-foreground shadow-sm" style={{ left: Math.min(Math.max(0, tip.x - 40), Math.max(0, w - 260)) }} role="status">{tip.text}</div>}
        <Meta className="mt-1"><span>{time(t0)} 新建</span><span>{f.done_at ? `${time(f.done_at)} 完成` : f.dropped_at ? `${time(f.dropped_at)} 放弃` : "进行中"}</span><span className="inline-flex items-center gap-1"><span className="inline-block h-2 w-3 rounded-sm bg-foreground/45" />回合</span><span className="inline-flex items-center gap-1"><span className="inline-block size-2 rotate-45 bg-foreground/35" />决定（橙色 = 在等你）</span><span className="inline-flex items-center gap-1"><span className="inline-block size-2 rounded-full bg-muted-foreground" />交付、发布、完成</span></Meta>
      </div>
    </section>
  )
}

/** 事项详情属性里的两行（stats.md §6.5）：用量与用时，点击进下钻；TS Hub 上不出现 */
export function ItemStatsRows({ id, render }: { id: string; render: (usage: string, time: string, href: string) => React.ReactNode }) {
  const [d] = useStats(() => stats.item(id, true), [id, st.items.find((i) => i.id === id)?.updated])
  if (d.s !== "ok") return null
  const u = d.v.usage, f = d.v.facts
  const usage = u.total ? `${fmtN(u.total)} tokens${u.hit_rate != null ? ` · 命中 ${fmtPct(u.hit_rate)}` : ""} · 估 ${fmtUsd(u.est)}` : u.reported ? `适配器报价 ≥${fmtUsd(u.reported)}` : "还没有用量"
  const tm = `Agent ${fmtMs(f.agent_ms ?? 0)} · 等你 ${fmtMs(f.wait_ms ?? 0)} · ${f.done_at ? "用了" : "已"} ${fmtMs(d.v.age_ms)}`
  return <>{render(usage, tm, "#/stats/items/" + id)}</>
}

/** 会话页顶栏：“本会话 估 $1.88 · 38 回合”（stats.md §6.5）；TS Hub 上或还没有回合时不出现 */
export function SessionStats({ id }: { id: string }) {
  const [d] = useStats(() => stats.session(id), [id])
  if (d.s !== "ok" || (!d.v.turns && !d.v.usage.total && !d.v.usage.reported)) return null
  const u = d.v.usage
  const est = u.total ? `估 ${fmtUsd(u.est)}` : u.reported ? `报价 ≥${fmtUsd(u.reported)}` : ""
  return (
    <span className="hidden whitespace-nowrap px-1 type-footnote tabular-nums text-muted-foreground md:inline" title={`本会话 ${fmtN(u.total)} tokens${u.hit_rate != null ? `，缓存命中 ${fmtPct(u.hit_rate)}` : ""}；费用是估算`} data-session-stats>
      本会话 {est}{est && " · "}{d.v.turns} 回合
    </span>
  )
}

/** 项目页 / Bot 页的一行：“最近 30 天：完成 6 · 中位前置 3 小时 · 估 $24” */
export function RecentStats({ project, bot, className }: { project?: string; bot?: string; className?: string }) {
  const [d] = useStats(() => stats.summary({ range: "30d", project, bot }), [project, bot])
  if (d.s !== "ok") return null
  const s = d.v
  const parts = bot
    ? [`回合 ${fmtN(s.agents.turns)}`, `首次通过 ${fmtPct(s.quality.first_pass)}`, `估 ${fmtUsd(s.spend.est)}`]
    : [`完成 ${s.throughput.done}`, `中位前置 ${fmtMs(s.time.lead.p50)}`, `估 ${fmtUsd(s.spend.est)}`]
  return <a href="#/stats" className={cn("type-footnote text-muted-foreground hover:underline", className)} data-recent-stats>最近 30 天：{parts.join(" · ")}</a>
}
