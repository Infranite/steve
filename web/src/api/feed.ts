/* The personal reading API: workspace reports and explicitly opted-in completion notifications.
 * Capture the Hub and workspace once; never let a mounted reader follow a global scope switch. */
import { modReq, qs } from "@/api"
import { currentHub } from "@/hub"

export type FeedKind = "report" | "completion"
export type FeedSource = "flow" | "notification"
export type FeedContent = { title: string; markdown: string; fields: [string, string][]; actions: [string, string][]; references: [string, string][] }
export type FeedEntry = {
  id: string; seq: number; kind: FeedKind; title: string; excerpt: string; body: FeedContent | null
  published_at: number; read_at: number | null; purged: boolean
  source: { type: FeedSource; id: string; run_id: string | null; node_id: string | null; event_id: string | null }
}
export type FeedFilter = { kind?: FeedKind; unread?: boolean; source?: FeedSource }
export type FeedPage = { entries: FeedEntry[]; next_cursor: string | null; snapshot: string; unread_count: number }
export type FeedRead = { ids: string[] } | (FeedFilter & { snapshot: string })
export const feedApi = {
  forWorkspace(workspace: string) {
    const scope = { workspace, hub: currentHub() }
    const req = <T>(method: string, path: string, body?: unknown, signal?: AbortSignal) => modReq<T>(method, path, body, signal ? AbortSignal.any([signal, AbortSignal.timeout(30_000)]) : AbortSignal.timeout(30_000), scope)
    return {
      list: (filter: FeedFilter, cursor?: string, limit = 50, signal?: AbortSignal) => req<FeedPage>("GET", "/me/feed" + qs({ ...filter, cursor, limit }), undefined, signal),
      detail: (id: string, signal?: AbortSignal) => req<FeedEntry>("GET", `/me/feed/${encodeURIComponent(id)}`, undefined, signal),
      read: (body: FeedRead, signal?: AbortSignal) => req<{ updated: number }>("POST", "/me/feed/read", body, signal),
    }
  },
}
export type FeedClient = ReturnType<typeof feedApi.forWorkspace>

/** Content links are navigation only, never commands. Local links stay within the client. */
export function feedHref(value: string): string | null {
  if (value !== value.trim() || /[\u0000- \\]/.test(value)) return null
  if (/^#\//.test(value) || /^\/(?!\/)/.test(value)) return value
  try { const url = new URL(value); return url.protocol === "https:" || url.protocol === "http:" ? url.href : null } catch { return null }
}

/** Preserve pagination order if a server page repeats a boundary row. */
export function appendFeed(current: FeedEntry[], incoming: FeedEntry[]): FeedEntry[] {
  const ids = new Set(current.map((entry) => entry.id))
  return [...current, ...incoming.filter((entry) => !ids.has(entry.id) && !!ids.add(entry.id))]
}
