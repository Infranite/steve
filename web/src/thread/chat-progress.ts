import type {
  ThreadLiveSnapshot,
  ThreadPlanEntry,
  ThreadResearchStage,
  ThreadSubtask,
  WorkLiveEvent,
} from "@/gen"

/** Show a quiet secondary "last activity X ago" hint only after this many silent seconds, without replacing total elapsed time. */
export const SILENCE_HINT_THRESHOLD_S = 15

/** The main thread's bounded view of actual tool events. A result belongs only to its own call. */
export type ResearchCall = {
  name: string
  brief: string
  description?: string
  kind?: string
  call?: number
  startedAt?: number
  at?: number
  ms?: number
  excerpt?: string
  error?: string
}

export type LiveThought = {
  id: string
  text: string
  startedAt: number
  at?: number
  done: boolean
}

export type LiveRun = {
  mid: string
  run: string
  startedAt?: number
  at?: number
  stage?: ThreadResearchStage
  tools: (ResearchCall & { ok: boolean })[]
  now?: ResearchCall
  active?: ResearchCall[]
  plan?: ThreadPlanEntry[]
  thoughts?: LiveThought[]
  subtasks?: ThreadSubtask[]
}

export type ResearchEvent = ResearchCall & {
  type: "steve.tool"
  ws: string
  mid: string
  run: string
  ok: boolean
  running?: boolean
  started_at?: number
  at?: number
  step_started_at?: number
  description?: string
  ms?: number
  excerpt?: string
  error?: string
}

export type ResearchProgressEvent = Extract<WorkLiveEvent, { type: "steve.progress" }>

const TOOL_STAGE: Record<string, ThreadResearchStage> = {
  read: "reading",
  search: "searching",
  exec: "executing",
  edit: "editing",
  delete: "editing",
  move: "editing",
  fetch: "fetching",
  think: "thinking",
}

function emptyRun(mid: string, run: string): LiveRun {
  return { mid, run, tools: [], plan: [], thoughts: [], subtasks: [] }
}

function sameRun(previous: LiveRun | null, mid: string, run: string): LiveRun {
  if (previous?.run === run && previous.mid === mid) return { ...previous }
  return emptyRun(mid, run)
}

export function researchEvent(previous: LiveRun | null, e: ResearchEvent): LiveRun {
  const l = sameRun(previous, e.mid, e.run)
  const existingActive = e.call == null ? l.now : (l.active ?? []).find((x) => x.call === e.call)
  const call: ResearchCall = {
    call: e.call,
    name: e.name,
    brief: e.brief,
    description: e.description || existingActive?.description,
    kind: e.kind,
    startedAt: e.step_started_at ?? existingActive?.startedAt ?? (e.running ? e.at : undefined),
    at: e.at,
    ms: e.ms,
    excerpt: e.excerpt,
    error: e.error,
  }
  l.startedAt = e.started_at ?? l.startedAt
  // A frame with no usable timestamp cannot tell how much time passed.
  if (e.at != null) l.at = Math.max(l.at ?? 0, e.at)
  if (e.running) {
    l.stage = TOOL_STAGE[e.kind ?? ""] ?? "calling_tool"
  }
  if (e.call == null) {
    // A tool that reported no call identity has one activity slot.
    l.now = e.running ? call : undefined
  } else {
    const others = (l.active ?? []).filter((x) => x.call !== e.call)
    l.active = e.running ? [...others, call] : others
    l.now = l.active.at(-1)
  }
  if (!e.running) {
    const last = e.call == null ? undefined : l.tools.findLast((t) => t.call === e.call)
    if (!last || last.ok !== e.ok) l.tools = [...l.tools, { ...call, ok: e.ok }]
    if (l.now) {
      l.stage = TOOL_STAGE[l.now.kind ?? ""] ?? "calling_tool"
    } else if ((l.thoughts ?? []).some((th) => !th.done)) {
      l.stage = "thinking"
    } else {
      l.stage = "responding"
    }
  }
  return l
}

export function researchProgressEvent(previous: LiveRun | null, e: ResearchProgressEvent): LiveRun {
  const l = sameRun(previous, e.mid, e.run)
  l.startedAt = e.started_at ?? l.startedAt
  if (e.at != null) l.at = Math.max(l.at ?? 0, e.at)
  const body = e.body
  if (body.kind === "stage") {
    l.stage = body.stage
  } else if (body.kind === "thought") {
    const list = [...(l.thoughts ?? [])]
    const idx = list.findIndex((th) => th.id === body.id)
    const prev = idx >= 0 ? list[idx] : undefined
    const next: LiveThought = {
      id: body.id,
      text: body.text,
      startedAt: prev?.startedAt ?? e.at ?? Date.now(),
      at: e.at ?? prev?.at,
      done: !!body.done,
    }
    if (idx >= 0) list[idx] = next
    else list.push(next)
    l.thoughts = list
    if (!body.done) {
      l.stage = "thinking"
    } else if (!l.now && !list.some((th) => !th.done) && l.stage === "thinking") {
      l.stage = "responding"
    }
  } else if (body.kind === "plan") {
    l.plan = body.entries
  } else if (body.kind === "subtask") {
    const list = [...(l.subtasks ?? [])]
    const idx = list.findIndex((s) => s.id === body.subtask.id)
    if (idx >= 0) list[idx] = body.subtask
    else list.push(body.subtask)
    l.subtasks = list
  }
  return l
}

/** Whether a `/api/summary` live snapshot carries a research run (vs. triage/direct answer/config). */
export function isResearchSnapshot(snap: ThreadLiveSnapshot): boolean {
  return snap.phase === "research"
    || (snap.tools?.length ?? 0) > 0
    || (snap.plan?.length ?? 0) > 0
    || (snap.thoughts?.length ?? 0) > 0
    || (snap.subtasks?.length ?? 0) > 0
}

export function liveRunFromSnapshot(snap: ThreadLiveSnapshot): LiveRun {
  const all = snap.tools ?? []
  const tools = all
    .filter((t) => !t.running)
    .map((t) => ({
      call: t.call,
      name: t.name,
      brief: t.brief ?? "",
      description: t.description || undefined,
      kind: t.kind,
      startedAt: t.started_at,
      at: t.at,
      ms: t.ms,
      excerpt: t.excerpt,
      error: t.error,
      ok: t.ok,
    }))
  const active: ResearchCall[] = all
    .filter((t) => t.running)
    .map((t) => ({
      call: t.call,
      name: t.name,
      brief: t.brief ?? "",
      description: t.description || undefined,
      kind: t.kind,
      startedAt: t.started_at,
      at: t.at,
    }))
  return {
    mid: snap.mid ?? snap.run,
    run: snap.run,
    startedAt: snap.started_at,
    at: snap.last_activity_at,
    stage: snap.stage,
    plan: snap.plan ?? [],
    thoughts: (snap.thoughts ?? []).map((th) => ({
      id: th.id,
      text: th.text,
      startedAt: th.started_at,
      at: th.at,
      done: !!th.done,
    })),
    subtasks: snap.subtasks ?? [],
    tools,
    active,
    now: active.at(-1),
  }
}

/** Only a reported success for the same call can clear that earlier failure. A later successful
 * unrelated call or reply, even with the same tool and arguments, says nothing about recovery. */
export function recoveredCalls<T extends { call?: number }>(rows: T[], ok: (row: T) => boolean): Set<number> {
  const next = new Map<number, boolean>(), recovered = new Set<number>()
  for (let n = rows.length - 1; n >= 0; n--) {
    const row = rows[n]
    if (row.call == null) continue
    const success = ok(row)
    if (!success && next.get(row.call) === true) recovered.add(n)
    next.set(row.call, success)
  }
  return recovered
}
