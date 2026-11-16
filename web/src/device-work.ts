import type { RSession, RDecision } from "./api.ts"
export type DeviceWork = { session: RSession; decisions: RDecision[]; queue: RSession["queued"] }
/** Exact inventory identity only; missing device attribution never falls back to the local Node. */
export function groupDeviceWork(device: string, sessions: RSession[], decisions: RDecision[], closedItems: Set<string> = new Set()) {
  const unique = [...new Map(sessions.map(s => [s.id, s])).values()]
  const open = [...new Map(decisions.filter(d => d.status === "open").map(d => [d.id, d])).values()]
  const rows = unique.filter(s => s.device_id === device).map(session => ({ session,
    decisions: open.filter(d => d.session_id === session.id),
    queue: [...new Map(session.queued.filter(q => !q.session_id || q.session_id === session.id).map(q => [q.id, q])).values()],
  }))
  const attention: DeviceWork[] = [], current: DeviceWork[] = [], recent: DeviceWork[] = []
  for (const row of rows) {
    const s = row.session
    if (s.readonly) recent.push(row)
    else if (row.decisions.length) attention.push(row)
    else if (s.status === "error" && s.item_id && closedItems.has(s.item_id) && !row.queue.length && !s.busy) recent.push(row)
    else if (s.status === "running" || s.status === "starting" || s.status === "error" || s.busy || s.room || row.queue.length) current.push(row)
    else recent.push(row)
  }
  const rank = (r: DeviceWork) => r.session.status === "error" ? 0 : r.session.room ? 4 : r.session.status === "running" ? 1 : r.session.status === "starting" ? 2 : r.session.busy ? 3 : 5
  const updated = (a: DeviceWork, b: DeviceWork) => b.session.updated - a.session.updated || a.session.id.localeCompare(b.session.id)
  attention.sort(updated); current.sort((a, b) => rank(a) - rank(b) || updated(a, b)); recent.sort(updated)
  const burden = [...attention, ...current]
  return { attention, current, recent: recent.slice(0, 5), recentTotal: recent.length,
    running: current.filter(r => r.session.status === "running" && !r.session.room).length,
    starting: current.filter(r => r.session.status === "starting" && !r.session.room).length,
    waiting: current.filter(r => !!r.session.room).length,
    queueMessages: burden.reduce((n, r) => n + r.queue.length, 0), queueSessions: burden.filter(r => r.queue.length).length }
}
