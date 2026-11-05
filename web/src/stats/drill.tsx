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
import { t, useT } from "@/i18n"
import { fmtMs, fmtN, fmtPct, fmtUsd, time, useStats } from "@/stats/format"

/* labels for the Hub's values, built at render so they follow the language */
const STOP = (): Record<string, string> => ({ end_turn: t("stats:drill.stop.endTurn"), cancelled: t("stats:drill.stop.cancelled"), max_tokens: t("stats:drill.stop.maxTokens"), refusal: t("stats:drill.stop.refusal"), error: t("stats:drill.stop.error"), interrupted: t("stats:drill.stop.interrupted") })
const DECISION = (): Record<string, string> => ({ accept: t("stats:drill.decision.accept"), ask: t("stats:drill.decision.ask"), approve: t("stats:drill.decision.approve"), fail: t("stats:drill.decision.fail"), propose: t("stats:drill.decision.propose"), confirm: t("stats:drill.decision.confirm") })
const CHOICE = (): Record<string, string> => ({ pass: t("stats:drill.choice.pass"), rework: t("stats:drill.choice.rework") })
const RELEASE = (): Record<string, string> => ({ released: t("stats:drill.release.released"), failed: t("stats:drill.release.failed"), not_released: t("stats:drill.release.notReleased"), rolled_back: t("stats:drill.release.rolledBack"), unknown: t("stats:drill.release.unknown") })

export function StatsItemView({ id }: { id: string }) {
  const t = useT()
  const [d] = useStats(() => stats.item(id), [id])
  if (d.s === "absent") return <Unwired id="stats" />
  const title = d.s === "ok" ? d.v.title : st.items.find((i) => i.id === id)?.t ?? id
  return (
    <Page title={<span className="inline-flex min-w-0 items-baseline gap-2"><ItemNo id={id} className="text-[15px]" /><span className="truncate">{title}</span></span>} back={["#/stats", t("stats:page.title")]} max={1040}
      sub={<a href={"#/items/" + id} className="inline-flex items-center gap-0.5 text-blue hover:underline" data-item-link>{t("stats:drill.openItem")}<ChevronRightIcon className="size-3.5" /></a>}>
      {d.s === "loading" ? <div className="flex justify-center py-24"><Spinner className="size-5 text-muted-foreground" /></div>
        : d.s === "error" ? <Empty title={t("stats:drill.loadFailed")} sub={d.msg} />
        : <Drill d={d.v} />}
    </Page>
  )
}

function Drill({ d }: { d: ItemDetail }) {
  const t = useT()
  const f = d.facts, u = d.usage
  const turns = d.turns ?? []
  const stop = STOP()
  return (
    <div className="flex flex-col gap-12" data-drill>
      <div className="grid grid-cols-2 gap-6 sm:grid-cols-3 lg:grid-cols-6">
        <Kpi k="tokens" label="tokens" value={fmtN(u.total)} foot={u.hit_rate != null ? t("stats:drill.hit", { pct: fmtPct(u.hit_rate) }) : d.backfill_only ? t("stats:drill.sessionOnly") : ""} def={t("stats:drill.tokensDef")} />
        <Kpi k="est" label={t("stats:word.estCost")} badge={<Est />} value={u.total ? fmtUsd(u.est) : u.reported ? "≥" + fmtUsd(u.reported) : "—"} foot={u.unpriced_tokens ? t("stats:drill.incomplete", { n: fmtN(u.unpriced_tokens) }) : u.reported ? t("stats:word.adapterQuote", { usd: fmtUsd(u.reported) }) : ""} def={t("stats:drill.estDef")} />
        <Kpi k="lead" label={t("stats:drill.lead")} value={fmtMs(f.lead_ms ?? d.age_ms)} foot={f.lead_ms != null ? t("stats:drill.leadDone") : t("stats:drill.leadOpen")} def={t("stats:drill.leadDef")} />
        <Kpi k="agent" label={t("stats:drill.agent")} value={fmtMs(f.agent_ms)} foot={t("stats:drill.turns", { count: f.turns })} def={t("stats:drill.agentDef")} />
        <Kpi k="wait" label={t("stats:drill.wait")} value={fmtMs(f.wait_ms)} foot={t("stats:drill.decisions", { count: f.decisions })} def={t("stats:drill.waitDef")} />
        <Kpi k="reworks" label={t("stats:drill.reworks")} value={f.reworks} foot={f.first_pass == null ? t("stats:drill.noVerdict") : f.first_pass ? t("stats:drill.firstPass") : t("stats:drill.firstFail")} def={t("stats:drill.reworksDef")} />
      </div>
      <Timeline d={d} />
      <section>
        <SectionLabel>{t("stats:drill.turns")}</SectionLabel>
        {!turns.length ? <Empty title={t("stats:drill.noTurns")} /> : (
          <Table label={t("stats:drill.turns")} min={820} head={<tr><th className={th}>{t("stats:col.start")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.duration")}</th><th className={cn(th, "pl-4")}>{t("stats:col.stopReason")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.tools")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.failed")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.input")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.output")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.cacheRead")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.cacheWrite")}</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">{t("stats:col.est")}<Est /></span></th></tr>}>
            {turns.map((tn) => (
              <tr key={tn.id} className="cursor-pointer hover:bg-accent" onClick={() => go("#/sessions/" + tn.session_id)} data-turn={tn.id}>
                <td className="py-2 tabular-nums">{time(tn.started)}{tn.attempt > 0 && <span className="ml-1 type-footnote text-muted-foreground">{t("stats:drill.retry", { n: tn.attempt })}</span>}</td>
                <td className={num}>{tn.ended ? fmtMs(tn.ended - tn.started) : t("stats:word.running")}</td>
                <td className="py-2 pl-4"><span className="inline-flex items-center gap-1.5">{(tn.stop_reason === "error" || tn.transient) && <span aria-hidden className="size-2 rounded-full bg-destructive" />}{tn.transient ? t("stats:drill.gatewayDown") : stop[tn.stop_reason ?? ""] ?? tn.stop_reason ?? "—"}</span></td>
                <td className={num}>{tn.tools}</td><td className={num}>{tn.tool_errors || "—"}</td>
                <td className={num}>{fmtN(tn.usage.input)}</td><td className={num}>{fmtN(tn.usage.output)}</td><td className={num}>{fmtN(tn.usage.cache_read)}</td><td className={num}>{fmtN(tn.usage.cache_write)}</td>
                <td className={num}>{tn.usage.total ? fmtUsd(tn.usage.est) : "—"}</td>
              </tr>
            ))}
          </Table>
        )}
      </section>
      {!!d.by_model?.length && (
        <section>
          <SectionLabel>{t("stats:drill.byModel")}</SectionLabel>
          <Table label={t("stats:drill.byModel")} min={640} head={<tr><th className={th}>{t("stats:col.model")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.input")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.output")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.cacheRead")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.cacheWrite")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.hitRate")}</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">{t("stats:word.estCost")}<Est /></span></th></tr>}>
            {d.by_model.map((m) => (
              <tr key={m.model} data-model={m.model}><td className="py-2">{m.label}</td><td className={num}>{fmtN(m.input)}</td><td className={num}>{fmtN(m.output)}</td><td className={num}>{fmtN(m.cache_read)}</td><td className={num}>{fmtN(m.cache_write)}</td><td className={num}>{fmtPct(m.hit_rate)}</td>
                <td className={num}>{m.total ? (m.unpriced_tokens ? <a href="#/settings/stats" className="text-blue hover:underline">{t("stats:word.unpriced")}</a> : fmtUsd(m.est)) : m.reported ? <span title={t("stats:word.adapterQuoteLowerBound")}>≥{fmtUsd(m.reported)}</span> : "—"}</td></tr>
            ))}
          </Table>
        </section>
      )}
      {d.backfill_only && <div className="type-footnote text-muted-foreground">{t("stats:drill.backfillNote")}</div>}
    </div>
  )
}

/** one axis, left to right: created → turns (thin bars) → decisions (diamonds; orange = waiting for you now) → deliveries, verdicts, releases */
function Timeline({ d }: { d: ItemDetail }) {
  const t = useT()
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
  const show = (at: number, text: string) => setTip({ x: x(at), text })
  const decision = DECISION(), choice = CHOICE(), release = RELEASE()
  return (
    <section data-timeline>
      <SectionLabel>{t("stats:drill.timeline")}</SectionLabel>
      <div ref={box} className="relative" onMouseLeave={() => setTip(null)}>
        <svg width={w} height={92} role="img" aria-label={t("stats:drill.timelineAria")} className="block overflow-visible">
          <line x1={L} x2={L + W} y1={46} y2={46} stroke="var(--border)" />
          <g>{turns.map((tn) => {
            const a = x(tn.started), b = x(tn.ended ?? now)
            return <rect key={tn.id} x={a} y={40} width={Math.max(2, b - a)} height={12} rx={2} className={cn("cursor-default", tn.stop_reason === "error" ? "fill-destructive/70" : "fill-foreground/45 hover:fill-blue")}
              onMouseEnter={() => show(tn.started, t("stats:drill.turnTip", { time: time(tn.started), dur: tn.ended ? fmtMs(tn.ended - tn.started) : t("stats:word.running"), tokens: fmtN(tn.usage.total), tools: tn.tools }))} />
          })}</g>
          <g>{decs.map((c) => {
            const cx = x(c.created), open = c.status === "open"
            return <path key={c.id} d={`M${cx},12 l6,6 l-6,6 l-6,-6Z`} className={open ? "fill-attention" : "fill-foreground/35 hover:fill-blue"} stroke="var(--background)" strokeWidth={2}
              onMouseEnter={() => {
                const v = { kind: decision[c.kind] ?? c.kind, time: time(c.created) }
                if (!c.resolved) return show(c.created, open ? t("stats:drill.decOpen", v) : t("stats:drill.decExpired", v))
                const done = t(c.by === "yolo" ? "stats:drill.decResolvedYolo" : "stats:drill.decResolved", { ...v, dur: fmtMs(c.resolved - c.created) })
                show(c.created, c.choice && choice[c.choice] ? t("stats:drill.decChoice", { text: done, choice: choice[c.choice] }) : done)
              }} />
          })}</g>
          <g>{ticks.map((m, i) => {
            const cx = x(m.ts), bad = m.kind === "release" && m.to_v !== "released"
            const label = m.kind === "deliver" ? (m.to_v === "auto" ? t("stats:drill.deliverAuto") : t("stats:drill.deliver")) : m.kind === "release" ? release[m.to_v ?? ""] ?? t("stats:drill.releaseMark") : m.to_v === "done" ? t("stats:drill.doneMark") : t("stats:drill.dropMark")
            return <g key={i} onMouseEnter={() => show(m.ts, t(m.backfill ? "stats:drill.markTipBackfill" : "stats:drill.markTip", { label, time: time(m.ts) }))}>
              <line x1={cx} x2={cx} y1={58} y2={72} stroke={bad ? "var(--destructive)" : "var(--muted-foreground)"} strokeWidth={2} />
              <circle cx={cx} cy={76} r={4} className={bad ? "fill-destructive" : m.kind === "status" ? "fill-foreground" : "fill-muted-foreground"} stroke="var(--background)" strokeWidth={2} />
            </g>
          })}</g>
        </svg>
        {tip && <div className="pointer-events-none absolute -top-8 z-10 whitespace-nowrap rounded-md border bg-popover px-2 py-1 type-footnote text-popover-foreground shadow-sm" style={{ left: Math.min(Math.max(0, tip.x - 40), Math.max(0, w - 260)) }} role="status">{tip.text}</div>}
        <Meta className="mt-1"><span>{t("stats:drill.createdAt", { time: time(t0) })}</span><span>{f.done_at ? t("stats:drill.doneAt", { time: time(f.done_at) }) : f.dropped_at ? t("stats:drill.droppedAt", { time: time(f.dropped_at) }) : t("stats:word.running")}</span><span className="inline-flex items-center gap-1"><span className="inline-block h-2 w-3 rounded-sm bg-foreground/45" />{t("stats:drill.legendTurn")}</span><span className="inline-flex items-center gap-1"><span className="inline-block size-2 rotate-45 bg-foreground/35" />{t("stats:drill.legendDecision")}</span><span className="inline-flex items-center gap-1"><span className="inline-block size-2 rounded-full bg-muted-foreground" />{t("stats:drill.legendMarks")}</span></Meta>
      </div>
    </section>
  )
}

/** 事项详情属性里的两行（stats.md §6.5）：用量与用时，点击进下钻；TS Hub 上不出现 */
export function ItemStatsRows({ id, render }: { id: string; render: (usage: string, time: string, href: string) => React.ReactNode }) {
  const t = useT()
  const [d] = useStats(() => stats.item(id, true), [id, st.items.find((i) => i.id === id)?.updated])
  if (d.s !== "ok") return null
  const u = d.v.usage, f = d.v.facts
  const usage = u.total ? (u.hit_rate != null ? t("stats:drill.usageHit", { tokens: fmtN(u.total), pct: fmtPct(u.hit_rate), usd: fmtUsd(u.est) }) : t("stats:drill.usage", { tokens: fmtN(u.total), usd: fmtUsd(u.est) })) : u.reported ? t("stats:word.adapterQuoteFloor", { usd: fmtUsd(u.reported) }) : t("stats:drill.noUsage")
  const tm = t(f.done_at ? "stats:drill.timeDone" : "stats:drill.timeOpen", { agent: fmtMs(f.agent_ms ?? 0), wait: fmtMs(f.wait_ms ?? 0), age: fmtMs(d.v.age_ms) })
  return <>{render(usage, tm, "#/stats/items/" + id)}</>
}

/** 会话页顶栏：“本会话 估 $1.88 · 38 回合”（stats.md §6.5）；TS Hub 上或还没有回合时不出现 */
export function SessionStats({ id }: { id: string }) {
  const t = useT()
  const [d] = useStats(() => stats.session(id), [id])
  if (d.s !== "ok" || (!d.v.turns && !d.v.usage.total && !d.v.usage.reported)) return null
  const u = d.v.usage
  const est = u.total ? t("stats:word.estShort", { usd: fmtUsd(u.est) }) : u.reported ? t("stats:word.quoteFloor", { usd: fmtUsd(u.reported) }) : ""
  return (
    <span className="hidden whitespace-nowrap px-1 type-footnote tabular-nums text-muted-foreground md:inline" title={u.hit_rate != null ? t("stats:drill.sessionTitleHit", { tokens: fmtN(u.total), pct: fmtPct(u.hit_rate) }) : t("stats:drill.sessionTitle", { tokens: fmtN(u.total) })} data-session-stats>
      {est ? t("stats:drill.sessionLine", { est, count: d.v.turns }) : t("stats:drill.sessionLineNoEst", { count: d.v.turns })}
    </span>
  )
}

/** 项目页 / Bot 页的一行：“最近 30 天：完成 6 · 中位前置 3 小时 · 估 $24” */
export function RecentStats({ project, bot, className }: { project?: string; bot?: string; className?: string }) {
  const t = useT()
  const [d] = useStats(() => stats.summary({ range: "30d", project, bot }), [project, bot])
  if (d.s !== "ok") return null
  const s = d.v
  const parts = bot
    ? [t("stats:drill.recentTurns", { n: fmtN(s.agents.turns) }), t("stats:drill.recentFirstPass", { pct: fmtPct(s.quality.first_pass) }), t("stats:word.estShort", { usd: fmtUsd(s.spend.est) })]
    : [t("stats:drill.recentDone", { n: s.throughput.done }), t("stats:drill.recentLead", { dur: fmtMs(s.time.lead.p50) }), t("stats:word.estShort", { usd: fmtUsd(s.spend.est) })]
  return <a href="#/stats" className={cn("type-footnote text-muted-foreground hover:underline", className)} data-recent-stats>{t("stats:drill.recent", { parts: parts.join(" · ") })}</a>
}
