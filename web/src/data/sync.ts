/* The page's views (data/table.ts) kept in TanStack Query: loaded from `/api/snapshot`, then changed only by the `patch`
 * frames of the event stream (and reads by id). The stream continues from the snapshot's cursor, so no patch after it is
 * missed; a reset loads the snapshot again. Settled decisions are a page of their own (`['page', …]`), never mixed into
 * the table: a snapshot drops the hot views it no longer holds, and a page is not hot. */
import { QueryClient } from "@tanstack/react-query"
import { useSyncExternalStore } from "react"
import { api, revOf } from "@/api"
import type { DecisionView, Envelope, Patch, View, ViewKind } from "@/gen"
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
export const settled = (): DecisionView[] => qc.getQueryData<DecisionView[]>(settledKey()) ?? []

/* the store's view model is rebuilt from these (until it reads them through hooks) */
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

/** (Re)loads the snapshot and the settled page; resolves once both are in. */
export async function loadViews(): Promise<void> {
  const s = scope
  const [snap] = await Promise.all([api.snapshot(KINDS.join(","), etag), loadSettled()])
  if (s !== scope || !snap) return
  etag = snap.etag; cursor = snap.snapshot.cursor
  set((t) => mergeSnapshot(t, snap.snapshot.rev, snap.snapshot.views))
}

export function resetViews() { scope++; etag = undefined; cursor = null; changed() }

let settledTimer: ReturnType<typeof setTimeout> | null = null
async function loadSettled() {
  const s = scope
  const page = await api.settledDecisions(Date.now() - SETTLED_DAYS * 86_400_000)
  if (s !== scope) return
  qc.setQueryData(settledKey(), page.views)
  changed()
}

/* merging a frame's ops, timed for `steve_web_patch_apply_seconds` */
const applied: number[] = []
export const takePatchTimes = () => applied.splice(0)

export function applyPatch(frame: Envelope<Patch>) {
  const t0 = performance.now()
  set((t) => mergePatch(t, frame.rev, frame.ops))
  if (applied.length < 500) applied.push(performance.now() - t0)
  // a decision answered or taken back changes the settled page too
  if (frame.ops.some((o) => o.type === "decision")) {
    if (settledTimer) clearTimeout(settledTimer)
    settledTimer = setTimeout(() => { settledTimer = null; void loadSettled().catch(() => {}) }, 300)
  }
}

/** After a command: until the views hold its revision (from its answer's `x-steve-rev`), then the answer. A patch
 * normally brings it within a frame; after a second, the views it changed are read by id instead. */
export async function settle<T>(answer: T, kind: ViewKind, ids: string[]): Promise<T> {
  const rev = revOf(answer)
  if (!rev || table().rev >= rev) return answer
  const s = scope
  const caught = await new Promise<boolean>((done) => {
    const timer = setTimeout(() => { off(); done(false) }, 1000)
    const off = onViews(() => { if (table().rev >= rev) { clearTimeout(timer); off(); done(true) } })
  })
  if (!caught && s === scope && ids.length) {
    const r = await api.views(kind, ids).catch(() => null)
    if (r && s === scope) set((t) => mergeViews(t, r.rev, kind, r.views, r.missing))
  }
  return answer
}

/* ── selectors ── */
export const decisionView = (id: string) => viewOf<DecisionView>(table(), "decision", id)
const openLists = new WeakMap<Table, DecisionView[]>()
/** the open decisions (待处理), oldest first */
export function openDecisions(): DecisionView[] {
  const t = table()
  let xs = openLists.get(t)
  if (!xs) {
    xs = viewsOf<DecisionView>(t, "decision").filter((d) => d.status === "open").sort((a, b) => a.created - b.created || a.id.localeCompare(b.id))
    openLists.set(t, xs)
  }
  return xs
}
/* Components subscribe through one listener set, not a QueryObserver each: with 500 rows on screen a frame costs
 * 0.5 ms instead of ~10 ms (an observer recomputes its whole result per change). Same data, same cache entry. */
/** the table; re-renders when any view changes (a page of cards) */
export const useViews = (): Table => useSyncExternalStore(onViews, table)
/** one view; re-renders only when that view changed (a row) */
export const useView = <V extends View>(kind: ViewKind, id: string): V | undefined => useSyncExternalStore(onViews, () => viewOf<V>(table(), kind, id))
