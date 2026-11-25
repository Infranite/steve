/* A page starts at a user prompt. Each turn keeps its mapped rows, so earlier pages and
 * completed turns keep their identities as live events arrive. */
import type { REvent, RSubtaskStatus, RSubtaskView } from "../api"
import i18n, { t } from "../i18n"

export type Ev = any[]
export const toolId = (e: Ev): string => (e[0] === "tool" && typeof e[15] === "string" ? e[15] : "")
export const toolParent = (e: Ev): string | undefined => (e[0] === "tool" && typeof e[16] === "string" && e[16] ? e[16] : undefined)
export const toolDurationMs = (e: Ev): number | undefined => (e[0] === "tool" && typeof e[17] === "number" && Number.isFinite(e[17]) ? e[17] : undefined)
export const isSubtaskEv = (e: Ev): boolean => e[0] === "sub" && !!e[1]
export const subtaskOf = (e: Ev): RSubtaskView | undefined => (e[0] === "sub" ? (e[1] as RSubtaskView) : undefined)
export const subtaskTimes = (e: Ev): { started?: number; ended?: number } => (e[0] === "sub"
  ? { started: typeof e[2] === "number" ? e[2] : undefined, ended: typeof e[3] === "number" ? e[3] : undefined }
  : {})
export const isActiveSubtaskStatus = (s: RSubtaskStatus | string | undefined): boolean => s === "running" || s === "queued" || s === "needs_you"
/** one line for a prompt Steve hands an agent (the task brief runs to pages: role, rules, retrieved knowledge) */
export function briefHead(text: string): string {
  const title = /^You are working for Steve\b[^\n]*?“(.+?)”/.exec(text)?.[1]
  if (title) return t("core:brief.titled", { title })
  const line = text.split("\n").find((l) => l.trim())?.trim() ?? ""
  return !line ? t("core:brief.plain") : line.length > 120 ? line.slice(0, 120) + "…" : line
}
const KIND: Record<string, string> = { read: "read", edit: "edit", search: "search", exec: "run", fetch: "search", think: "read", delete: "edit", move: "edit" }
const unfence = (t: string) => t.replace(/^```[a-z]*\n?/, "").replace(/\n?```\s*$/, "")
const diffText = (d: any) => [...String(d.old ?? "").split("\n").filter((l, _i, a) => a.length > 1 || l).map((l) => "- " + l), ...String(d.new ?? "").split("\n").map((l) => "+ " + l)].join("\n")
function toolName(name: string) {
  const label = i18n.getResource(i18n.language, "core", "steveTool." + name)
  return typeof label === "string" ? label : name
}

/** a tool's whole current view (transcript `tool`, the Node protocol's ToolView) → its transcript tuple, in place */
function toolEv(t: Ev, v: any) {
  const name = String(v.name ?? ""), short = name.replace(/^mcp__steve__/, ""), title = String(v.title ?? "")
  const steve = v.origin === "steve" || v.category === "steve", live = v.phase === "succeeded" || v.phase === "failed" ? 0 : 1
  const args = v.action?.type === "tool" && v.action.args && typeof v.action.args === "object" ? v.action.args : undefined
  const result: any[] = Array.isArray(v.result) ? v.result : [], diffs = result.filter((p) => p?.type === "diff")
  const text = result.filter((p) => p?.type === "text").map((p) => String(p.text ?? "")).join("\n")
  t.length = 0
  if (steve) t.push("tool", "steve", toolName(short), "Steve", "", live)
  else t.push("tool", KIND[v.category] ?? "run", name || title.split(" ")[0] || i18n.t("core:tool.fallback"), v.target ?? title.split(" ").slice(1).join(" ").slice(0, 80), "", live)
  if (diffs.length) { t[4] = diffs.map(diffText).join("\n⋯\n"); t[6] = diffs.reduce((n, d) => n + (d.added ?? 0), 0); t[7] = diffs.reduce((n, d) => n + (d.removed ?? 0), 0); if (!steve && diffs[0].path) t[3] = diffs[0].path }
  else if (text) t[4] = t[1] === "run" ? unfence(text) : text
  if (!t[4] && v.error) t[4] = String(v.error)
  t[8] = v.phase === "failed" ? 1 : 0; t[9] = name
  const a = v.action
  if (a?.type === "exec") t[11] = a.command; else if (a?.type === "search") t[11] = a.query; else if (a?.type === "fetch") t[11] = a.url
  else if (t[1] === "run" && v.target) t[11] = v.target
  if (!steve && v.description) t[10] = v.description
  if (args) {
    if (typeof args.now === "string") t[10] = args.now
    if (Array.isArray(args.plan)) t[12] = args.plan.map((x: any) => [String(x.step ?? ""), !!x.done])
    if (typeof args.summary === "string") t[10] = args.summary.split("\n")[0].slice(0, 160)
    if (typeof args.question === "string") t[10] = args.question
    /* [14]: what went into one of Steve's own tools, as the Hub stored it, for the call's card and timeline */
    if (steve && Object.keys(args).length) t[14] = args
  }
  /* [15]: call id, [16]: parent subtask id when nested, [17]: adapter-observed duration in ms */
  t[15] = String(v.id ?? "")
  if (typeof v.parent === "string" && v.parent) t[16] = v.parent
  if (typeof v.duration_ms === "number" && Number.isFinite(v.duration_ms)) t[17] = v.duration_ms
  return t
}
function tuplesOf(evs: readonly REvent[]) {
  const ev: Ev[] = [], seqs: number[] = [], ends: number[] = [], ts: number[] = [], tools = new Map<string, Ev>(), subtasks = new Map<string, Ev>()
  let cur: REvent | undefined
  const push = (e: Ev, seq: number) => { ev.push(e); seqs.push(seq); ends.push(seq); ts.push(cur?.ts ?? 0) }
  for (const e of evs) {
    const d = e.data ?? {}, last = ev[ev.length - 1]; cur = e
    if (e.kind === "user") { if (d.via === "steve") push(["hi", briefHead(String(d.text ?? "")), d.files, String(d.text ?? "")], e.seq); else push(["u", d.text, d.via === "me" ? "" : d.via, d.files], e.seq); continue }
    if (e.kind === "text") {
      const text = String(d.text ?? ""), k = d.role === "thought" ? "th" : "a"
      if (last?.[0] === k) { last[1] += text; ends[ends.length - 1] = e.seq }
      else push(k === "th" ? ["th", text, ""] : ["a", text], e.seq)
      continue
    }
    if (e.kind === "tool") {
      const v = d.tool ?? {}, id = String(v.id ?? ""), had = tools.get(id), over = v.phase === "succeeded" || v.phase === "failed"
      /* [13]: when the call ended (the update that settled it), so a turn's timeline can draw each call as long as it took */
      if (had) { const ended = had[13]; toolEv(had, v); if (ended != null || over) had[13] = ended ?? e.ts; continue }
      const t = toolEv([], v); if (over) t[13] = e.ts; tools.set(id, t); push(t, e.seq); continue
    }
    if (e.kind === "subtask") {
      const v = d.subtask as RSubtaskView | undefined
      if (!v || !v.id) continue
      const id = String(v.id), over = v.status === "done" || v.status === "failed" || v.status === "cancelled"
      const had = subtasks.get(id)
      if (had) {
        const started = had[2], ended = had[3]
        had[1] = v
        had[2] = started ?? e.ts
        if (ended != null || over) had[3] = ended ?? e.ts
        continue
      }
      const sub: Ev = ["sub", v, e.ts, over ? e.ts : undefined]
      subtasks.set(id, sub); push(sub, e.seq); continue
    }
    if (e.kind === "system") { push(d.faulted ? ["sys", d.text, "faulted"] : ["sys", d.text], e.seq); continue }
    if (e.kind === "permission") { push(["ask", String(d.title ?? t("core:tool.permission")), d.decision], e.seq); continue }
    if (e.kind === "turn_end") { push(["end", d.stopReason ?? ""], e.seq); continue }
    if (e.kind === "queued") { push(["sys", t("core:queueLine.queued", { text: briefHead(String(d.text ?? "")) })], e.seq); continue }
    if (e.kind === "queued_edited") { push(["sys", t("core:queueLine.edited", { text: briefHead(String(d.text ?? "")) })], e.seq); continue }
    if (e.kind === "queued_removed") { push(["sys", t("core:queueLine.removed", { text: briefHead(String(d.text ?? "")) })], e.seq); continue }
    if (e.kind === "queued_sent_now") { push(["sys", t(d.interrupted ? "core:queueLine.sentNowInterrupted" : "core:queueLine.sentNow", { text: briefHead(String(d.text ?? "")) })], e.seq); continue }
  }
  return { ev, seqs, ends, ts }
}
export type Seg = { id: string; t: string; evs: Ev[]; seq: number; through: number; prompt?: string; /** when each of `evs` came */ at?: number[]; t0?: number; t1?: number; end?: string; files?: [string, number, number][] }

const PROMPT = ["u", "hi"]
export function segmentsOf(tuple: ReturnType<typeof tuplesOf>, rangeStart = 0): Seg[] {
  const { ev, seqs, ends, ts } = tuple
  const rows: Seg[] = []
  let buf: number[] = [], t0 = 0, work: Seg | undefined, prompt: string | undefined
  let files = new Map<string, [string, number, number]>()
  const row = (at: number, kind: string, evs: Ev[], more?: Partial<Seg>) => {
    const seq = seqs[at]
    const out = { id: seq + ":" + kind, t: kind, evs, seq, through: ends[at], prompt, ...more }
    rows.push(out)
    return out
  }
  const flush = (end?: [string, number]) => {
    if (!buf.length) return
    const answer = ev[buf.at(-1)!][0] === "a" ? buf.pop() : undefined
    if (buf.length) {
      const first = buf[0], last = buf.at(-1)!
      work = row(first, "work", buf.map((i) => ev[i]), {
        at: buf.map((i) => ts[i]), t0: t0 || ts[first], t1: end?.[1] ?? ts[last + 1] ?? ts[last], end: end?.[0],
      })
      for (const i of buf) {
        const e = ev[i]
        if (e[0] !== "tool" || e[1] !== "edit" || e[6] == null || e[8] === 1 || !e[3]) continue
        const f = files.get(e[3]) ?? [e[3], 0, 0]
        f[1] += e[6]; f[2] += e[7] ?? 0; files.set(e[3], f)
      }
    }
    if (answer !== undefined) row(answer, "a", [ev[answer]])
    buf = []
  }
  const turnDone = (i: number) => {
    if (files.size) row(i, "files", [], { files: [...files.values()] })
    files = new Map()
  }
  for (let i = 0; i < ev.length; i++) {
    const e = ev[i], kind = e[0]
    if (PROMPT.includes(kind)) {
      flush(); turnDone(i); t0 = ts[i]; work = undefined
      prompt = kind === "hi" ? briefHead(String(e[3] ?? e[1])) : String(e[1] ?? "")
      if (i >= rangeStart) row(i, kind, [e])
    } else if (i < rangeStart) continue
    else if (kind === "th" || kind === "tool" || kind === "sub" || kind === "a") buf.push(i)
    else if (kind === "end") {
      if (!buf.length && work && !work.end) Object.assign(work, { end: e[1], t1: ts[i] })
      flush([e[1], ts[i]]); turnDone(i); t0 = 0; work = undefined
    } else {
      const split = buf.length > 0
      flush(); row(i, kind, [e]); if (split) t0 = 0
    }
  }
  flush()
  turnDone(ev.length - 1)
  return rows
}

export type TranscriptTurn = ReturnType<typeof mapTurn>
function mapTurn(events: REvent[]) {
  const tuples = tuplesOf(events)
  const start = events[0].seq
  return { start, events, ...tuples, rows: segmentsOf(tuples) }
}

export type Transcript = { turns: TranscriptTurn[]; oldest: number | null; newest: number }

export type ActiveSubtask = { subtask: RSubtaskView; started?: number; childSteps: number }
/** Active (running/queued/needs_you) subtasks in the latest turn, for the header's RunningNow capsule. */
export function activeSubtasksOf(transcript: Transcript | undefined): ActiveSubtask[] {
  if (!transcript?.turns.length) return []
  const latest = transcript.turns.at(-1)!
  const childCounts = new Map<string, number>()
  for (const e of latest.ev) {
    const p = toolParent(e)
    if (p) childCounts.set(p, (childCounts.get(p) ?? 0) + 1)
  }
  const out: ActiveSubtask[] = []
  for (const e of latest.ev) {
    const s = subtaskOf(e)
    if (s && isActiveSubtaskStatus(s.status)) {
      const { started } = subtaskTimes(e)
      out.push({ subtask: s, started, childSteps: childCounts.get(s.id) ?? 0 })
    }
  }
  return out
}

export type WorkPartition = {
  /** Top-level event indices (omitting child tools nested under a known subtask and raw launcher tools duplicated by a subtask with the same id). */
  rootIndices: number[]
  /** Subtask ids present in this work segment. */
  subtaskIds: Set<string>
  /** Child tool event indices grouped by their parent subtask id. */
  childrenBySubtask: Map<string, number[]>
}

/** Separate top-level work steps from child tool calls nested under a subtask, without duplicating launcher tool rows. */
export function partitionWork(evs: readonly Ev[]): WorkPartition {
  const subtaskIds = new Set<string>()
  for (const e of evs) {
    const s = subtaskOf(e)
    if (s) subtaskIds.add(s.id)
  }
  const rootIndices: number[] = []
  const childrenBySubtask = new Map<string, number[]>()
  for (let i = 0; i < evs.length; i++) {
    const e = evs[i]
    if (e[0] === "tool") {
      const id = toolId(e)
      if (id && subtaskIds.has(id)) continue
      const parent = toolParent(e)
      if (parent && subtaskIds.has(parent)) {
        const list = childrenBySubtask.get(parent)
        if (list) list.push(i)
        else childrenBySubtask.set(parent, [i])
        continue
      }
    }
    rootIndices.push(i)
  }
  return { rootIndices, subtaskIds, childrenBySubtask }
}

export function splitTurns(events: readonly REvent[], lastKind?: string): REvent[][] {
  const turns: REvent[][] = [[]]
  for (const event of events) {
    if (event.kind === "user" && lastKind && lastKind !== "user") turns.push([])
    turns.at(-1)!.push(event)
    lastKind = event.kind
  }
  return turns
}
export const mapTurns = (events: readonly REvent[]) => splitTurns(events).filter((turn) => turn.length).map(mapTurn)

export function appendEvents(data: Transcript, events: readonly REvent[]): Transcript {
  const fresh = events.filter((e) => e.seq > data.newest)
  if (!fresh.length) return data
  const last = data.turns.at(-1)
  const [continuation, ...newTurns] = splitTurns(fresh, last?.events.at(-1)?.kind)
  const turns = data.turns.slice()
  if (continuation.length) {
    if (last) turns.pop()
    turns.push(mapTurn([...(last?.events ?? []), ...continuation]))
  }
  for (const turn of newTurns) turns.push(mapTurn(turn))
  return { ...data, newest: fresh.at(-1)!.seq, turns }
}

/** Fetch the history of one fixed snapshot in independent cursor ranges, including large ranges that span several pages. */
export async function allEvents(
  marks: number[],
  read: (after: number, through: number) => Promise<{ events: REvent[]; next: number | null }>,
): Promise<REvent[]> {
  const chunks: REvent[][] = []
  let job = 0
  await Promise.all(Array.from({ length: Math.min(4, marks.length) }, async () => {
    while (job < marks.length) {
      const i = job++, through = marks[i], events: REvent[] = []
      let after: number | null = marks[i - 1] ?? 0
      while (after !== null) {
        const page = await read(after, through)
        for (const event of page.events) events.push(event)
        after = page.next
      }
      chunks[i] = events
    }
  }))
  return chunks.flat()
}
