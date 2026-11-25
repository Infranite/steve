import { getSession } from "@/store"
/* A turn's work on one timeline (affectUI Trace Waterfall): the turn as the root, under it every thinking stretch, note,
 * tool call and subtask (with its own nested child tool calls) as a bar from when it began to when it ended. Times come
 * from the session's events or adapter-reported durations. Tokens and estimated cost appear only where reported — never
 * fabricated, and never double-counted when the turn total already covers subagents. */
import { useMemo } from "react"
import { TraceWaterfall, type TraceSpan } from "@/components/affect/trace-waterfall"
import type { ToolStep, TrailItem } from "@/components/work-trail"
import { isActiveSubtaskStatus, partitionWork, subtaskOf, subtaskTimes, toolDurationMs } from "@/data/transcript-model"
import { stats } from "@/api/stats"
import { useHubQuery } from "@/data/use-hub-query"
import { fmtUsd } from "@/stats/format"
import { useT } from "@/i18n"

/* one line of plain text: Markdown's marks and code fences dropped, like the folded reply line */
const clip = (s: string, n: number) => { const x = s.replace(/```[\s\S]*?```/g, " ").replace(/[`*_>#|]/g, "").replace(/\s+/g, " ").trim(); return x.length > n ? x.slice(0, n) + "…" : x }
/* a call's input or output as it is – JSON and paths keep their underscores and asterisks – only cut to length */
const cut = (s: string, n: number) => (s.length > n ? s.slice(0, n) + "…" : s)
const text = (v: unknown) => (v == null || v === "" ? undefined : typeof v === "string" ? v : JSON.stringify(v))

function leafSpan(
  e: any[],
  j: number,
  nextAt: number | undefined,
  last: boolean,
  at: number[],
  items: (TrailItem | undefined)[],
  t0: number,
  live: boolean,
  t: ReturnType<typeof useT>,
): TraceSpan {
  const rel = (ms: number | undefined) => (ms ? Math.max(0, ms - t0) : 0)
  const start = rel(at[j])
  const it = items[j]
  if (e[0] === "th" || e[0] === "a") {
    const running = live && last
    return {
      id: "e" + j,
      kind: "llm",
      name: t(e[0] === "th" ? "core:trace.think" : "core:trace.note"),
      detail: clip(String(e[1] ?? ""), 60) || undefined,
      status: running ? "running" : "done",
      start,
      end: running ? undefined : Math.max(start, rel(nextAt) || start),
      output: clip(String(e[1] ?? ""), 300) || undefined,
      /* a note is what the model said, not a thinking stretch: the Thinking total counts only thinking, like the steps' header */
      ...(e[0] === "a" ? { uncounted: true } : {}),
    }
  }
  const s = it?.t === "tool" ? (it as ToolStep) : undefined, c = s?.card
  const running = live && e[5] === 1, failed = e[8] === 1
  const durMs = toolDurationMs(e)
  /* settled: adapter duration_ms, or the update that settled it, or until the next step began */
  const end = running
    ? undefined
    : durMs != null && durMs > 0
      ? start + durMs
      : e[13] && e[13] > at[j]
        ? rel(e[13])
        : Math.max(start, rel(nextAt) || start)
  const verb = s ? s.verb[running ? 0 : 1] : "", what = [verb, s?.subj && s.subj !== verb ? s.subj : ""].filter(Boolean).join(" ")
  return {
    id: "e" + j,
    kind: "tool",
    name: c?.name || String(e[2] ?? ""),
    icon: c?.icon,
    detail: what ? clip(what, 80) : undefined,
    status: running ? "running" : failed ? "failed" : "done",
    start,
    end,
    input: text(c?.args) && cut(text(c!.args)!, 300),
    output: failed ? undefined : text(c?.result) && cut(text(c!.result)!, 300),
    error: failed ? (c?.error ? c.error.slice(0, 300) : t("core:toolCard.failedText")) : undefined,
  }
}

/** a turn's steps as spans, with subtasks as nested `subagent` spans holding their own child tool calls */
export function turnSpans(
  evs: any[],
  at: number[],
  items: (TrailItem | undefined)[],
  t0: number,
  t1: number | undefined,
  live: boolean,
  t: ReturnType<typeof useT>,
): TraceSpan[] {
  const rel = (ms: number | undefined) => (ms ? Math.max(0, ms - t0) : 0)
  const { rootIndices, childrenBySubtask } = partitionWork(evs)
  return rootIndices.map((j, pos): TraceSpan => {
    const e = evs[j]
    const nextIdx = pos + 1 < rootIndices.length ? rootIndices[pos + 1] : undefined
    const nextAt = nextIdx != null ? at[nextIdx] : t1
    const last = pos === rootIndices.length - 1
    const sub = subtaskOf(e)
    if (!sub) return leafSpan(e, j, nextAt, last, at, items, t0, live, t)

    const { started, ended } = subtaskTimes(e)
    const start = rel(started ?? at[j])
    const running = live && isActiveSubtaskStatus(sub.status)
    const status: TraceSpan["status"] = sub.status === "failed"
      ? "failed"
      : sub.status === "cancelled"
        ? "cancelled"
        : running
          ? "running"
          : "done"
    const childIdxs = childrenBySubtask.get(sub.id) ?? []
    const children = childIdxs.map((ci, k) => {
      const cNext = k + 1 < childIdxs.length ? at[childIdxs[k + 1]] : ended ?? nextAt
      return leafSpan(evs[ci], ci, cNext, k === childIdxs.length - 1, at, items, t0, running, t)
    })
    const maxChildEnd = children.reduce((m, c) => Math.max(m, c.end ?? c.start), start)
    const end = running
      ? undefined
      : sub.duration_ms != null && sub.duration_ms > 0
        ? Math.max(start + sub.duration_ms, maxChildEnd)
        : ended && ended > (started ?? at[j] ?? 0)
          ? Math.max(rel(ended), maxChildEnd)
          : Math.max(maxChildEnd, rel(nextAt) || start)
    const u = sub.usage
    const hasIn = u && (u.input != null || u.cache_read != null || u.cache_write != null)
    const tokensIn = hasIn ? (u.input ?? 0) + (u.cache_read ?? 0) + (u.cache_write ?? 0) : undefined
    const tokensOut = u?.output != null ? u.output : undefined
    const costNum = u?.cost?.currency === "USD" ? Number(u.cost.decimal) : NaN
    const cost = Number.isFinite(costNum) && costNum > 0 ? costNum : undefined
    const originLabel = t(`core:subtask.origin.${sub.origin}`)
    const detail = [originLabel, sub.actor && sub.actor !== sub.title ? sub.actor : undefined].filter(Boolean).join(" · ")
    return {
      id: "sub:" + sub.id,
      kind: "subagent",
      name: sub.title || originLabel,
      model: sub.model ?? undefined,
      detail: detail ? clip(detail, 80) : undefined,
      status,
      start,
      end,
      ...(tokensIn != null && tokensIn > 0 ? { tokensIn } : {}),
      ...(tokensOut != null && tokensOut > 0 ? { tokensOut } : {}),
      ...(cost != null ? { cost } : {}),
      output: sub.summary ? cut(sub.summary, 300) : undefined,
      error: sub.error ? cut(sub.error, 300) : sub.status === "failed" ? t("core:toolCard.failedText") : undefined,
      ...(children.length ? { children } : {}),
    }
  })
}

const sumTree = (spans: TraceSpan[]): [number, number, number] => spans.reduce<[number, number, number]>(
  ([i, o, c], s) => {
    const [ci, co, cc] = sumTree(s.children ?? [])
    return [i + (s.tokensIn ?? 0) + ci, o + (s.tokensOut ?? 0) + co, c + (s.cost ?? 0) + cc]
  },
  [0, 0, 0],
)

export default function TurnTrace({ k, prompt, evs, at, items, t0: turnT0, t1, live, cancelled }: {
  k: string; prompt?: string; evs: any[]; at: number[]; items: (TrailItem | undefined)[]; t0?: number; t1?: number; live: boolean; cancelled: boolean
}) {
  const t = useT()
  const item = getSession(k)?.item ?? null
  /* the turn's usage: the Hub keeps it per turn for an item's sessions */
  const u = useHubQuery(["stats", "item", item ?? null, live ? 0 : t1 ?? 0], () => (item ? stats.item(item) : Promise.resolve(null)), { events: ["stats"], debounceMs: 200, live: !live })
  const t0 = turnT0 || at.find(Boolean) || Date.now()
  const turn = u.data && !live && t1 ? u.data.turns?.find((x) => x.session_id === k && Math.abs(x.started - t0) <= 5000 && x.ended != null && Math.abs(x.ended - t1) <= 5000) : undefined
  const root = useMemo<TraceSpan>(() => {
    const rel = (ms: number | undefined) => (ms ? Math.max(0, ms - t0) : 0)
    const children = turnSpans(evs, at, items, t0, t1, live, t)
    const usage = turn?.usage
    /* TraceWaterfall sums child spans onto root; subtract child figures already accounted in the turn total to avoid double-counting */
    const [childIn, childOut, childCost] = sumTree(children)
    const turnIn = usage && usage.total ? Math.max(0, usage.input + usage.cache_read + usage.cache_write - childIn) : undefined
    const turnOut = usage && usage.total ? Math.max(0, usage.output - childOut) : undefined
    const turnCost = usage && usage.est > 0 ? Math.max(0, usage.est - childCost) : undefined
    return {
      id: "turn",
      kind: "agent",
      name: t("core:trace.turn"),
      input: prompt ? clip(prompt, 120) : undefined,
      status: live ? "running" : cancelled ? "cancelled" : "done",
      start: 0,
      end: live ? undefined : Math.max(rel(t1), ...children.map((c) => c.end ?? c.start)),
      ...(turnIn ? { tokensIn: turnIn } : {}),
      ...(turnOut ? { tokensOut: turnOut } : {}),
      ...(turnCost ? { cost: turnCost } : {}),
      children,
    }
  }, [evs, at, items, t0, t1, live, cancelled, turn, prompt, t])
  return <div className="my-1" data-turn-trace={live ? "live" : cancelled ? "cancelled" : "done"}><TraceWaterfall root={root} startedAt={live ? t0 : undefined} formatCost={fmtUsd} /></div>
}
