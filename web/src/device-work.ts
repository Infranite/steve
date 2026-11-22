import type { SessionView } from "./gen/SessionView.ts"
import type { DecisionView } from "./gen/DecisionView.ts"
export type DeviceWork = { session: SessionView; decisions: DecisionView[]; queued: number }
/** Exact inventory identity only; missing device attribution never falls back to the local Node. */
export function groupDeviceWork(device: string, sessions: SessionView[], decisions: DecisionView[], closedItems: Set<string> = new Set()) {
  const unique = [...new Map(sessions.map(s => [s.id, s])).values()]
  const open = [...new Map(decisions.filter(d => d.status === "open").map(d => [d.id, d])).values()]
  const rows = unique.filter(s => s.device_id === device).map(session => ({ session,
    decisions: open.filter(d => d.session_id === session.id),
    queued: session.queued,
  }))
  const attention: DeviceWork[] = [], current: DeviceWork[] = [], recent: DeviceWork[] = []
  for (const row of rows) {
    const s = row.session
    if (s.readonly) recent.push(row)
    else if (row.decisions.length) attention.push(row)
    else if (s.status === "error" && s.item_id && closedItems.has(s.item_id) && !row.queued && !s.busy) recent.push(row)
    else if (s.status === "running" || s.status === "starting" || s.status === "error" || s.busy || s.waiting || row.queued) current.push(row)
    else recent.push(row)
  }
  const rank = (r: DeviceWork) => r.session.status === "error" ? 0 : r.session.waiting ? 4 : r.session.status === "running" ? 1 : r.session.status === "starting" ? 2 : r.session.busy ? 3 : 5
  const updated = (a: DeviceWork, b: DeviceWork) => b.session.updated - a.session.updated || a.session.id.localeCompare(b.session.id)
  attention.sort(updated); current.sort((a, b) => rank(a) - rank(b) || updated(a, b)); recent.sort(updated)
  const burden = [...attention, ...current]
  return { attention, current, recent: recent.slice(0, 5), recentTotal: recent.length,
    running: current.filter(r => r.session.status === "running" && !r.session.waiting).length,
    starting: current.filter(r => r.session.status === "starting" && !r.session.waiting).length,
    waiting: current.filter(r => !!r.session.waiting).length,
    queueMessages: burden.reduce((n, r) => n + r.queued, 0), queueSessions: burden.filter(r => r.queued).length }
}
