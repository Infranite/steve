/* The page's views (data/table.ts) kept in TanStack Query: loaded from `/api/snapshot`, then changed only by the `patch`
 * frames of the event stream (and reads by id). The stream continues from the snapshot's cursor, so no patch after it is
 * missed; a reset loads the snapshot again. Settled decisions are a page of their own (`['page', …]`), never mixed into
 * the table: a snapshot drops the hot views it no longer holds, and a page is not hot. */
import { QueryClient } from "@tanstack/react-query"
import { useSyncExternalStore } from "react"
import { api, revOf } from "@/api"
import type { DecisionView, Envelope, PageOf, Patch, ViewKind } from "@/gen"
import { EMPTY, mergePatch, mergeSnapshot, mergeViews, viewOf, viewsOf, type Table } from "./table"

export const qc = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, gcTime: Infinity, retry: false, refetchOnWindowFocus: false } } })

/** the kinds this page reads as views */
const KINDS: ViewKind[] = ["decision"]
/** how far back 已处理 goes */
const SETTLED_DAYS = 7

/* a workspace switch starts a new scope: nothing of the old one is merged into it */
let scope = 0
const viewsKey = () => ["views", scope] as const
const settledKey = () => ["page", scope, "decision", "settled"] as const

export const table = (): Table => qc.getQueryData<Table>(viewsKey()) ?? EMPTY
const history = () => qc.getQueryData<PageOf<DecisionView>>(settledKey())
const noDecisions: DecisionView[] = []
export const hasMoreSettled = () => !!history()?.next

let displayed: { table: Table; page: ReturnType<typeof history>; rows: DecisionView[] } | undefined
export function settled(): DecisionView[] {
  const t = table(), page = history()
  if (displayed && displayed.table === t && displayed.page === page) return displayed.rows
  const rows = new Map((page?.views ?? []).map((v) => [v.id, v]))
  for (const [id] of rows) {
    if ((t.tomb.get(`decision:${id}`) ?? 0) > (page?.rev ?? 0)) rows.delete(id)
  }
  const since = Date.now() - SETTLED_DAYS * 86_400_000
  for (const { view, rev } of t.rows.values()) {
    if (rev < (page?.rev ?? 0) && rows.has(view.id)) continue
    if (view.status === "open") rows.delete(view.id)
    else if ((view.resolved ?? 0) > since) rows.set(view.id, view)
  }
  const sorted = [...rows.values()].sort((a, b) => (b.resolved ?? 0) - (a.resolved ?? 0) || b.id.localeCompare(a.id))
  displayed = { table: t, page, rows: sorted }
  return sorted
}

/* One notification per merge, with selectors reading the updated Query cache. */
const listeners = new Set<() => void>()
export const onViews = (f: () => void) => { listeners.add(f); return () => { listeners.delete(f) } }
const changed = () => listeners.forEach((f) => f())

function set(f: (t: Table) => Table) {
  const t0 = table(), t1 = f(t0)
  if (t1 === t0) return
  qc.setQueryData(viewsKey(), t1)
  changed()
}

/* the snapshot's ETag: a reload after a reset that finds nothing changed costs a 304 */
let etag: string | undefined
/** the stream's cursor of the last snapshot read */
let cursor: string | null = null
export const viewsCursor = () => cursor

/** Loads the hot set first. The settled page can arrive independently. */
export async function loadViews(): Promise<void> {
  const s = scope
  const snap = await api.snapshot(KINDS.join(","), etag)
  if (s !== scope) return
  if (snap) {
    etag = snap.etag; cursor = snap.snapshot.cursor
    set((t) => mergeSnapshot(t, snap.snapshot.rev, snap.snapshot.views))
  }
  if (!history()) void loadSettled().catch(() => {})
}

export function resetViews() {
  qc.removeQueries({ queryKey: viewsKey() })
  qc.removeQueries({ queryKey: settledKey() })
  scope++; etag = undefined; cursor = null; changed()
}

async function loadSettled(before?: string) {
  const s = scope, since = Date.now() - SETTLED_DAYS * 86_400_000
  const page = await api.settledDecisions(since, 100, before)
  if (s !== scope) return
  const prior = history()
  if (before && prior?.next !== before) return
  qc.setQueryData(settledKey(), before && prior
    ? { ...page, rev: Math.min(prior.rev, page.rev), views: [...prior.views, ...page.views] }
    : page)
  changed()
}

export const loadMoreSettled = () => {
  const before = history()?.next
  return before ? loadSettled(before) : Promise.resolve()
}

/* merging a frame's ops, timed for `steve_web_patch_apply_seconds` */
const applied: number[] = []
export const takePatchTimes = () => applied.splice(0)

export function applyPatch(frame: Envelope<Patch>) {
  const t0 = performance.now()
  set((t) => mergePatch(t, frame.rev, frame.ops))
  if (applied.length < 500) applied.push(performance.now() - t0)

}

/** After a command: until the views hold its revision (from its answer's `x-steve-rev`), then the answer. A patch
 * normally brings it within a frame; after a second, the views it changed are read by id instead. */
export async function settle<T>(answer: T, kind: ViewKind, ids: string[]): Promise<T> {
  const rev = revOf(answer)
  const ready = () => ids.every((id) =>
    Math.max(table().rows.get(`${kind}:${id}`)?.rev ?? 0, table().tomb.get(`${kind}:${id}`) ?? 0) >= rev)
  if (!rev || ready()) return answer
  const s = scope
  const caught = await new Promise<boolean>((done) => {
    const timer = setTimeout(() => { off(); done(false) }, 1000)
    const off = onViews(() => { if (s !== scope || ready()) { clearTimeout(timer); off(); done(true) } })
  })
  if (!caught && s === scope && ids.length) {
    const r = await api.views(kind, ids).catch(() => null)
    if (r && s === scope) set((t) => mergeViews(t, r.rev, kind, r.views, r.missing))
  }
  return answer
}

/* ── selectors ── */
export const decisionView = (id: string) => viewOf<DecisionView>(table(), "decision", id)
type Open = { all: DecisionView[]; item: ReadonlyMap<string, DecisionView[]>; session: ReadonlyMap<string, DecisionView[]> }
const openLists = new WeakMap<Table, Open>()
function openIndex(): Open {
  const t = table()
  const cached = openLists.get(t)
  if (cached) return cached
  const all = viewsOf<DecisionView>(t, "decision").filter((d) => d.status === "open")
    .sort((a, b) => a.created - b.created || a.id.localeCompare(b.id))
  const item = new Map<string, DecisionView[]>(), session = new Map<string, DecisionView[]>()
  for (const d of all) {
    if (d.item_id) append(item, d.item_id, d)
    if (d.session_id) append(session, d.session_id, d)
  }
  const index = { all, item, session }
  openLists.set(t, index)
  return index
}
function append(index: Map<string, DecisionView[]>, id: string, d: DecisionView) {
  const group = index.get(id)
  if (group) group.push(d)
  else index.set(id, [d])
}
export const openDecisions = () => openIndex().all
export const decisionsForItem = (id: string) => openIndex().item.get(id) ?? noDecisions
export const decisionsForSession = (id: string) => openIndex().session.get(id) ?? noDecisions

/** The inbox list subscribes once. Unchanged row views keep their identity across patches. */
export const useOpenDecisions = () => useSyncExternalStore(onViews, openDecisions)
