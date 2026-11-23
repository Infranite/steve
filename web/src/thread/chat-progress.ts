/** The main thread's bounded view of actual tool events. A result belongs only to its own call. */
export type ResearchCall = { name: string; brief: string; kind?: string; call?: number }
export type LiveRun = {
  mid: string; run: string; startedAt?: number; at?: number
  tools: (ResearchCall & { ok: boolean })[]
  now?: ResearchCall
  active?: ResearchCall[]
}
export type ResearchEvent = ResearchCall & {
  type: "steve.tool"; ws: string; mid: string; run: string; ok: boolean; running?: boolean; started_at?: number; at?: number
}

export function researchEvent(previous: LiveRun | null, e: ResearchEvent): LiveRun {
  const l: LiveRun = previous?.run === e.run && previous.mid === e.mid ? previous : { mid: e.mid, run: e.run, tools: [] }
  const call = { call: e.call, name: e.name, brief: e.brief, kind: e.kind }
  l.startedAt = e.started_at ?? l.startedAt
  // A frame with no usable timestamp cannot tell how much time passed.
  if (e.at != null) l.at = Math.max(l.at ?? 0, e.at)
  if (e.call == null) {
    // A tool that reported no call identity has one activity slot.
    l.now = e.running ? call : undefined
  } else {
    const others = (l.active ?? []).filter(x => x.call !== e.call)
    l.active = e.running ? [...others, call] : others
    l.now = l.active.at(-1)
  }
  if (!e.running) {
    const last = e.call == null ? undefined : l.tools.findLast(t => t.call === e.call)
    if (!last || last.ok !== e.ok) l.tools.push({ ...call, ok: e.ok })
  }
  return l
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
