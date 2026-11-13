/* A turn's work on one timeline (affectUI Trace Waterfall): the turn as the root, under it every thinking stretch, note
 * and tool call as a bar from when it began to when it ended. Times come from the session's events: a call runs from
 * the update that started it to the one that settled it, thinking and notes until the next step began. Tokens and the
 * estimated cost are the turn's own, from the Hub's stats, and only when the Hub has them for exactly this turn (an
 * item's session); Steve has no per-call usage, so the bars carry none. A turn that was interrupted says so, never "Done". */
import { useMemo } from "react"
import { TraceWaterfall, type TraceSpan } from "@/components/affect/trace-waterfall"
import type { ToolStep, TrailItem } from "@/components/work-trail"
import { stats } from "@/api/stats"
import { fmtUsd, useStats } from "@/stats/format"
import { st, briefHead } from "@/store"
import { useT } from "@/i18n"

const PROMPT = ["u", "hi", "q"]
/* one line of plain text: Markdown's marks and code fences dropped, like the folded reply line */
const clip = (s: string, n: number) => { const x = s.replace(/```[\s\S]*?```/g, " ").replace(/[`*_>#|]/g, "").replace(/\s+/g, " ").trim(); return x.length > n ? x.slice(0, n) + "…" : x }
/* a call's input or output as it is – JSON and paths keep their underscores and asterisks – only cut to length */
const cut = (s: string, n: number) => (s.length > n ? s.slice(0, n) + "…" : s)
const text = (v: unknown) => (v == null || v === "" ? undefined : typeof v === "string" ? v : JSON.stringify(v))
/** the words the turn was asked: the prompt before its first step */
function promptOf(k: string, first: any[]): string | undefined {
  const s = st.S[k]
  if (!s) return undefined
  const at = s.ev.indexOf(first)
  for (let i = at - 1; i >= 0; i--) {
    const e = s.ev[i]
    if (e[0] === "u") return clip(String(e[1] ?? ""), 120)
    if (e[0] === "hi") return briefHead(String(e[3] ?? e[1] ?? ""))
    if (e[0] === "q") return clip((e[1] as string[]).join(" / "), 120)
    if (e[0] === "end" || PROMPT.includes(e[0])) break
  }
  return undefined
}

export default function TurnTrace({ k, evs, at, items, t0: turnT0, t1, live, cancelled }: {
  k: string; evs: any[]; at: number[]; items: TrailItem[]; t0?: number; t1?: number; live: boolean; cancelled: boolean
}) {
  const t = useT()
  const item = st.S[k]?.item ?? null
  /* the turn's usage: the Hub keeps it per turn for an item's sessions */
  const [u] = useStats(() => (item ? stats.item(item) : Promise.resolve(null)), [item, live ? 0 : t1], !live)
  const t0 = turnT0 || at.find(Boolean) || Date.now()
  const turn = u.s === "ok" && u.v && !live && t1 ? u.v.turns?.find((x) => x.session_id === k && Math.abs(x.started - t0) <= 5000 && x.ended != null && Math.abs(x.ended - t1) <= 5000) : undefined
  const root = useMemo<TraceSpan>(() => {
    const rel = (ms: number | undefined) => (ms ? Math.max(0, ms - t0) : 0)
    const children = evs.map((e, j): TraceSpan => {
      const start = rel(at[j]), next = j + 1 < evs.length ? at[j + 1] : t1
      const last = j === evs.length - 1, it = items[j]
      if (e[0] === "th" || e[0] === "a") {
        const running = live && last
        return { id: "e" + j, kind: "llm", name: t(e[0] === "th" ? "core:trace.think" : "core:trace.note"), detail: clip(String(e[1] ?? ""), 60) || undefined,
          status: running ? "running" : "done", start, end: running ? undefined : Math.max(start, rel(next) || start), output: clip(String(e[1] ?? ""), 300) || undefined,
          /* a note is what the model said, not a thinking stretch: the Thinking total counts only thinking, like the steps' header */
          ...(e[0] === "a" ? { uncounted: true } : {}) }
      }
      const s = it?.t === "tool" ? (it as ToolStep) : undefined, c = s?.card
      const running = live && e[5] === 1, failed = e[8] === 1
      /* settled: the update that settled it; never settled (a turn cut off) or started and settled in one update: until the next step began */
      const end = running ? undefined : e[13] && e[13] > at[j] ? rel(e[13]) : Math.max(start, rel(next) || start)
      /* the detail says what the steps say: Steve's verb and its subject ("读取 workflows.rs", "读事项") */
      const verb = s ? s.verb[running ? 0 : 1] : "", what = [verb, s?.subj && s.subj !== verb ? s.subj : ""].filter(Boolean).join(" ")
      return { id: "e" + j, kind: e[9] === "Task" || e[9] === "Agent" ? "subagent" : "tool", name: c?.name || String(e[2] ?? ""), detail: what ? clip(what, 80) : undefined,
        status: running ? "running" : failed ? "failed" : "done", start, end,
        input: text(c?.args) && cut(text(c!.args)!, 300), output: failed ? undefined : text(c?.result) && cut(text(c!.result)!, 300), error: failed ? (c?.error ? c.error.slice(0, 300) : t("core:toolCard.failedText")) : undefined }
    })
    const usage = turn?.usage
    return { id: "turn", kind: "agent", name: t("core:trace.turn"), input: promptOf(k, evs[0]), status: live ? "running" : cancelled ? "cancelled" : "done", start: 0,
      end: live ? undefined : Math.max(rel(t1), ...children.map((c) => c.end ?? c.start)),
      ...(usage && usage.total ? { tokensIn: usage.input + usage.cache_read + usage.cache_write, tokensOut: usage.output, ...(usage.est > 0 ? { cost: usage.est } : {}) } : {}),
      children }
  }, [evs, at, items, t0, t1, live, cancelled, turn, k, t])
  return <div className="my-1" data-turn-trace={live ? "live" : cancelled ? "cancelled" : "done"}><TraceWaterfall root={root} startedAt={live ? t0 : undefined} formatCost={fmtUsd} /></div>
}
