/* What the session page says about a session, worked out from what the Hub already reports: its turns (from the
 * transcript's events), what it delivered and the files it changed. Nothing here guesses: a figure the Hub does not
 * report stays unknown. Kept free of React and of views so node --test can load it. */
/** a transcript tuple of store.ts (`Ev`) */
type Ev = any[]

/** a turn's tokens and estimated cost, as the Hub's per-turn stats (`/stats/items/:id` turns) report them */
export type TurnUsage = { input: number; output: number; cache_read: number; cache_write: number; total: number; est: number; reported: number; unpriced_tokens: number }
/** one turn of a session: from the prompt that opened it to the end the harness reported */
export type Turn = {
  /** 1-based, in order */ n: number
  /** where it starts in the session's tuples */ idx: number
  /** the words that opened it: a message, Steve's brief, merged queued messages; "" when the work began without one */
  prompt: string
  /** what opened it: the user ("u"), Steve's handoff ("hi"), merged queued messages ("q") or nothing ("") */
  by: "u" | "hi" | "q" | ""
  start: number; end?: number
  /** still running (the session runs and no end came yet) */ live: boolean
  tools: number; fails: number
  /** the stop reason the harness gave ("end_turn", "cancelled"…) */ stop?: string
}
const PROMPT = ["u", "hi", "q"]
/** one line of plain text: Markdown marks and code fences dropped */
export const plain = (s: string, n = 140) => { const x = s.replace(/```[\s\S]*?```/g, " ").replace(/[`*_>#|]/g, "").replace(/\s+/g, " ").trim(); return x.length > n ? x.slice(0, n) + "…" : x }
const promptOf = (e: Ev, head: (s: string) => string) =>
  e[0] === "u" ? plain(String(e[1] ?? "")) : e[0] === "hi" ? head(String(e[3] ?? e[1] ?? "")) : e[0] === "q" ? plain((e[1] as string[]).join(" / ")) : ""

/** the session's turns from its tuples (`ev`, with `ts` the time of each). A prompt opens a turn unless one is open (a
 *  message sent into a running turn steers it); the harness's end closes it. Work with no prompt before it opens a turn
 *  of its own. `head` shortens Steve's long briefs to their first line. */
export function turnsOf(ev: Ev[], ts: number[], running: boolean, head: (s: string) => string = (s) => plain(s), /** an imported transcript carries
  no ends: a prompt after the work opens the next turn */ endless = false): Turn[] {
  const out: Turn[] = []
  let cur: Turn | null = null
  ev.forEach((e, idx) => {
    const k = e[0], at = ts[idx] ?? 0
    if (PROMPT.includes(k)) {
      if (cur && endless && (cur.tools || ev[idx - 1]?.[0] === "a")) cur = null
      if (!cur) { cur = { n: out.length + 1, idx, prompt: promptOf(e, head), by: k, start: at, live: false, tools: 0, fails: 0 }; out.push(cur) }
      return
    }
    if (k === "th" || k === "tool" || k === "a" || k === "ask") {
      if (!cur) { cur = { n: out.length + 1, idx, prompt: "", by: "", start: at, live: false, tools: 0, fails: 0 }; out.push(cur) }
      if (k === "tool") { cur.tools++; if (e[8] === 1) cur.fails++ }
      return
    }
    if (k === "end") {
      if (cur) { cur.end = at || undefined; cur.stop = String(e[1] ?? "") || undefined; cur = null }
    }
  })
  if (cur && running) (cur as Turn).live = true
  return out
}

/** the files the session changed, from its edit calls that succeeded: path, lines added, lines removed; most changed first */
export function filesOf(ev: Ev[]): [string, number, number][] {
  const files = new Map<string, [string, number, number]>()
  for (const e of ev) if (e[0] === "tool" && e[1] === "edit" && e[6] != null && e[8] !== 1 && e[3]) {
    const f = files.get(e[3]) ?? [e[3], 0, 0]; f[1] += Number(e[6]) || 0; f[2] += Number(e[7]) || 0; files.set(e[3], f)
  }
  return [...files.values()].sort((a, b) => b[1] + b[2] - (a[1] + a[2]))
}

type TimelineLine = { ts: number; session_id: string | null }
type Artifact = { id: string; created: number }
/** the item's outputs this session delivered. A delivery writes its outputs and a line in the item's timeline at the
 *  same moment, and that line names the session; outputs whose moment has no line naming a session belong to the
 *  session only when it is the item's only one. */
export function outputsOf<A extends Artifact>(sid: string, artifacts: A[], timeline: TimelineLine[], sessions: string[]): A[] {
  const near = (a: A) => timeline.filter((l) => l.session_id && Math.abs(l.ts - a.created) <= 2000)
  return artifacts.filter((a) => { const ls = near(a); return ls.length ? ls.some((l) => l.session_id === sid) : sessions.length === 1 && sessions[0] === sid })
}

/** time worked: the turns' own lengths added up (a live one up to `now`), never the gaps between them */
export const workedMs = (turns: Turn[], now: number) => turns.reduce((n, t) => n + Math.max(0, (t.end ?? (t.live ? now : t.start)) - t.start), 0)
