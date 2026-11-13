/* 单件事项的下钻（stats.md §6.4）：报告面板里先是三条泳道的时间轴（回合 / 决定 / 交付、验收、发布），再是一条数字条，
 * 费用去向（cost.tsx：每个回合和子事项一步，回合表并在里面）和按模型的用量表。事项页的“用量 / 用时”两行也从这里取数（brief）。 */
import { useEffect, useRef, useState } from "react"
import { cn } from "cn"
import { ChartGanttIcon, ChevronRightIcon, CpuIcon } from "lucide-react"
import { Panel, Tag } from "@/panel"
import { Spinner } from "@/components/ui/spinner"
import { stats, type ItemDetail } from "@/api/stats"
import { st } from "@/store"
import { Page, Empty } from "@/ui"
import { Unwired } from "@/unwired"
import { Kpi, Est, kpiRow } from "@/stats/kpi"
import { Table, num, th } from "@/stats/tables"
import { CostPanel } from "@/stats/cost"
import { t, useT } from "@/i18n"
import { fmtEst, fmtMs, fmtN, fmtPct, fmtUsd, tickLabel, time, timeTicks, useStats } from "@/stats/format"

/* labels for the Hub's values, built at render so they follow the language */
export const STOP = (): Record<string, string> => ({ end_turn: t("stats:drill.stop.endTurn"), cancelled: t("stats:drill.stop.cancelled"), max_tokens: t("stats:drill.stop.maxTokens"), refusal: t("stats:drill.stop.refusal"), error: t("stats:drill.stop.error"), interrupted: t("stats:drill.stop.interrupted") })
export const DECISION = (): Record<string, string> => ({ accept: t("stats:drill.decision.accept"), ask: t("stats:drill.decision.ask"), approve: t("stats:drill.decision.approve"), fail: t("stats:drill.decision.fail"), propose: t("stats:drill.decision.propose"), confirm: t("stats:drill.decision.confirm") })
const CHOICE = (): Record<string, string> => ({ pass: t("stats:drill.choice.pass"), rework: t("stats:drill.choice.rework") })
export const RELEASE = (): Record<string, string> => ({ released: t("stats:drill.release.released"), failed: t("stats:drill.release.failed"), not_released: t("stats:drill.release.notReleased"), rolled_back: t("stats:drill.release.rolledBack"), unknown: t("stats:drill.release.unknown") })

export function StatsItemView({ id }: { id: string }) {
  const t = useT()
  const [d] = useStats(() => stats.item(id), [id])
  if (d.s === "absent") return <Unwired id="stats" />
  const title = d.s === "ok" ? d.v.title : st.items.find((i) => i.id === id)?.t ?? id
  return (
    <Page title={<span className="flex min-w-0 items-baseline gap-2"><span className="shrink-0 font-mono font-normal tabular-nums text-tertiary" data-no>{id}</span><span className="min-w-0 break-words">{title}</span></span>} back={["#/stats", t("stats:page.title")]} max={1280}
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
  const open = f.done_at == null && f.dropped_at == null
  return (
    <div className="flex flex-col gap-4" data-drill>
      <Panel k="report" className="@container" icon={<ChartGanttIcon />} title={t("stats:drill.report")} sub={t("stats:drill.reportSub", { created: time(f.created) })}
        right={open ? <Tag tone="run"><span className="size-1.5 dot-running rounded-full bg-current" />{t("stats:word.running")}</Tag> : f.done_at ? <Tag tone="ok">{t("stats:drill.doneMark")}</Tag> : <Tag>{t("stats:drill.dropMark")}</Tag>}>
        <Timeline d={d} />
        <div className={kpiRow(6)}>
          <Kpi k="tokens" label="tokens" value={fmtN(u.total)} foot={u.hit_rate != null ? t("stats:drill.hit", { pct: fmtPct(u.hit_rate) }) : ""} def={t("stats:drill.tokensDef")} />
          <Kpi k="est" label={t("stats:col.cost")} badge={<Est />} value={u.total ? (u.unpriced_tokens ? (u.est >= 0.01 ? "≥" + fmtUsd(u.est) : "—") : fmtUsd(u.est)) : u.reported ? "≥" + fmtUsd(u.reported) : "—"} foot={u.unpriced_tokens ? t("stats:drill.incomplete", { n: fmtN(u.unpriced_tokens) }) : u.reported ? t("stats:word.adapterQuote", { usd: fmtUsd(u.reported) }) : ""} def={t("stats:drill.estDef")} />
          <Kpi k="lead" label={t("stats:drill.lead")} value={fmtMs(f.lead_ms ?? d.age_ms)} foot={f.lead_ms != null ? t("stats:drill.leadDone") : t("stats:drill.leadOpen")} def={t("stats:drill.leadDef")} />
          <Kpi k="agent" label={t("stats:drill.agent")} value={fmtMs(f.agent_ms)} foot={t("stats:drill.turns", { count: f.turns })} def={t("stats:drill.agentDef")} />
          <Kpi k="wait" label={t("stats:drill.wait")} value={fmtMs(f.wait_ms)} tone={f.wait_ms && f.agent_ms && f.wait_ms > f.agent_ms ? "wait" : undefined} foot={t("stats:drill.decisions", { count: f.decisions })} def={t("stats:drill.waitDef")} />
          <Kpi k="reworks" label={t("stats:drill.reworks")} value={f.reworks} tone={f.reworks ? "wait" : undefined} foot={f.first_pass == null ? t("stats:drill.noVerdict") : f.first_pass ? t("stats:drill.firstPass") : t("stats:drill.firstFail")} def={t("stats:drill.reworksDef")} />
        </div>
      </Panel>
      <CostPanel d={d} />
      {!!d.by_model?.length && (
        <Panel k="by-model" icon={<CpuIcon />} title={t("stats:drill.byModel")} sub={t("stats:drill.byModelSub")}>
          <div className="pb-2"><Table label={t("stats:drill.byModel")} min={640} head={<tr><th className={th}>{t("stats:col.model")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.input")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.output")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.cacheRead")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.cacheWrite")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.hitRate")}</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">{t("stats:col.cost")}<Est /></span></th></tr>}>
            {d.by_model.map((m) => (
              <tr key={m.model} data-model={m.model}><td className="py-2.5">{m.label}</td><td className={num}>{fmtN(m.input)}</td><td className={num}>{fmtN(m.output)}</td><td className={num}>{fmtN(m.cache_read)}</td><td className={num}>{fmtN(m.cache_write)}</td><td className={num}>{fmtPct(m.hit_rate)}</td>
                <td className={num}>{m.total ? (m.unpriced_tokens ? <a href="#/settings/stats" className="text-blue hover:underline">{t("stats:word.unpriced")}</a> : fmtUsd(m.est)) : m.reported ? <span title={t("stats:word.adapterQuoteLowerBound")}>≥{fmtUsd(m.reported)}</span> : "—"}</td></tr>
            ))}
          </Table></div>
        </Panel>
      )}
    </div>
  )
}

/** one axis, three lanes (the run as a Gantt): turns as bars, decisions as diamonds (orange = waiting for you now),
 *  deliveries, verdicts and releases as dots; the time axis in monospace under them, “now” dashed while the item is open */
function Timeline({ d }: { d: ItemDetail }) {
  const t = useT()
  const box = useRef<HTMLDivElement>(null)
  const [w, setW] = useState(800)
  const [tip, setTip] = useState<{ x: number; y: number; text: string } | null>(null)
  useEffect(() => { const el = box.current; if (!el) return; const ro = new ResizeObserver(() => setW(el.clientWidth || 800)); ro.observe(el); setW(el.clientWidth || 800); return () => ro.disconnect() }, [])
  const f = d.facts, now = Date.now()
  const open = f.done_at == null && f.dropped_at == null
  const t0 = f.created, t1 = Math.max(t0 + 60000, f.done_at ?? f.dropped_at ?? now)
  const LABEL = w < 480 ? 0 : 72, L = LABEL + 6, R = 10, W = Math.max(1, w - L - R)
  const x = (t: number) => L + ((Math.min(Math.max(t, t0), t1) - t0) / (t1 - t0)) * W
  const turns = d.turns ?? [], decs = d.decisions ?? [], marks = d.marks ?? []
  const ticks = marks.filter((m) => m.kind === "deliver" || m.kind === "release" || (m.kind === "status" && (m.to_v === "done" || m.to_v === "drop")))
  const show = (at: number, y: number, text: string) => setTip({ x: x(at), y, text })
  const decision = DECISION(), choice = CHOICE(), release = RELEASE()
  const LANE = 30, Y = { turns: 14, decs: 14 + LANE, marks: 14 + LANE * 2 }, AXIS = 14 + LANE * 3, H = AXIS + 22
  /* the two ends at their exact time (with the date once the span covers more than one day), round times between them */
  const multiDay = new Date(t0).toDateString() !== new Date(t1).toDateString()
  const edge = (ts: number) => { const dt = new Date(ts), hm = `${String(dt.getHours()).padStart(2, "0")}:${String(dt.getMinutes()).padStart(2, "0")}`; return multiDay ? `${dt.getMonth() + 1}/${dt.getDate()} ${hm}` : hm }
  const room = multiDay ? 104 : 64, inner = timeTicks(t0, t1, W).filter((ts, i, xs) => x(ts) - x(t0) > room && x(t1) - x(ts) > room && (i === 0 || x(ts) - x(xs[i - 1]) > 48))
  const tickAt: [number, string, boolean][] = [[t0, edge(t0), true], ...inner.map((ts): [number, string, boolean] => [ts, tickLabel(ts), false]), [t1, edge(t1), true]]
  const lanes: [keyof typeof Y, string][] = [["turns", t("stats:drill.laneTurns")], ["decs", t("stats:drill.laneDecisions")], ["marks", t("stats:drill.laneMarks")]]
  return (
    <section className="px-5 pt-2 pb-4" data-timeline>
      <div className="mb-3 flex items-center justify-between gap-3">
        <span className="type-label text-tertiary">{t("stats:drill.timeline")}</span>
        <span className="type-footnote tabular-nums text-muted-foreground">{fmtMs(t1 - t0)}</span>
      </div>
      <div ref={box} className="relative" onMouseLeave={() => setTip(null)}>
        <svg width={w} height={H} role="img" aria-label={t("stats:drill.timelineAria")} className="block overflow-visible">
          {tickAt.map(([ts, label, end], i) => <g key={i}>
            <line x1={x(ts)} x2={x(ts)} y1={0} y2={AXIS} stroke="var(--border)" strokeOpacity={end ? 1 : 0.6} />
            <text x={x(ts)} y={AXIS + 16} textAnchor={i === 0 ? "start" : i === tickAt.length - 1 ? "end" : "middle"} className="fill-tertiary type-footnote tabular-nums">{label}</text>
          </g>)}
          {lanes.map(([k, label]) => <g key={k}>
            {LABEL > 0 && <text x={0} y={Y[k]} dy="0.35em" className="fill-muted-foreground type-footnote">{label}</text>}
            <line x1={L} x2={L + W} y1={Y[k]} y2={Y[k]} stroke="var(--border)" strokeOpacity={0.6} />
          </g>)}
          <g>{turns.map((tn) => {
            const a = x(tn.started), b = x(tn.ended ?? now), bad = tn.stop_reason === "error" || tn.transient
            return <rect key={tn.id} x={a} y={Y.turns - 6} width={Math.max(3, b - a)} height={12} rx={4} className={cn("cursor-default transition-[fill] duration-120", bad ? "fill-destructive/80" : tn.ended ? "fill-blue/70 hover:fill-blue" : "fill-blue")}
              onMouseEnter={() => show(tn.started, Y.turns, t("stats:drill.turnTip", { time: time(tn.started), dur: tn.ended ? fmtMs(tn.ended - tn.started) : t("stats:word.running"), tokens: fmtN(tn.usage.total), tools: tn.tools }))} />
          })}</g>
          <g>{decs.map((c) => {
            const cx = x(c.created), cy = Y.decs, waiting = c.status === "open"
            const end = c.resolved ? x(c.resolved) : waiting ? x(now) : cx
            return <g key={c.id}>
              {end > cx + 1 && <line x1={cx} x2={end} y1={cy} y2={cy} stroke={waiting ? "var(--attention)" : "var(--muted-foreground)"} strokeOpacity={0.6} strokeWidth={2} strokeDasharray={waiting ? "3 3" : undefined} />}
              <path d={`M${cx},${cy - 6} l6,6 l-6,6 l-6,-6Z`} className={waiting ? "fill-attention" : "fill-foreground/60 hover:fill-blue"} stroke="var(--card)" strokeWidth={2}
                onMouseEnter={() => {
                  const v = { kind: decision[c.kind] ?? c.kind, time: time(c.created) }
                  if (!c.resolved) return show(c.created, cy, waiting ? t("stats:drill.decOpen", v) : t("stats:drill.decExpired", v))
                  const done = t(c.by === "yolo" ? "stats:drill.decResolvedYolo" : "stats:drill.decResolved", { ...v, dur: fmtMs(c.resolved - c.created) })
                  show(c.created, cy, c.choice && choice[c.choice] ? t("stats:drill.decChoice", { text: done, choice: choice[c.choice] }) : done)
                }} />
            </g>
          })}</g>
          <g>{ticks.map((m, i) => {
            const cx = x(m.ts), bad = m.kind === "release" && m.to_v !== "released"
            const label = m.kind === "deliver" ? (m.to_v === "auto" ? t("stats:drill.deliverAuto") : t("stats:drill.deliver")) : m.kind === "release" ? release[m.to_v ?? ""] ?? t("stats:drill.releaseMark") : m.to_v === "done" ? t("stats:drill.doneMark") : t("stats:drill.dropMark")
            return <circle key={i} cx={cx} cy={Y.marks} r={5} className={bad ? "fill-destructive" : m.kind === "status" ? "fill-success" : "fill-foreground/70"} stroke="var(--card)" strokeWidth={2}
              onMouseEnter={() => show(m.ts, Y.marks, t("stats:drill.markTip", { label, time: time(m.ts) }))} />
          })}</g>
          {open && <g>
            <line x1={x(now)} x2={x(now)} y1={0} y2={AXIS} stroke="var(--foreground)" strokeOpacity={0.5} strokeDasharray="3 3" />
          </g>}
        </svg>
        {tip && <div className="pointer-events-none absolute z-10 whitespace-nowrap rounded-lg bg-popover px-2.5 py-1 type-footnote tabular-nums text-popover-foreground shadow-md" style={{ left: Math.min(Math.max(0, tip.x - 40), Math.max(0, w - 280)), top: tip.y - 34 }} role="status">{tip.text}</div>}
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
  const usage = u.total ? (u.hit_rate != null ? t("stats:drill.usageHit", { tokens: fmtN(u.total), pct: fmtPct(u.hit_rate), usd: fmtEst(u.est, u.unpriced_tokens) }) : t("stats:drill.usage", { tokens: fmtN(u.total), usd: fmtEst(u.est, u.unpriced_tokens) })) : u.reported ? t("stats:word.adapterQuoteFloor", { usd: fmtUsd(u.reported) }) : t("stats:drill.noUsage")
  const tm = t(f.done_at ? "stats:drill.timeDone" : "stats:drill.timeOpen", { agent: fmtMs(f.agent_ms ?? 0), wait: fmtMs(f.wait_ms ?? 0), age: fmtMs(d.v.age_ms) })
  return <>{render(usage, tm, "#/stats/items/" + id)}</>
}

/** 会话页顶栏：“本会话 估 $1.88 · 38 回合”（stats.md §6.5）；TS Hub 上或还没有回合时不出现 */
export function SessionStats({ id, ctx }: { id: string; /** the context in use, %, when the harness reports it */ ctx?: number }) {
  const t = useT()
  const [d] = useStats(() => stats.session(id), [id])
  const ctxTag = ctx != null && <span className={cn("hidden whitespace-nowrap px-1 type-footnote tabular-nums md:inline", ctx >= 80 ? "text-attention" : "text-muted-foreground")} data-session-ctx-head={ctx}>{t("views:sessionPanel.ctxShort", { pct: ctx })}</span>
  if (d.s !== "ok" || (!d.v.turns && !d.v.usage.total && !d.v.usage.reported)) return ctxTag || null
  const u = d.v.usage
  const est = u.total ? t("stats:word.estShort", { usd: fmtUsd(u.est) }) : u.reported ? t("stats:word.quoteFloor", { usd: fmtUsd(u.reported) }) : ""
  return (<>{ctxTag}
    <span className="hidden whitespace-nowrap px-1 type-footnote tabular-nums text-muted-foreground md:inline" title={u.hit_rate != null ? t("stats:drill.sessionTitleHit", { tokens: fmtN(u.total), pct: fmtPct(u.hit_rate) }) : t("stats:drill.sessionTitle", { tokens: fmtN(u.total) })} data-session-stats>
      {est ? t("stats:drill.sessionLine", { est, count: d.v.turns }) : t("stats:drill.sessionLineNoEst", { count: d.v.turns })}
    </span></>
  )
}
