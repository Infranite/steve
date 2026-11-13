/* 费用去向（事项下钻）：affectUI 的 Cost Waterfall。数字全来自统计接口：这件事项的回合（GET /stats/items/{id}），
 * 它启动的子事项（GET /stats/tree/{id}）和每件子事项自己的回合。每个回合是一步，子事项是可折叠的一组，
 * 按开始时间排，条从上一步结束的地方接着画，读下来就是总费用怎么累起来的。回合表并进了这里：点一步看它的详情和会话。 */
import { useEffect, useState, type ReactNode } from "react"
import { CoinsIcon } from "lucide-react"
import { Panel } from "@/panel"
import { Segmented, StatusDot } from "@/ui"
import { st } from "@/store"
import { stats, type ItemDetail, type Usage } from "@/api/stats"
import type { TreeStat } from "@/agent-tree"
import { CostWaterfall, type CostMode, type CostStep, type CostStepStatus } from "@/components/affect/cost-waterfall"
import { STOP } from "@/stats/drill"
import { t, useT } from "@/i18n"
import { fmtMs, fmtN, fmtPct, fmtUsd, hhmm, time } from "@/stats/format"

type Turn = NonNullable<ItemDetail["turns"]>[number]
/** what a step stands for, so its details can say more than the waterfall's row */
type Ref = { turn: Turn } | { item: string; node?: TreeStat; detail?: ItemDetail; failed?: boolean } | { other: Usage }
/** sub-items fetched one by one: at most this many, the rest stay out (the tree says when it was cut) */
const MAX_CHILDREN = 40
/** and at most this many of those fetches at a time */
const FETCH_AT_ONCE = 6

/** `f` over `xs`, at most `n` running at once, the results in order */
async function pooled<X, Y>(xs: X[], n: number, f: (x: X) => Promise<Y>): Promise<Y[]> {
  const out: Y[] = new Array(xs.length)
  let next = 0
  await Promise.all(Array.from({ length: Math.min(n, xs.length) }, async () => { while (next < xs.length) { const i = next++; out[i] = await f(xs[i]) } }))
  return out
}

/** a usage's cost: the estimate; with no tokens counted, the adapter's own quote (a lower bound) */
const costOf = (u: Usage) => (u.total > 0 ? u.est : u.reported > 0 ? u.reported : undefined)
/** the waterfall's "In": everything read, cached or not (its column head says so; the details split it) */
const inOf = (u: Usage) => u.input + u.cache_read + u.cache_write

/** a turn; `owner` is the sub-item it belongs to, named with it where the turn shows out of its group (the most expensive step) */
function turnStep(tn: Turn, refs: Map<string, Ref>, owner?: string): CostStep {
  const stop = STOP()
  const failed = tn.stop_reason === "error" || tn.transient
  const reason = tn.transient ? t("stats:drill.gatewayDown") : stop[tn.stop_reason ?? ""] ?? tn.stop_reason ?? undefined
  const id = "turn:" + tn.id
  refs.set(id, { turn: tn })
  return {
    id, kind: "llm", status: tn.ended == null ? "running" : failed ? "failed" : "done",
    name: t("stats:cost.turn", { time: hhmm(tn.started) }),
    fullName: owner ? t("stats:cost.turnOf", { item: owner, time: hhmm(tn.started) }) : undefined,
    detail: [tn.attempt > 0 ? t("stats:drill.retry", { n: tn.attempt }) : "", reason ?? "", tn.usage.total === 0 && tn.usage.reported > 0 ? t("stats:cost.quote") : ""].filter(Boolean).join(" · ") || undefined,
    tokensIn: inOf(tn.usage), tokensOut: tn.usage.output, cost: costOf(tn.usage), unpriced: tn.usage.unpriced_tokens,
    duration: (tn.ended ?? Date.now()) - tn.started,
    error: failed ? reason : undefined,
  }
}

/** the item's own steps: its turns and the sub-items it started, in the order they began */
function stepsOf(id: string, detail: ItemDetail | undefined, tree: TreeStat[], details: Map<string, ItemDetail | null>, refs: Map<string, Ref>, root = true): CostStep[] {
  const turns = detail?.turns ?? []
  const own: [number, CostStep][] = turns.map((tn) => [tn.started, turnStep(tn, refs, root ? undefined : id)])
  /* usage the turns don't hold (gateway calls made for the item, at no one time): one step at the end, so the total stays the item's */
  if (detail) {
    const u = detail.usage, inTurns = turns.reduce((a, tn) => a + (costOf(tn.usage) ?? 0), 0), all = costOf(u) ?? 0
    const tokens = turns.reduce((a, tn) => a + tn.usage.total, 0)
    if (all - inTurns >= 0.005 || u.total - tokens > 0) {
      const sid = "other:" + id
      const rest: Usage = { ...u, input: Math.max(0, u.input - turns.reduce((a, tn) => a + tn.usage.input, 0)), output: Math.max(0, u.output - turns.reduce((a, tn) => a + tn.usage.output, 0)), cache_read: Math.max(0, u.cache_read - turns.reduce((a, tn) => a + tn.usage.cache_read, 0)), cache_write: Math.max(0, u.cache_write - turns.reduce((a, tn) => a + tn.usage.cache_write, 0)), total: Math.max(0, u.total - tokens), est: Math.max(0, all - inTurns), unpriced_tokens: Math.max(0, u.unpriced_tokens - turns.reduce((a, tn) => a + tn.usage.unpriced_tokens, 0)), hit_rate: null }
      refs.set(sid, { other: rest })
      own.push([Number.MAX_SAFE_INTEGER, { id: sid, kind: "tool", status: "done", name: t("stats:cost.other"), detail: t("stats:cost.otherDetail"), tokensIn: inOf(rest), tokensOut: rest.output, cost: rest.est, unpriced: rest.unpriced_tokens }])
    }
  }
  for (const n of tree.filter((x) => x.parent_id === id)) {
    const d = details.get(n.item_id), item = st.items.find((i) => i.id === n.item_id)
    const sid = "item:" + n.item_id
    refs.set(sid, { item: n.item_id, node: n, detail: d ?? undefined, failed: d === null })
    const status: CostStepStatus = item?.st === "fail" ? "failed" : n.done_at == null && n.dropped_at == null && (item?.st === "run" || !item) ? "running" : "done"
    const children = stepsOf(n.item_id, d ?? undefined, tree, details, refs, false)
    own.push([n.first_turn ?? d?.facts.created ?? Infinity, {
      id: sid, kind: "agent", status, name: `${n.item_id} ${d?.title ?? item?.t ?? ""}`.trim(), model: [item?.b ? st.bots[item.b]?.n ?? item.b : "", (d ? d.usage.total === 0 && d.usage.reported > 0 : n.tokens === 0 && n.reported > 0) ? t("stats:cost.quote") : ""].filter(Boolean).join(" · ") || undefined,
      duration: n.agent_ms ?? undefined, children,
      /* its figures come from its turns; without them (not loaded) its own total from the tree, which has no in/out split */
      ...(d ? {} : { cost: n.tokens > 0 ? n.est : n.reported > 0 ? n.reported : undefined, tokens: n.tokens, unpriced: n.unpriced_tokens }),
    }])
  }
  return own.sort((a, b) => a[0] - b[0]).map(([, s]) => s)
}

export function CostPanel({ d }: { d: ItemDetail }) {
  const t = useT()
  const [tree, setTree] = useState<{ nodes: TreeStat[]; truncated: boolean; details: Map<string, ItemDetail | null> } | null>(null)
  const [mode, setMode] = useState<CostMode>("cost")
  /* sub-items start folded: the item's own turns first, a sub-item opens on demand */
  const [collapsed, setCollapsed] = useState<string[] | null>(null)
  /* fetched again whenever the item's own figures move (the drill-down refreshes live), so a running item's sub-items
   * don't stand still; the last tree stays on screen meanwhile */
  useEffect(() => {
    let off = false
    stats.tree(d.item_id).then(async (tr) => {
      const kids = tr.nodes.filter((n) => n.depth > 0).slice(0, MAX_CHILDREN)
      const got = await pooled(kids, FETCH_AT_ONCE, (n) => stats.item(n.item_id).catch(() => null))
      if (!off) setTree({ nodes: tr.nodes.filter((n) => n.depth === 0 || kids.includes(n)), truncated: tr.truncated || kids.length < tr.nodes.length - 1, details: new Map(kids.map((n, i) => [n.item_id, got[i]])) })
    }).catch(() => { if (!off) setTree({ nodes: [], truncated: false, details: new Map() }) })
    return () => { off = true }
  }, [d.item_id, d.usage.total, d.usage.est, d.facts.turns, d.facts.done_at, d.facts.dropped_at])
  const refs = new Map<string, Ref>()
  const steps = stepsOf(d.item_id, d, tree?.nodes ?? [], tree?.details ?? new Map(), refs)
  const kids = tree ? tree.nodes.filter((n) => n.depth > 0).length : 0
  const open = d.facts.done_at == null && d.facts.dropped_at == null
  return (
    <Panel k="cost" icon={<CoinsIcon />} title={t("stats:cwf.title")} sub={<>{kids ? t("stats:cost.subTree", { count: kids }) : t("stats:cost.sub")}{tree?.truncated && <> · {t("stats:cost.truncated")}</>}</>}
      right={<Segmented size="sm" value={mode} onChange={setMode} options={[["cost", t("stats:cwf.cost")], ["tokens", t("stats:cwf.tokens")]]} />} data-cost-waterfall>
      <CostWaterfall bare steps={steps} mode={mode} onModeChange={setMode} status={open ? undefined : "done"} maxHeight={640}
        collapsed={collapsed ?? [...refs.keys()].filter((k) => k.startsWith("item:"))} onCollapsedChange={setCollapsed} formatCost={(n) => fmtUsd(n)} formatTokens={(n) => fmtN(n)} formatDuration={(ms) => fmtMs(ms)}
        rowProps={(s) => s.id.startsWith("turn:") ? { "data-turn": s.id.slice(5) } : s.id.startsWith("item:") ? { "data-cost-item": s.id.slice(5) } : {}}
        renderDetail={(s) => <StepDetail r={refs.get(s.id)} />} />
    </Panel>
  )
}

function Facts({ rows, foot }: { rows: [string, ReactNode][]; foot?: ReactNode }) {
  return (
    <div className="rounded-lg bg-card px-3 py-2.5 shadow-[var(--lift)]">
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 type-footnote">
        {rows.map(([k, v]) => <div key={k} className="contents"><dt className="text-muted-foreground">{k}</dt><dd className="min-w-0 tabular-nums text-foreground">{v}</dd></div>)}
      </dl>
      {foot && <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 type-footnote">{foot}</div>}
    </div>
  )
}
const link = "text-blue hover:underline"

const usd = (u: Usage) => u.total > 0 ? (u.unpriced_tokens ? t("stats:cost.unpriced", { usd: fmtUsd(u.est), n: fmtN(u.unpriced_tokens) }) : fmtUsd(u.est)) : u.reported > 0 ? t("stats:word.adapterQuoteFloor", { usd: fmtUsd(u.reported) }) : "—"
/** the adapter's own quote beside an estimate that leaves tokens out: what the unpriced part may have cost */
const quoteRow = (u: Usage): [string, ReactNode][] => (u.total > 0 && u.unpriced_tokens > 0 && u.reported > 0 ? [[t("stats:cost.quoteRow"), fmtUsd(u.reported)]] : [])
const tokenLine = (u: Usage) => t("stats:cost.tokens", { input: fmtN(u.input), output: fmtN(u.output), read: fmtN(u.cache_read), write: fmtN(u.cache_write) })

function StepDetail({ r }: { r?: Ref }) {
  const t = useT()
  if (!r) return null
  if ("turn" in r) {
    const tn = r.turn, stop = STOP()
    return <Facts rows={[
      [t("stats:cost.started"), time(tn.started)],
      [t("stats:cost.duration"), tn.ended ? fmtMs(tn.ended - tn.started) : t("stats:word.running")],
      [t("stats:cost.stop"), tn.transient ? t("stats:drill.gatewayDown") : stop[tn.stop_reason ?? ""] ?? tn.stop_reason ?? "—"],
      [t("stats:cost.tools"), tn.tool_errors ? t("stats:cost.toolsFailed", { n: tn.tools, failed: tn.tool_errors }) : t("stats:cost.toolsN", { n: tn.tools })],
      ["tokens", tokenLine(tn.usage)],
      [t("stats:cost.hit"), fmtPct(tn.usage.hit_rate)],
      [t("stats:cost.est"), usd(tn.usage)],
      ...quoteRow(tn.usage),
    ]} foot={<a href={"#/sessions/" + tn.session_id} className={link} data-turn-session>{t("stats:cost.openSession")}</a>} />
  }
  if ("other" in r) return <Facts rows={[["tokens", tokenLine(r.other)], [t("stats:cost.est"), fmtUsd(r.other.est)]]} />
  const item = st.items.find((i) => i.id === r.item), u = r.detail?.usage
  return <Facts rows={[
    [t("stats:cost.status"), <span className="inline-flex items-center gap-1.5">{item && <StatusDot s={item.st} />}{r.node?.done_at ? t("stats:drill.doneMark") : r.node?.dropped_at ? t("stats:drill.dropMark") : t("stats:word.running")}</span>],
    [t("stats:cost.agent"), fmtMs(r.node?.agent_ms)],
    ...(u ? [["tokens", tokenLine(u)] as [string, ReactNode], [t("stats:cost.est"), usd(u)] as [string, ReactNode], ...quoteRow(u)] : [["tokens", fmtN(r.node?.tokens)] as [string, ReactNode], [t("stats:cost.est"), r.failed ? t("stats:cost.loadFailed") : r.node?.unpriced_tokens ? t("stats:cost.unpriced", { usd: fmtUsd(r.node.est), n: fmtN(r.node.unpriced_tokens) }) : fmtUsd(r.node?.est)] as [string, ReactNode]]),
  ]} foot={<><a href={"#/items/" + r.item} className={link}>{t("stats:cost.openItem")}</a><a href={"#/stats/items/" + r.item} className={link}>{t("stats:cost.openStats")}</a></>} />
}
